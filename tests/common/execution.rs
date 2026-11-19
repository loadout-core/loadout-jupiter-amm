//! Test-only composition lab. The native relay exercises Agave CPI privilege checks and
//! the dumped mainnet core SBF binary, not Jupiter's program, wrapping code or routing engine.
#![allow(dead_code)]
use super::common::{Snapshot, amm_at, create_token_account};
use jupiter_amm_interface::{Amm, SwapMode, SwapParams};
use litesvm::LiteSVM;
use loadout_jupiter_amm::{
    LOADOUT_PROGRAM_ID, NATIVE_MINT, TOKEN_PROGRAM_ID, encode_instruction, trader_accounts,
};
use solana_clock::Clock;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::{AccountMeta, Instruction};
use solana_instruction_error::InstructionError;
use solana_keypair::Keypair;
use solana_program_runtime::{
    declare_process_instruction, solana_sbpf::program::BuiltinFunctionDefinition,
};
use solana_pubkey::{Pubkey, pubkey};
use solana_signer::Signer;
use solana_system_interface::instruction as system;
use solana_transaction::Transaction;
use std::collections::BTreeMap;

pub const PROBE_ID: Pubkey = pubkey!("CpiProbe111111111111111111111111111111111111");

// The instruction's account order/privileges are the pinned core ABI, not the outer flags.
// Reconstructing them independently lets negative tests detect CPI privilege escalation.
declare_process_instruction!(Relay, 1_000, |context| {
    let current = context
        .transaction_context
        .get_current_instruction_context()?;
    let data = current.get_instruction_data();
    if data.len() != 25 {
        return Err(InstructionError::InvalidInstructionData);
    }
    let sign_pda = data[0] == 1;
    let payload = data[1..].to_vec();
    let mut accounts = Vec::with_capacity(15);
    for index in 0..15 {
        accounts.push(AccountMeta {
            pubkey: *current.get_key_of_instruction_account(index)?,
            is_signer: index == 12,
            is_writable: matches!(index, 1 | 2 | 3 | 5 | 6 | 9 | 10 | 11 | 12),
        });
    }
    let inner = Instruction {
        program_id: LOADOUT_PROGRAM_ID,
        accounts,
        data: payload,
    };
    if sign_pda {
        let (_, bump) = Pubkey::find_program_address(&[b"authority"], &PROBE_ID);
        context.native_invoke_signed(inner, &[&[b"authority", &[bump]]])
    } else {
        context.native_invoke_signed(inner, &[])
    }
});

pub struct Lab {
    pub svm: LiteSVM,
    pub payer: Keypair,
    pub user: Keypair,
    pub wallet_authority: Keypair,
    pub authority: Pubkey,
    pub user_token: Pubkey,
    pub authority_token: Pubkey,
    initial: Snapshot,
}

#[derive(Debug)]
pub struct Sent {
    pub error: Option<String>,
    pub logs: Vec<String>,
    pub transaction_fee: u64,
    pub payer_delta: i128,
}

