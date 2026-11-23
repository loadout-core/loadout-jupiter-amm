//! A LiteSVM harness over the committed snapshots: runs `loadout-core`'s real `buy`/`sell` and
//! measures every balance a trade moves, so the parity tests can compare all of it with the
//! adapter's quote (Jupiter's kit compares the output token delta only, and can't see the native
//! SOL a sell pays out).

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::Path;

use jupiter_amm_interface::{
    AccountProvider, Amm, AmmContext, ClockRef, KeyedAccount, QuoteParams, SwapMode, SwapParams,
};
use litesvm::LiteSVM;
use loadout_jupiter_amm::{
    LOADOUT_PROGRAM_ID, LoadoutCurve, NATIVE_MINT, encode_instruction, protocol_fee_shard,
    trader_accounts,
};
use solana_account::{Account, ReadableAccount};
use solana_clock::Clock;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::{Pubkey, pubkey};
use solana_signer::Signer;
use solana_transaction::Transaction;

const ATA_PROGRAM_ID: Pubkey = pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
const CLOCK_SYSVAR: Pubkey = pubkey!("SysvarC1ock11111111111111111111111111111111");
/// Sizes of the two accounts a trader's first trade creates (the trader pays their rent).
const TRADER_REBATE_ACCOUNT_LEN: usize = 57;
const REFERRAL_BINDING_LEN: usize = 113;

/// The committed fixtures of one snapshotted curve.
#[derive(Clone)]
pub struct Snapshot {
    pub curve: Pubkey,
    pub accounts: HashMap<Pubkey, Account>,
    pub program_path: &'static str,
}

impl Snapshot {
    pub fn load(curve: &str) -> Self {
        let curve: Pubkey = curve.parse().expect("curve address");
        let dir = Path::new("tests/fixtures/accounts").join(curve.to_string());
        let mut accounts = HashMap::new();
        for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{dir:?}: {e}")) {
            let path = entry.expect("entry").path();
            let key: Pubkey = path.file_stem().unwrap().to_str().unwrap().parse().unwrap();
            let file = std::fs::File::open(&path).expect("fixture");
            accounts.insert(key, bincode::deserialize_from(file).expect("account"));
        }
        Self {
            curve,
            accounts,
            program_path: "tests/fixtures/loadout.so",
        }
    }

    pub fn clock(&self) -> Clock {
        bincode::deserialize(&self.accounts[&CLOCK_SYSVAR].data).expect("clock")
    }

    /// A copy with one byte of `key`'s data changed: for states no snapshot captured (a paused
    /// project, ...). Used for refusals only.
    pub fn with_byte(&self, key: &Pubkey, offset: usize, value: u8) -> Self {
        let mut copy = self.clone();
        copy.accounts
            .get_mut(key)
            .expect("account in snapshot")
            .data[offset] = value;
        copy
    }
}

struct Provider<'a>(&'a HashMap<Pubkey, Account>);

impl<'a> AccountProvider for Provider<'a> {
    fn get(&self, pubkey: &Pubkey) -> Option<impl ReadableAccount + use<'_, 'a>> {
        self.0.get(pubkey).cloned()
    }
}

/// The adapter built and updated from a snapshot, as Jupiter would, at the snapshot's clock
/// unless `at` gives another unix time.
pub fn amm_at(snapshot: &Snapshot, at: Option<i64>) -> (LoadoutCurve, ClockRef) {
    let mut clock = snapshot.clock();
    if let Some(t) = at {
        clock.unix_timestamp = t;
    }
    let clock_ref = ClockRef::from(clock);
    let keyed = KeyedAccount {
        key: snapshot.curve,
        account: snapshot.accounts[&snapshot.curve].clone(),
        params: None,
    };
    let mut amm = LoadoutCurve::from_keyed_account(
        &keyed,
        &AmmContext {
            clock_ref: clock_ref.clone(),
        },
    )
    .expect("from_keyed_account");
    amm.update(Provider(&snapshot.accounts)).expect("update");
    (amm, clock_ref)
}

/// What one executed trade moved.
#[derive(Debug, PartialEq, Eq)]
pub struct Executed {
    /// Lamports that left (buy) or reached (sell) the trader, net of the first-trade rent.
    pub sol: u64,
    /// Tokens that reached (buy) or left (sell) the trader's token account.
    pub tokens: u64,
    pub treasury_fee: u64,
    pub protocol_fee: u64,
}

/// Full accounting around a swap. Fees and ATA setup are paid by the separate test payer.
#[derive(Debug)]
pub struct ExecutionReport {
    pub swap: Executed,
    /// Signed native SOL change, including trader-account rent, excluding separate payer costs.
    pub wallet_delta: i128,
    pub rent: u64,
    pub transaction_fee: u64,
    /// One-time session setup, paid once by the separate payer, not per-trade costs.
    pub ata_rent: u64,
    pub ata_transaction_fee: u64,
}