impl Lab {
    pub fn new(snapshot: Snapshot, pda: bool) -> Self {
        let mut svm = LiteSVM::new();
        svm.add_program(
            LOADOUT_PROGRAM_ID,
            &std::fs::read(snapshot.program_path).unwrap(),
        )
        .unwrap();
        svm.add_builtin(PROBE_ID, Relay::register);
        for (key, account) in &snapshot.accounts {
            if !account.executable {
                svm.set_account(*key, account.clone()).unwrap();
            }
        }
        svm.set_sysvar::<Clock>(&snapshot.clock());
        let payer = Keypair::new();
        let user = Keypair::new();
        let wallet_authority = Keypair::new();
        let authority = if pda {
            Pubkey::find_program_address(&[b"authority"], &PROBE_ID).0
        } else {
            wallet_authority.pubkey()
        };
        for key in [payer.pubkey(), user.pubkey(), authority] {
            svm.airdrop(&key, 2_000_000_000_000).unwrap();
        }
        let mint = amm_at(&snapshot, None).0.accounts().token_mint;
        let user_token = create_token_account(&mut svm, &payer, &user.pubkey(), &mint);
        let authority_token = create_token_account(&mut svm, &payer, &authority, &mint);
        Self {
            svm,
            payer,
            user,
            wallet_authority,
            authority,
            user_token,
            authority_token,
            initial: snapshot,
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let mut snapshot = self.initial.clone();
        snapshot.accounts.retain(|key, account| {
            if let Some(current) = self.svm.get_account(key) {
                *account = current;
                true
            } else {
                // A zero-lamport account may be purged by the runtime. Keep absence truthful.
                false
            }
        });
        snapshot
    }

    pub fn swap(&self, buy: bool, amount: u64, min_out: u64, token_account: Pubkey) -> Instruction {
        let (amm, _) = amm_at(&self.snapshot(), None);
        let token = amm.accounts().token_mint;
        let swap = amm
            .get_swap_and_account_metas(&SwapParams {
                swap_mode: SwapMode::ExactIn,
                in_amount: amount,
                out_amount: min_out,
                source_mint: if buy { NATIVE_MINT } else { token },
                destination_mint: if buy { token } else { NATIVE_MINT },
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
                token_transfer_authority: self.authority,
                user: self.user.pubkey(),
                payer: self.payer.pubkey(),
                quote_mint_to_referrer: None,
                jupiter_program_id: &PROBE_ID,
                missing_dynamic_accounts_as_default: false,
            })
            .unwrap();
        Instruction {
            program_id: LOADOUT_PROGRAM_ID,
            accounts: swap.account_metas,
            data: encode_instruction(&swap.swap, amount, min_out).unwrap(),
        }
    }

    pub fn relay(&self, native: Instruction, sign_pda: bool) -> Instruction {
        let mut accounts = native.accounts;
        if sign_pda {
            accounts[12].is_signer = false;
        }
        accounts.push(AccountMeta::new_readonly(LOADOUT_PROGRAM_ID, false));
        let mut data = vec![u8::from(sign_pda)];
        data.extend(native.data);
        Instruction {
            program_id: PROBE_ID,
            accounts,
            data,
        }
    }

    /// Every failed transaction must roll back all supplied writable accounts except fee payer.
    pub fn send(&mut self, instructions: &[Instruction]) -> Sent {
        let mut tracked = BTreeMap::new();
        for meta in instructions
            .iter()
            .flat_map(|ix| &ix.accounts)
            .filter(|meta| meta.is_writable)
        {
            if meta.pubkey != self.payer.pubkey() {
                tracked.insert(meta.pubkey, self.svm.get_account(&meta.pubkey));
            }
        }
        let needs = |key| {
            instructions
                .iter()
                .flat_map(|ix| &ix.accounts)
                .any(|a| a.is_signer && a.pubkey == key)
        };
        let mut signers = vec![&self.payer];
        if needs(self.user.pubkey()) {
            signers.push(&self.user);
        }
        if needs(self.wallet_authority.pubkey()) {
            signers.push(&self.wallet_authority);
        }
        let payer_before = self.balance(self.payer.pubkey());
        tracked.insert(
            self.payer.pubkey(),
            self.svm.get_account(&self.payer.pubkey()),
        );
        let total_before: u128 = tracked
            .values()
            .map(|a| u128::from(a.as_ref().map_or(0, |a| a.lamports)))
            .sum();
        let mut ixs = vec![ComputeBudgetInstruction::set_compute_unit_limit(800_000)];
        ixs.extend_from_slice(instructions);
        let tx = Transaction::new_signed_with_payer(
            &ixs,
            Some(&self.payer.pubkey()),
            &signers,
            self.svm.latest_blockhash(),
        );
        // LiteSVM default base fee, no priority-price or precompile instructions in this lab.
        let expected_fee = 5_000 * u64::from(tx.message.header.num_required_signatures);
        let result = self.svm.send_transaction(tx);
        self.svm.expire_blockhash();
        let total_after: u128 = tracked
            .keys()
            .map(|key| u128::from(self.balance(*key)))
            .sum();
        let transaction_fee =
            u64::try_from(total_before - total_after).expect("fee is the only destroyed lamports");
        assert_eq!(
            transaction_fee, expected_fee,
            "conservation across all writable accounts"
        );
        let payer_delta = i128::from(self.balance(self.payer.pubkey())) - i128::from(payer_before);
        match result {
            Ok(meta) => Sent {
                error: None,
                logs: meta.logs,
                transaction_fee,
                payer_delta,
            },
            Err(failure) => {
                for (key, before) in tracked {
                    if key == self.payer.pubkey() {
                        continue;
                    }
                    assert_eq!(self.svm.get_account(&key), before, "rollback of {key}");
                }
                Sent {
                    error: Some(format!("{:?}", failure.err)),
                    logs: failure.meta.logs,
                    transaction_fee,
                    payer_delta,
                }
            }
        }
    }

    pub fn balance(&self, key: Pubkey) -> u64 {
        self.svm.get_account(&key).map_or(0, |a| a.lamports)
    }
    pub fn tokens(&self, key: Pubkey) -> u64 {
        u64::from_le_bytes(
            self.svm.get_account(&key).unwrap().data[64..72]
                .try_into()
                .unwrap(),
        )
    }
    pub fn rent(&self) -> u64 {
        let (a, b) = trader_accounts(&self.authority);
        self.balance(a) + self.balance(b)
    }
    pub fn wsol(&mut self, owner: Pubkey) -> Pubkey {
        create_token_account(&mut self.svm, &self.payer, &owner, &NATIVE_MINT)
    }
    pub fn set_clock(&mut self, now: i64) {
        let mut clock = self.svm.get_sysvar::<Clock>();
        clock.unix_timestamp = now;
        self.svm.set_sysvar(&clock);
    }
    pub fn set_authority_data(&mut self) {
        let mut account = self.svm.get_account(&self.authority).unwrap();
        account.data = vec![0; 80];
        self.svm.set_account(self.authority, account).unwrap();
    }
}

pub fn transfer(from: Pubkey, to: Pubkey, amount: u64) -> Instruction {
    system::transfer(&from, &to, amount)
}
pub fn sync(account: Pubkey) -> Instruction {
    Instruction {
        program_id: TOKEN_PROGRAM_ID,
        accounts: vec![AccountMeta::new(account, false)],
        data: vec![17],
    }
}
pub fn close(account: Pubkey, to: Pubkey, authority: Pubkey) -> Instruction {
    Instruction {
        program_id: TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(account, false),
            AccountMeta::new(to, false),
            AccountMeta::new_readonly(authority, true),
        ],
        data: vec![9],
    }
}
pub fn token_transfer(from: Pubkey, to: Pubkey, authority: Pubkey, amount: u64) -> Instruction {
    let mut data = vec![3];
    data.extend_from_slice(&amount.to_le_bytes());
    Instruction {
        program_id: TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(from, false),
            AccountMeta::new(to, false),
            AccountMeta::new_readonly(authority, true),
        ],
        data,
    }
}
pub fn approve(account: Pubkey, delegate: Pubkey, owner: Pubkey, amount: u64) -> Instruction {
    let mut data = vec![4];
    data.extend_from_slice(&amount.to_le_bytes());
    Instruction {
        program_id: TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(account, false),
            AccountMeta::new_readonly(delegate, false),
            AccountMeta::new_readonly(owner, true),
        ],
        data,
    }
}