fn balance(svm: &LiteSVM, key: &Pubkey) -> u64 {
    svm.get_account(key).map_or(0, |a| a.lamports)
}

/// Reuses the actual program-created trader accounts and updated curve across trades.
pub struct Session {
    svm: LiteSVM,
    payer: Keypair,
    trader: Keypair,
    token_account: Pubkey,
    original: Snapshot,
    ata_rent: u64,
    ata_transaction_fee: u64,
}

impl Session {
    pub fn new(snapshot: &Snapshot, at: Option<i64>) -> Self {
        let mut svm = LiteSVM::new();
        let program = std::fs::read(snapshot.program_path).expect("program fixture");
        svm.add_program(LOADOUT_PROGRAM_ID, &program)
            .expect("add program");
        for (key, account) in &snapshot.accounts {
            if !account.executable {
                svm.set_account(*key, account.clone()).expect("set account");
            }
        }
        let mut clock = snapshot.clock();
        if let Some(t) = at {
            clock.unix_timestamp = t;
        }
        svm.set_sysvar::<Clock>(&clock);

        let (amm, _) = amm_at(snapshot, at);
        let accounts = *amm.accounts();
        let mint = accounts.token_mint;

        // A separate fee payer, so the trader's balance moves by the trade (and its rent) only.
        let payer = Keypair::new();
        let trader = Keypair::new();
        svm.airdrop(&payer.pubkey(), 10_000_000_000)
            .expect("airdrop payer");
        svm.airdrop(&trader.pubkey(), 2_000_000_000_000)
            .expect("airdrop trader");
        let payer_before = balance(&svm, &payer.pubkey());
        let token_account = create_token_account(&mut svm, &payer, &trader.pubkey(), &mint);
        let ata_rent = balance(&svm, &token_account);
        let ata_transaction_fee = payer_before - balance(&svm, &payer.pubkey()) - ata_rent;
        Self {
            svm,
            payer,
            trader,
            token_account,
            original: snapshot.clone(),
            ata_rent,
            ata_transaction_fee,
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let mut snapshot = self.original.clone();
        for (key, account) in &mut snapshot.accounts {
            if let Some(current) = self.svm.get_account(key) {
                *account = current;
            }
        }
        snapshot
    }

    pub fn trade(
        &mut self,
        input_mint: Pubkey,
        output_mint: Pubkey,
        amount: u64,
        min_out: u64,
    ) -> Result<ExecutionReport, String> {
        let snapshot = self.snapshot();
        let (amm, _) = amm_at(&snapshot, None);
        let accounts = *amm.accounts();
        let buy = input_mint == NATIVE_MINT;
        let trader = &self.trader;
        let payer = &self.payer;
        let token_account = self.token_account;
        let svm = &mut self.svm;
        let swap = amm
            .get_swap_and_account_metas(&SwapParams {
                swap_mode: SwapMode::ExactIn,
                in_amount: amount,
                out_amount: min_out,
                source_mint: input_mint,
                destination_mint: output_mint,
                source_token_account: if buy {
                    Pubkey::default()
                } else {
                    token_account
                },
                destination_token_account: if buy {
                    token_account
                } else {
                    Pubkey::default()
                },
                token_transfer_authority: trader.pubkey(),
                user: trader.pubkey(),
                payer: trader.pubkey(),
                quote_mint_to_referrer: None,
                jupiter_program_id: &Pubkey::new_unique(),
                missing_dynamic_accounts_as_default: false,
            })
            .expect("swap metas");
        let ix = Instruction {
            program_id: LOADOUT_PROGRAM_ID,
            accounts: swap.account_metas,
            data: encode_instruction(&swap.swap, amount, min_out).expect("encode"),
        };

        let fee_shard = protocol_fee_shard(&trader.pubkey());
        let (rebate, binding) = trader_accounts(&trader.pubkey());
        let tokens = |svm: &LiteSVM| {
            let data = svm.get_account(&token_account).expect("token account").data;
            u64::from_le_bytes(data[64..72].try_into().unwrap())
        };
        let before_wallet = balance(svm, &trader.pubkey());
        let before_tokens = tokens(svm);
        let before_treasury = balance(svm, &accounts.project_treasury);
        let before_protocol = balance(svm, &fee_shard);
        let before_rent = balance(svm, &rebate) + balance(svm, &binding);
        let before_payer = balance(svm, &payer.pubkey());
        let tracked: Vec<_> = ix
            .accounts
            .iter()
            .filter(|meta| meta.is_writable)
            .map(|meta| (meta.pubkey, svm.get_account(&meta.pubkey)))
            .collect();
        let tx = Transaction::new_signed_with_payer(
            &[
                ComputeBudgetInstruction::set_compute_unit_limit(400_000),
                ix,
            ],
            Some(&payer.pubkey()),
            &[payer, trader],
            svm.latest_blockhash(),
        );
        // A different blockhash also makes identical consecutive trades distinct transactions.
        let result = svm.send_transaction(tx);
        svm.expire_blockhash();
        let transaction_fee = before_payer - balance(svm, &payer.pubkey());
        if let Err(failure) = result {
            // Failed swaps may charge a transaction fee, but every instruction-side write rolls back.
            for (key, before) in tracked {
                assert_eq!(svm.get_account(&key), before, "failed swap changed {key}");
            }
            return Err(format!("{:?}", failure.err));
        }
        let wallet_delta = i128::from(balance(svm, &trader.pubkey())) - i128::from(before_wallet);
        let rent = balance(svm, &rebate) + balance(svm, &binding) - before_rent;
        let expected_rent = if self.original.program_path == "tests/fixtures/next.so" {
            0
        } else {
            [
                (rebate, TRADER_REBATE_ACCOUNT_LEN),
                (binding, REFERRAL_BINDING_LEN),
            ]
            .into_iter()
            .filter(|(key, _)| {
                tracked
                    .iter()
                    .any(|(k, a)| k == key && a.as_ref().is_none_or(|a| a.lamports == 0))
            })
            .map(|(_, len)| svm.minimum_balance_for_rent_exemption(len))
            .sum::<u64>()
        };
        assert_eq!(
            rent, expected_rent,
            "observed account funding must equal expected rent"
        );
        let swap_sol = if buy {
            -wallet_delta - i128::from(rent)
        } else {
            wallet_delta + i128::from(rent)
        };
        Ok(ExecutionReport {
            swap: Executed {
                sol: u64::try_from(swap_sol).expect("nonnegative swap SOL"),
                tokens: before_tokens.abs_diff(tokens(svm)),
                treasury_fee: balance(svm, &accounts.project_treasury) - before_treasury,
                protocol_fee: balance(svm, &fee_shard) - before_protocol,
            },
            wallet_delta,
            rent,
            transaction_fee,
            ata_rent: self.ata_rent,
            ata_transaction_fee: self.ata_transaction_fee,
        })
    }

    /// Synthetic token funding for isolated sell tests; sequence tests use tokens bought above.
    pub fn fund_tokens(&mut self, amount: u64) {
        let mut account = self
            .svm
            .get_account(&self.token_account)
            .expect("token account");
        account.data[64..72].copy_from_slice(&amount.to_le_bytes());
        self.svm
            .set_account(self.token_account, account)
            .expect("fund token account");
    }
}

pub fn execute(
    snapshot: &Snapshot,
    input_mint: Pubkey,
    output_mint: Pubkey,
    amount: u64,
    at: Option<i64>,
) -> Result<Executed, String> {
    let mut session = Session::new(snapshot, at);
    if input_mint != NATIVE_MINT {
        session.fund_tokens(amount);
    }
    session
        .trade(input_mint, output_mint, amount, 0)
        .map(|report| report.swap)
}

/// The trader's associated token account for `mint`, created through the ATA program.
pub fn create_token_account(
    svm: &mut LiteSVM,
    payer: &Keypair,
    owner: &Pubkey,
    mint: &Pubkey,
) -> Pubkey {
    let token_program = svm.get_account(mint).expect("mint").owner;
    let (ata, _) = Pubkey::find_program_address(
        &[owner.as_ref(), token_program.as_ref(), mint.as_ref()],
        &ATA_PROGRAM_ID,
    );
    let ix = Instruction {
        program_id: ATA_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(ata, false),
            AccountMeta::new_readonly(*owner, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(pubkey!("11111111111111111111111111111111"), false),
            AccountMeta::new_readonly(token_program, false),
        ],
        data: vec![0],
    };
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[payer],
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx).expect("create token account");
    ata
}

/// The adapter's quote for the same trade.
pub fn quote(
    snapshot: &Snapshot,
    input_mint: Pubkey,
    output_mint: Pubkey,
    amount: u64,
    at: Option<i64>,
) -> Result<jupiter_amm_interface::Quote, String> {
    let (amm, _) = amm_at(snapshot, at);
    amm.quote(&QuoteParams {
        amount,
        input_mint,
        output_mint,
        swap_mode: SwapMode::ExactIn,
        flow_type: Default::default(),
    })
    .map_err(|e| e.to_string())
}