/// Decode only the pinned SwapEvent, including the identity and referral attribution fields.
pub fn event_trader(sent: &Sent) -> Pubkey {
    use base64::Engine;
    let tag = [64, 198, 205, 232, 38, 8, 113, 226]; // sha256("event:SwapEvent")[..8]
    for log in &sent.logs {
        if let Some(encoded) = log.strip_prefix("Program data: ") {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap();
            if bytes.starts_with(&tag) {
                assert_eq!(
                    &bytes[177..209],
                    &[0u8; 32],
                    "unreferred trade has no referrer"
                );
                assert_eq!(
                    &bytes[209..265],
                    &[0u8; 56],
                    "no referral reward, rebate, L2 referrer or reward"
                );
                return Pubkey::new_from_array(bytes[48..80].try_into().unwrap());
            }
        }
    }
    panic!("missing SwapEvent: {:?}", sent.logs);
}

pub fn create_wsol(owner: Pubkey, payer: Pubkey) -> (Pubkey, Instruction) {
    let ata_program = pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
    let key = Pubkey::find_program_address(
        &[
            owner.as_ref(),
            TOKEN_PROGRAM_ID.as_ref(),
            NATIVE_MINT.as_ref(),
        ],
        &ata_program,
    )
    .0;
    (
        key,
        Instruction {
            program_id: ata_program,
            accounts: vec![
                AccountMeta::new(payer, true),
                AccountMeta::new(key, false),
                AccountMeta::new_readonly(owner, false),
                AccountMeta::new_readonly(NATIVE_MINT, false),
                AccountMeta::new_readonly(loadout_jupiter_amm::SYSTEM_PROGRAM_ID, false),
                AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            ],
            data: vec![0],
        },
    )
}
