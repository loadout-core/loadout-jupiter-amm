//! Real loader and core-instruction execution, entirely inside LiteSVM.
//! Buffer payloads and loader authority are bootstrapped test accounts; upgrades use loader instructions.
mod common;
use jupiter_amm_interface::{AccountProvider, Amm, AmmContext, ClockRef, KeyedAccount};
use litesvm::LiteSVM;
use loadout_jupiter_amm::{
    LOADOUT_PROGRAM_ID, LoadoutCurve, NATIVE_MINT, encode_instruction, runtime,
};
use sha2::{Digest, Sha256};
use solana_account::{Account, ReadableAccount};
use solana_clock::Clock;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_loader_v3_interface::instruction as loader;
use solana_pubkey::{Pubkey, pubkey};
use solana_signer::Signer;
use solana_transaction::Transaction;
const ACTIVE: &str = "8gMCp3MpUN6nJ9N2nwjuefrqK6rp6Mrvpnqn1pjUvmwn";
const LOADER: Pubkey = pubkey!("BPFLoaderUpgradeab1e11111111111111111111111");
struct Lab {
    svm: LiteSVM,
    payer: Keypair,
}
impl Lab {
    fn new(path: &str, authority: &Keypair) -> Self {
        let mut svm = LiteSVM::new();
        let mut clock = svm.get_sysvar::<Clock>();
        clock.slot = 100;
        svm.set_sysvar(&clock);
        svm.add_program(LOADOUT_PROGRAM_ID, &std::fs::read(path).unwrap())
            .unwrap();
        let key = runtime::program_data_address();
        let mut a = svm.get_account(&key).unwrap();
        a.data[12] = 1;
        a.data[13..45].copy_from_slice(authority.pubkey().as_ref());
        svm.set_account(key, a).unwrap();
        let payer = Keypair::new();
        svm.airdrop(&payer.pubkey(), 100_000_000_000).unwrap();
        svm.airdrop(&authority.pubkey(), 100_000_000_000).unwrap();
        svm.warp_to_slot(101);
        Self { svm, payer }
    }
    fn slot(&self) -> u64 {
        u64::from_le_bytes(
            self.svm
                .get_account(&runtime::program_data_address())
                .unwrap()
                .data[4..12]
                .try_into()
                .unwrap(),
        )
    }
    fn hash(&self) -> [u8; 32] {
        Sha256::digest(
            &self
                .svm
                .get_account(&runtime::program_data_address())
                .unwrap()
                .data[45..],
        )
        .into()
    }
    fn publish(&self, authority: Pubkey, slot: u64, hash: [u8; 32]) -> Instruction {
        let mut data = Sha256::digest(b"global:publish_integration_config")[..8].to_vec();
        data.extend_from_slice(&slot.to_le_bytes());
        data.extend_from_slice(&hash);
        Instruction {
            program_id: LOADOUT_PROGRAM_ID,
            data,
            accounts: vec![
                AccountMeta::new_readonly(LOADOUT_PROGRAM_ID, false),
                AccountMeta::new_readonly(runtime::program_data_address(), false),
                AccountMeta::new(authority, true),
                AccountMeta::new(runtime::integration_config_address(), false),
                AccountMeta::new_readonly(pubkey!("11111111111111111111111111111111"), false),
            ],
        }
    }
    fn send(&mut self, ixs: &[Instruction], signers: &[&Keypair]) -> Result<u64, String> {
        let mut all = vec![&self.payer];
        all.extend_from_slice(signers);
        let mut instructions = vec![ComputeBudgetInstruction::set_compute_unit_limit(1_400_000)];
        instructions.extend_from_slice(ixs);
        let tx = Transaction::new_signed_with_payer(
            &instructions,
            Some(&self.payer.pubkey()),
            &all,
            self.svm.latest_blockhash(),
        );
        let result = self.svm.send_transaction(tx);
        self.svm.expire_blockhash();
        result
            .map(|m| m.compute_units_consumed)
            .map_err(|e| format!("{:?}: {:?}", e.err, e.meta.logs))
    }
    fn buffer(&mut self, path: &str, authority: Pubkey) -> Pubkey {
        let program = std::fs::read(path).unwrap();
        let mut data = 1u32.to_le_bytes().to_vec();
        data.push(1);
        data.extend_from_slice(authority.as_ref());
        data.extend_from_slice(&program);
        let key = Pubkey::new_unique();
        self.svm
            .set_account(
                key,
                Account {
                    lamports: self.svm.minimum_balance_for_rent_exemption(data.len()),
                    data,
                    owner: LOADER,
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .unwrap();
        key
    }
    fn seed_market(&mut self) {
        let s = common::Snapshot::load(ACTIVE);
        for (key, a) in s.accounts {
            if !a.executable && key != runtime::program_data_address() {
                self.svm.set_account(key, a).unwrap();
            }
        }
        // Preserve the scenario's unix time, but use a post-deployment bank slot.
        let mut clock = self.svm.get_sysvar::<Clock>();
        clock.slot = 102;
        self.svm.set_sysvar(&clock);
        self.svm.warp_to_slot(102);
    }
    fn amm(&self) -> Result<LoadoutCurve, String> {
        let key: Pubkey = ACTIVE.parse().unwrap();
        let keyed = KeyedAccount {
            key,
            account: self.svm.get_account(&key).unwrap(),
            params: None,
        };
        let mut amm = LoadoutCurve::from_keyed_account(
            &keyed,
            &AmmContext {
                clock_ref: ClockRef::from(self.svm.get_sysvar::<Clock>()),
            },
        )
        .map_err(|e| e.to_string())?;
        amm.update(Bank(&self.svm)).map_err(|e| e.to_string())?;
        Ok(amm)
    }
}
struct Bank<'a>(&'a LiteSVM);
impl<'a> AccountProvider for Bank<'a> {
    fn get(&self, key: &Pubkey) -> Option<impl ReadableAccount + use<'_, 'a>> {
        self.0.get_account(key)
    }
}
fn assert_code(result: Result<u64, String>, code: u32) {
    let error = result.unwrap_err();
    assert!(error.contains(&format!("Custom({code})")), "{error}");
}

#[test]
fn publication_rejects_wrong_signer_slot_hash_and_account_substitution() {
    let authority = Keypair::new();
    let mut lab = Lab::new("tests/fixtures/interface.so", &authority);
    let attacker = Keypair::new();
    lab.svm.airdrop(&attacker.pubkey(), 10_000_000).unwrap();
    let key = runtime::integration_config_address();
    let before = lab.svm.get_account(&key);
    let ix = lab.publish(attacker.pubkey(), lab.slot(), lab.hash());
    assert_code(lab.send(&[ix], &[&attacker]), 12309);
    assert_eq!(lab.svm.get_account(&key), before);
    for slot in [lab.slot() - 1, lab.slot() + 1] {
        let ix = lab.publish(authority.pubkey(), slot, lab.hash());
        assert_code(lab.send(&[ix], &[&authority]), 12308);
        assert_eq!(lab.svm.get_account(&key), before);
    }
    let ix = lab.publish(authority.pubkey(), lab.slot(), [0; 32]);
    assert_code(lab.send(&[ix], &[&authority]), 12307);
    let mut ix = lab.publish(authority.pubkey(), lab.slot(), lab.hash());
    ix.accounts[3].pubkey = Pubkey::new_unique();
    assert_code(lab.send(&[ix], &[&authority]), 2006);
    let fake = Pubkey::new_unique();
    let mut copied = lab
        .svm
        .get_account(&runtime::program_data_address())
        .unwrap();
    copied.owner = pubkey!("11111111111111111111111111111111");
    lab.svm.set_account(fake, copied).unwrap();
    let mut ix = lab.publish(authority.pubkey(), lab.slot(), lab.hash());
    ix.accounts[1].pubkey = fake;
    assert_code(lab.send(&[ix], &[&authority]), 3007);
    let mut copied = lab
        .svm
        .get_account(&runtime::program_data_address())
        .unwrap();
    copied.owner = LOADER;
    lab.svm.set_account(fake, copied).unwrap();
    let mut ix = lab.publish(authority.pubkey(), lab.slot(), lab.hash());
    ix.accounts[1].pubkey = fake;
    assert_code(lab.send(&[ix], &[&authority]), 12307);
    let mut ix = lab.publish(authority.pubkey(), lab.slot(), lab.hash());
    ix.accounts[0].pubkey = pubkey!("11111111111111111111111111111111");
    assert_code(lab.send(&[ix], &[&authority]), 3008);
    let mut ix = lab.publish(authority.pubkey(), lab.slot(), lab.hash());
    ix.accounts[2].is_signer = false;
    assert_code(lab.send(&[ix], &[]), 3010);
}
#[test]
fn publication_is_rent_exact_replay_safe_and_enables_unknown_executable_quotes() {
    let authority = Keypair::new();
    let mut lab = Lab::new("tests/fixtures/interface.so", &authority);
    lab.seed_market();
    assert!(lab.amm().is_err());
    let pd = lab
        .svm
        .get_account(&runtime::program_data_address())
        .unwrap();
    assert!(runtime::Runtime::decode(&pd.data, &pd.owner).is_err());
    let before = lab.svm.get_account(&authority.pubkey()).unwrap().lamports;
    let ix = lab.publish(authority.pubkey(), lab.slot(), lab.hash());
    let cu = lab.send(std::slice::from_ref(&ix), &[&authority]).unwrap();
    let config = lab
        .svm
        .get_account(&runtime::integration_config_address())
        .unwrap();
    assert_eq!(config.data.len(), 84);
    assert_eq!(
        before - lab.svm.get_account(&authority.pubkey()).unwrap().lamports,
        config.lamports
    );
    assert_eq!(
        config.lamports,
        lab.svm.minimum_balance_for_rent_exemption(84)
    );
    let before = lab.svm.get_account(&authority.pubkey()).unwrap().lamports;
    lab.send(&[ix], &[&authority]).unwrap();
    assert_eq!(
        before,
        lab.svm.get_account(&authority.pubkey()).unwrap().lamports
    );
    println!("publish_integration_config initialize CU={cu}");
    let amm = lab.amm().unwrap();
    let market = amm.market().unwrap();
    let fill = loadout_jupiter_amm::quote::buy(
        &market,
        10_000_000,
        lab.svm.get_sysvar::<Clock>().unix_timestamp,
    )
    .unwrap();
    assert!(fill.out_amount > 0);
    // A nonzero mistyped hash is an authority attestation, but the SDK must independently refuse it.
    let ix = lab.publish(authority.pubkey(), lab.slot(), [7; 32]);
    lab.send(&[ix], &[&authority]).unwrap();
    assert!(lab.amm().is_err());
}
#[test]
fn authority_rotation_uses_the_loaders_current_authority() {
    let old = Keypair::new();
    let next = Keypair::new();
    let mut lab = Lab::new("tests/fixtures/interface.so", &old);
    lab.svm.airdrop(&next.pubkey(), 100_000_000).unwrap();
    let ix =
        loader::set_upgrade_authority(&LOADOUT_PROGRAM_ID, &old.pubkey(), Some(&next.pubkey()));
    lab.send(&[ix], &[&old]).unwrap();
    let ix = lab.publish(old.pubkey(), lab.slot(), lab.hash());
    assert_code(lab.send(&[ix], &[&old]), 12309);
    let ix = lab.publish(next.pubkey(), lab.slot(), lab.hash());
    lab.send(&[ix], &[&next]).unwrap();
}
#[test]
fn real_loader_upgrade_rejects_insufficient_capacity_then_requires_fresh_metadata() {
    let authority = Keypair::new();
    let mut lab = Lab::new("tests/fixtures/nominated.so", &authority);
    lab.seed_market();
    assert!(lab.amm().is_ok());
    let buffer = lab.buffer("tests/fixtures/interface.so", authority.pubkey());
    let upgrade = loader::upgrade(
        &LOADOUT_PROGRAM_ID,
        &buffer,
        &authority.pubkey(),
        &lab.payer.pubkey(),
        true,
    );
    let err = lab
        .send(std::slice::from_ref(&upgrade), &[&authority])
        .unwrap_err();
    assert!(err.contains("AccountDataTooSmall"), "{err}");
    lab.send(
        &[loader::extend_program(
            &LOADOUT_PROGRAM_ID,
            Some(&lab.payer.pubkey()),
            65_536,
        )],
        &[],
    )
    .unwrap();
    lab.svm.warp_to_slot(105);
    lab.send(&[upgrade], &[&authority]).unwrap();
    assert_eq!(lab.slot(), 105);
    lab.svm.warp_to_slot(106);
    assert!(lab.amm().is_err());
    let ix = lab.publish(authority.pubkey(), lab.slot(), lab.hash());
    lab.send(&[ix], &[&authority]).unwrap();
    assert!(lab.amm().is_ok());
}
#[test]
fn first_upgrade_cannot_invoke_a_new_instruction_in_the_same_transaction() {
    let authority = Keypair::new();
    let mut lab = Lab::new("tests/fixtures/nominated.so", &authority);
    lab.send(
        &[loader::extend_program(
            &LOADOUT_PROGRAM_ID,
            Some(&lab.payer.pubkey()),
            65_536,
        )],
        &[],
    )
    .unwrap();
    let buffer = lab.buffer("tests/fixtures/interface.so", authority.pubkey());
    lab.svm.warp_to_slot(105);
    let upgrade = loader::upgrade(
        &LOADOUT_PROGRAM_ID,
        &buffer,
        &authority.pubkey(),
        &lab.payer.pubkey(),
        true,
    );
    let mut payload = std::fs::read("tests/fixtures/interface.so").unwrap();
    payload.resize(
        lab.svm
            .get_account(&runtime::program_data_address())
            .unwrap()
            .data
            .len()
            - 45,
        0,
    );
    let publish = lab.publish(authority.pubkey(), 105, Sha256::digest(&payload).into());
    let before_slot = lab.slot();
    let before_hash = lab.hash();
    let error = lab.send(&[upgrade, publish], &[&authority]).unwrap_err();
    // The loader blocks execution in the deployment slot before Anchor dispatch.
    assert!(error.contains("UnsupportedProgramId"), "{error}");
    assert_eq!(
        lab.slot(),
        before_slot,
        "failed transaction must roll the loader upgrade back"
    );
    assert_eq!(
        lab.hash(),
        before_hash,
        "failed transaction must preserve the executable payload"
    );
}

#[test]
fn unchanged_quote_behavior_accepts_new_payload_and_slot_without_an_sdk_release() {
    let authority = Keypair::new();
    let mut lab = Lab::new("tests/fixtures/interface.so", &authority);
    lab.seed_market();
    let ix = lab.publish(authority.pubkey(), lab.slot(), lab.hash());
    lab.send(&[ix], &[&authority]).unwrap();
    assert!(lab.amm().is_ok());
    let original_hash = lab.hash();
    lab.svm.warp_to_slot(105);
    lab.send(
        &[loader::extend_program(
            &LOADOUT_PROGRAM_ID,
            Some(&lab.payer.pubkey()),
            65_536,
        )],
        &[],
    )
    .unwrap();
    assert_ne!(
        lab.hash(),
        original_hash,
        "allocation padding changes the executable payload identity"
    );
    assert!(lab.amm().is_err());
    lab.svm.warp_to_slot(106);
    let ix = lab.publish(authority.pubkey(), lab.slot(), lab.hash());
    lab.send(&[ix], &[&authority]).unwrap();
    assert!(lab.amm().is_ok());
    let buffer = lab.buffer("tests/fixtures/interface.so", authority.pubkey());
    lab.svm.warp_to_slot(110);
    let upgrade = loader::upgrade(
        &LOADOUT_PROGRAM_ID,
        &buffer,
        &authority.pubkey(),
        &lab.payer.pubkey(),
        true,
    );
    lab.send(&[upgrade], &[&authority]).unwrap();
    assert!(lab.amm().is_err());
    lab.svm.warp_to_slot(111);
    let ix = lab.publish(authority.pubkey(), lab.slot(), lab.hash());
    lab.send(&[ix], &[&authority]).unwrap();
    assert!(lab.amm().is_ok());
}
#[test]
fn prefunded_descriptor_and_immutable_program_do_not_grant_an_authority_bypass() {
    let authority = Keypair::new();
    let mut lab = Lab::new("tests/fixtures/interface.so", &authority);
    let config = runtime::integration_config_address();
    let rent = lab.svm.minimum_balance_for_rent_exemption(84);
    lab.svm
        .set_account(
            config,
            Account {
                lamports: rent - 1,
                data: vec![],
                owner: pubkey!("11111111111111111111111111111111"),
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
    let before = lab.svm.get_account(&authority.pubkey()).unwrap().lamports;
    let ix = lab.publish(authority.pubkey(), lab.slot(), lab.hash());
    lab.send(&[ix], &[&authority]).unwrap();
    assert_eq!(
        before - lab.svm.get_account(&authority.pubkey()).unwrap().lamports,
        1
    );
    let ix = loader::set_upgrade_authority(&LOADOUT_PROGRAM_ID, &authority.pubkey(), None);
    lab.send(&[ix], &[&authority]).unwrap();
    let ix = lab.publish(authority.pubkey(), lab.slot(), lab.hash());
    assert_code(lab.send(&[ix], &[&authority]), 12309);
}
#[test]
fn sdk_quotes_execute_after_publication_with_exact_native_sol_and_token_deltas() {
    use jupiter_amm_interface::{SwapMode, SwapParams};
    let authority = Keypair::new();
    let mut lab = Lab::new("tests/fixtures/interface.so", &authority);
    lab.seed_market();
    let ix = lab.publish(authority.pubkey(), lab.slot(), lab.hash());
    lab.send(&[ix], &[&authority]).unwrap();
    let trader = Keypair::new();
    lab.svm.airdrop(&trader.pubkey(), 1_000_000_000).unwrap();
    let mint = lab.amm().unwrap().accounts().token_mint;
    let token = common::create_token_account(&mut lab.svm, &lab.payer, &trader.pubkey(), &mint);
    let mut held = 0;
    for buy in [true, false] {
        let amm = lab.amm().unwrap();
        let market = amm.market().unwrap();
        let now = lab.svm.get_sysvar::<Clock>().unix_timestamp;
        let fill = if buy {
            loadout_jupiter_amm::quote::buy(&market, 10_000_000, now)
        } else {
            loadout_jupiter_amm::quote::sell(&market, held / 2, now)
        }
        .unwrap();
        let swap = amm
            .get_swap_and_account_metas(&SwapParams {
                swap_mode: SwapMode::ExactIn,
                in_amount: fill.in_amount,
                out_amount: fill.out_amount,
                source_mint: if buy { NATIVE_MINT } else { mint },
                destination_mint: if buy { mint } else { NATIVE_MINT },
                source_token_account: if buy { Pubkey::default() } else { token },
                destination_token_account: if buy { token } else { Pubkey::default() },
                token_transfer_authority: trader.pubkey(),
                user: trader.pubkey(),
                payer: lab.payer.pubkey(),
                quote_mint_to_referrer: None,
                jupiter_program_id: &Pubkey::default(),
                missing_dynamic_accounts_as_default: false,
            })
            .unwrap();
        let ix = Instruction {
            program_id: LOADOUT_PROGRAM_ID,
            accounts: swap.account_metas,
            data: encode_instruction(&swap.swap, fill.in_amount, fill.out_amount).unwrap(),
        };
        let balance = lab.svm.get_account(&trader.pubkey()).unwrap().lamports;
        let tokens = u64::from_le_bytes(
            lab.svm.get_account(&token).unwrap().data[64..72]
                .try_into()
                .unwrap(),
        );
        lab.send(&[ix], &[&trader]).unwrap();
        let after = lab.svm.get_account(&trader.pubkey()).unwrap().lamports;
        let after_tokens = u64::from_le_bytes(
            lab.svm.get_account(&token).unwrap().data[64..72]
                .try_into()
                .unwrap(),
        );
        if buy {
            assert_eq!(balance - after, fill.in_amount);
            assert_eq!(after_tokens - tokens, fill.out_amount);
            held = fill.out_amount;
        } else {
            assert_eq!(after - balance, fill.out_amount);
            assert_eq!(tokens - after_tokens, fill.in_amount);
        }
        for pda in [
            loadout_jupiter_amm::trader_accounts(&trader.pubkey()).0,
            loadout_jupiter_amm::trader_accounts(&trader.pubkey()).1,
        ] {
            assert!(lab.svm.get_account(&pda).is_none_or(|a| a.lamports == 0));
        }
    }
}

#[test]
fn metadata_publication_ignores_protocol_pause_but_not_loader_authority() {
    let authority = Keypair::new();
    let attacker = Keypair::new();
    let mut lab = Lab::new("tests/fixtures/interface.so", &authority);
    lab.seed_market();
    lab.svm.airdrop(&attacker.pubkey(), 100_000_000).unwrap();
    let accounts = loadout_jupiter_amm::ProjectAccounts::derive(195);
    let mut config = lab.svm.get_account(&accounts.fee_config).unwrap();
    config.data[8..40].copy_from_slice(attacker.pubkey().as_ref());
    config.data[108] = 1;
    lab.svm.set_account(accounts.fee_config, config).unwrap();
    let ix = lab.publish(attacker.pubkey(), lab.slot(), lab.hash());
    assert_code(lab.send(&[ix], &[&attacker]), 12309);
    let ix = lab.publish(authority.pubkey(), lab.slot(), lab.hash());
    lab.send(&[ix], &[&authority]).unwrap();
    assert!(!lab.amm().unwrap().is_active());
}

#[test]
fn real_loader_rollback_recognizes_a_zero_extended_historical_executable() {
    let authority = Keypair::new();
    let mut lab = Lab::new("tests/fixtures/nominated.so", &authority);
    lab.seed_market();
    lab.send(
        &[loader::extend_program(
            &LOADOUT_PROGRAM_ID,
            Some(&lab.payer.pubkey()),
            65_536,
        )],
        &[],
    )
    .unwrap();
    lab.svm.warp_to_slot(104);
    assert!(lab.amm().is_ok());
    let buffer = lab.buffer("tests/fixtures/interface.so", authority.pubkey());
    lab.svm.warp_to_slot(105);
    let upgrade = loader::upgrade(
        &LOADOUT_PROGRAM_ID,
        &buffer,
        &authority.pubkey(),
        &lab.payer.pubkey(),
        true,
    );
    lab.send(&[upgrade], &[&authority]).unwrap();
    lab.svm.warp_to_slot(106);
    let publish = lab.publish(authority.pubkey(), lab.slot(), lab.hash());
    lab.send(&[publish], &[&authority]).unwrap();
    assert!(lab.amm().is_ok());
    let buffer = lab.buffer("tests/fixtures/nominated.so", authority.pubkey());
    lab.svm.warp_to_slot(110);
    let rollback = loader::upgrade(
        &LOADOUT_PROGRAM_ID,
        &buffer,
        &authority.pubkey(),
        &lab.payer.pubkey(),
        true,
    );
    lab.send(&[rollback], &[&authority]).unwrap();
    lab.svm.warp_to_slot(111);
    assert_eq!(
        lab.amm().unwrap().market().unwrap().runtime,
        runtime::Runtime::Nominated
    );
    let key = runtime::program_data_address();
    let mut a = lab.svm.get_account(&key).unwrap();
    let last = a.data.len() - 1;
    a.data[last] = 1;
    lab.svm.set_account(key, a).unwrap();
    assert!(
        lab.amm().is_err(),
        "nonzero payload extensions are never inferred compatible"
    );
}

#[test]
fn a_different_elf_with_the_same_quote_version_needs_only_descriptor_republication() {
    let authority = Keypair::new();
    let mut lab = Lab::new("tests/fixtures/interface.so", &authority);
    lab.seed_market();
    let ix = lab.publish(authority.pubkey(), lab.slot(), lab.hash());
    lab.send(&[ix], &[&authority]).unwrap();
    let mut cached = lab.amm().unwrap();
    let before_hash = lab.hash();
    let before_len = lab
        .svm
        .get_account(&runtime::program_data_address())
        .unwrap()
        .data
        .len();
    let buffer = lab.buffer("tests/fixtures/interface-no-log.so", authority.pubkey());
    lab.svm.warp_to_slot(110);
    let upgrade = loader::upgrade(
        &LOADOUT_PROGRAM_ID,
        &buffer,
        &authority.pubkey(),
        &lab.payer.pubkey(),
        true,
    );
    lab.send(&[upgrade], &[&authority]).unwrap();
    assert_ne!(
        lab.hash(),
        before_hash,
        "the ELF changed, not merely the deployment slot"
    );
    assert_eq!(
        lab.svm
            .get_account(&runtime::program_data_address())
            .unwrap()
            .data
            .len(),
        before_len
    );
    assert!(cached.update(Bank(&lab.svm)).is_err());
    assert!(!cached.is_active());
    lab.svm.warp_to_slot(111);
    let ix = lab.publish(authority.pubkey(), lab.slot(), lab.hash());
    lab.send(&[ix], &[&authority]).unwrap();
    cached.update(Bank(&lab.svm)).unwrap();
    assert!(cached.is_active());
    assert_eq!(cached.market().unwrap().runtime, runtime::Runtime::Preview);
    let data = lab
        .svm
        .get_account(&runtime::program_data_address())
        .unwrap();
    assert!(
        runtime::Runtime::decode(&data.data, &data.owner).is_err(),
        "no SDK fingerprint was added for either prototype binary"
    );
}

// A test-only native CPI probe, not an implementation or verification of production Squads.
solana_program_runtime::declare_process_instruction!(GovernancePublisher, 1_000, |context| {
    let current = context
        .transaction_context
        .get_current_instruction_context()?;
    let data = current.get_instruction_data().to_vec();
    let mut accounts = Vec::with_capacity(5);
    for index in 0..5 {
        accounts.push(AccountMeta {
            pubkey: *current.get_key_of_instruction_account(index)?,
            is_signer: index == 2,
            is_writable: matches!(index, 2 | 3),
        });
    }
    let (_, bump) = Pubkey::find_program_address(&[b"authority"], &governance_probe_id());
    context.native_invoke_signed(
        Instruction {
            program_id: LOADOUT_PROGRAM_ID,
            accounts,
            data,
        },
        &[&[b"authority", &[bump]]],
    )
});
fn governance_probe_id() -> Pubkey {
    pubkey!("CpiProbe111111111111111111111111111111111111")
}
#[test]
fn a_program_owned_pda_authority_can_publish_through_cpi_with_prefunded_rent() {
    use solana_program_runtime::solana_sbpf::program::BuiltinFunctionDefinition;
    let bootstrap = Keypair::new();
    let mut lab = Lab::new("tests/fixtures/interface.so", &bootstrap);
    lab.seed_market();
    let probe = governance_probe_id();
    lab.svm.add_builtin(probe, GovernancePublisher::register);
    let (authority, _) = Pubkey::find_program_address(&[b"authority"], &probe);
    // Valid pre-existing governance state: a data-owning PDA cannot pay a System transfer.
    lab.svm
        .set_account(
            authority,
            Account {
                lamports: lab.svm.minimum_balance_for_rent_exemption(1),
                data: vec![1],
                owner: probe,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
    let key = runtime::program_data_address();
    let mut pd = lab.svm.get_account(&key).unwrap();
    pd.data[13..45].copy_from_slice(authority.as_ref());
    lab.svm.set_account(key, pd).unwrap();
    let before = lab.svm.get_account(&authority).unwrap();
    let mut publish = lab.publish(authority, lab.slot(), lab.hash());
    publish.program_id = probe;
    publish.accounts[2].is_signer = false;
    let rent = lab.svm.minimum_balance_for_rent_exemption(84);
    let prefund = solana_system_interface::instruction::transfer(
        &lab.payer.pubkey(),
        &runtime::integration_config_address(),
        rent,
    );
    lab.send(&[prefund, publish], &[]).unwrap();
    assert_eq!(
        lab.svm.get_account(&authority).unwrap(),
        before,
        "the governance account pays no rent and its data is unchanged"
    );
    assert_eq!(
        lab.svm
            .get_account(&runtime::integration_config_address())
            .unwrap()
            .lamports,
        rent
    );
    assert!(lab.amm().is_ok());
}

/// Native instruction parity after each loader upgrade, including actual trader-account rent.
fn assert_upgrade_trade_pair(lab: &mut Lab) {
    use jupiter_amm_interface::{SwapMode, SwapParams};
    use loadout_jupiter_amm::{protocol_fee_shard, state::BondingCurve, trader_accounts};
    let trader = Keypair::new();
    lab.svm.airdrop(&trader.pubkey(), 1_000_000_000).unwrap();
    let mint = lab.amm().unwrap().accounts().token_mint;
    let token = common::create_token_account(&mut lab.svm, &lab.payer, &trader.pubkey(), &mint);
    let balance = |svm: &LiteSVM, key: Pubkey| svm.get_account(&key).map_or(0, |a| a.lamports);
    let tokens = |svm: &LiteSVM| {
        u64::from_le_bytes(
            svm.get_account(&token).unwrap().data[64..72]
                .try_into()
                .unwrap(),
        )
    };
    let (rebate, binding) = trader_accounts(&trader.pubkey());
    for buy in [true, false] {
        let amm = lab.amm().unwrap();
        let market = amm.market().unwrap();
        let now = lab.svm.get_sysvar::<Clock>().unix_timestamp;
        let fill = if buy {
            loadout_jupiter_amm::quote::buy(&market, 10_000_000, now)
        } else {
            loadout_jupiter_amm::quote::sell(&market, tokens(&lab.svm) / 2, now)
        }
        .unwrap();
        let swap = amm
            .get_swap_and_account_metas(&SwapParams {
                swap_mode: SwapMode::ExactIn,
                in_amount: fill.in_amount,
                out_amount: fill.out_amount,
                source_mint: if buy { NATIVE_MINT } else { mint },
                destination_mint: if buy { mint } else { NATIVE_MINT },
                source_token_account: if buy { Pubkey::default() } else { token },
                destination_token_account: if buy { token } else { Pubkey::default() },
                token_transfer_authority: trader.pubkey(),
                user: trader.pubkey(),
                payer: lab.payer.pubkey(),
                quote_mint_to_referrer: None,
                jupiter_program_id: &Pubkey::default(),
                missing_dynamic_accounts_as_default: false,
            })
            .unwrap();
        let before_wallet = balance(&lab.svm, trader.pubkey());
        let before_tokens = tokens(&lab.svm);
        let before_rent = balance(&lab.svm, rebate) + balance(&lab.svm, binding);
        let treasury = amm.accounts().project_treasury;
        let shard = protocol_fee_shard(&trader.pubkey());
        let before_treasury = balance(&lab.svm, treasury);
        let before_protocol = balance(&lab.svm, shard);
        let ix = Instruction {
            program_id: LOADOUT_PROGRAM_ID,
            accounts: swap.account_metas,
            data: encode_instruction(&swap.swap, fill.in_amount, fill.out_amount).unwrap(),
        };
        lab.send(&[ix], &[&trader]).unwrap();
        let rent = balance(&lab.svm, rebate) + balance(&lab.svm, binding) - before_rent;
        if market.runtime == runtime::Runtime::Develop {
            assert_eq!(
                rent, 0,
                "unreferred develop trade allocates no trader accounts"
            );
            assert!(lab.svm.get_account(&rebate).is_none());
            assert!(lab.svm.get_account(&binding).is_none());
        }
        let after_wallet = balance(&lab.svm, trader.pubkey());
        if buy {
            assert_eq!(before_wallet - after_wallet, fill.in_amount + rent);
            assert_eq!(tokens(&lab.svm) - before_tokens, fill.out_amount);
        } else {
            assert_eq!(after_wallet + rent - before_wallet, fill.out_amount);
            assert_eq!(before_tokens - tokens(&lab.svm), fill.in_amount);
        }
        assert_eq!(
            balance(&lab.svm, treasury) - before_treasury + fill.buyback_sol,
            fill.treasury_fee
        );
        assert_eq!(
            balance(&lab.svm, shard) - before_protocol,
            fill.protocol_fee
        );
        let curve =
            BondingCurve::decode(&lab.svm.get_account(&ACTIVE.parse().unwrap()).unwrap().data)
                .unwrap();
        assert_eq!(
            (
                curve.reserve_sol,
                curve.reserve_token,
                curve.virtual_sol,
                curve.virtual_token,
                curve.invariant_k
            ),
            (
                fill.after.new_reserve_sol,
                fill.after.new_reserve_token,
                fill.after.new_virtual_sol,
                fill.after.new_virtual_token,
                fill.after.new_k
            )
        );
        println!(
            "upgrade parity {:?} buy={buy} out={} rent={rent}",
            market.runtime, fill.out_amount
        );
    }
}

fn assert_supported_upgrade_sequence(
    bridge: &str,
    profile: u16,
    expected_runtime: runtime::Runtime,
) {
    let authority = Keypair::new();
    let mut lab = Lab::new("tests/fixtures/nominated.so", &authority);
    lab.seed_market();
    let mut cached = lab.amm().unwrap();
    assert_eq!(
        cached.market().unwrap().runtime,
        runtime::Runtime::Nominated
    );
    assert_upgrade_trade_pair(&mut lab);
    // Extend the pinned current binary before either new executable; SDK fallback stays usable.
    lab.send(
        &[loader::extend_program(
            &LOADOUT_PROGRAM_ID,
            Some(&lab.payer.pubkey()),
            65_536,
        )],
        &[],
    )
    .unwrap();
    lab.svm.warp_to_slot(105);
    cached.update(Bank(&lab.svm)).unwrap();
    let phases = [
        (bridge, 110, profile, expected_runtime),
        (
            "tests/fixtures/interface.so",
            120,
            2,
            runtime::Runtime::Preview,
        ),
        (bridge, 130, profile, expected_runtime),
    ];
    for (path, slot, version, expected) in phases {
        let curve_before = lab.svm.get_account(&ACTIVE.parse().unwrap()).unwrap();
        let buffer = lab.buffer(path, authority.pubkey());
        lab.svm.warp_to_slot(slot);
        lab.send(
            &[loader::upgrade(
                &LOADOUT_PROGRAM_ID,
                &buffer,
                &authority.pubkey(),
                &lab.payer.pubkey(),
                true,
            )],
            &[&authority],
        )
        .unwrap();
        assert_eq!(
            lab.svm.get_account(&ACTIVE.parse().unwrap()).unwrap(),
            curve_before,
            "upgrade preserves live curve bytes"
        );
        assert!(
            cached.update(Bank(&lab.svm)).is_err(),
            "new executable cannot use old metadata"
        );
        assert!(cached.market().is_err());
        assert!(!cached.is_active());
        let pd = lab
            .svm
            .get_account(&runtime::program_data_address())
            .unwrap();
        assert!(
            runtime::Runtime::decode(&pd.data, &pd.owner).is_err(),
            "prototype has no SDK fingerprint"
        );
        lab.svm.warp_to_slot(slot + 1);
        let ix = lab.publish(authority.pubkey(), lab.slot(), lab.hash());
        lab.send(&[ix], &[&authority]).unwrap();
        let descriptor = lab
            .svm
            .get_account(&runtime::integration_config_address())
            .unwrap();
        assert_eq!(
            runtime::IntegrationDescriptor::decode(&descriptor.data, &descriptor.owner)
                .unwrap()
                .quote_version,
            version
        );
        cached.update(Bank(&lab.svm)).unwrap();
        assert_eq!(cached.market().unwrap().runtime, expected);
        assert_upgrade_trade_pair(&mut lab);
    }
    let buffer = lab.buffer("tests/fixtures/nominated.so", authority.pubkey());
    lab.svm.warp_to_slot(140);
    lab.send(
        &[loader::upgrade(
            &LOADOUT_PROGRAM_ID,
            &buffer,
            &authority.pubkey(),
            &lab.payer.pubkey(),
            true,
        )],
        &[&authority],
    )
    .unwrap();
    lab.svm.warp_to_slot(141);
    cached.update(Bank(&lab.svm)).unwrap();
    assert_eq!(
        cached.market().unwrap().runtime,
        runtime::Runtime::Nominated,
        "historical rollback ignores stale descriptor"
    );
    assert_upgrade_trade_pair(&mut lab);
}

#[test]
fn one_sdk_survives_nominated_bridge_preview_and_both_rollbacks() {
    assert_supported_upgrade_sequence(
        "tests/fixtures/nominated-interface.so",
        1,
        runtime::Runtime::Nominated,
    );
}

#[test]
fn nominated_bridge_publisher_preserves_authority_checks_and_fixes_version_one() {
    let authority = Keypair::new();
    let mut lab = Lab::new("tests/fixtures/nominated-interface.so", &authority);
    lab.seed_market();
    assert!(lab.amm().is_err());
    let attacker = Keypair::new();
    lab.svm.airdrop(&attacker.pubkey(), 10_000_000).unwrap();
    let ix = lab.publish(attacker.pubkey(), lab.slot(), lab.hash());
    assert_code(lab.send(&[ix], &[&attacker]), 12309);
    let ix = lab.publish(authority.pubkey(), lab.slot() + 1, lab.hash());
    assert_code(lab.send(&[ix], &[&authority]), 12308);
    let ix = lab.publish(authority.pubkey(), lab.slot(), [0; 32]);
    assert_code(lab.send(&[ix], &[&authority]), 12307);
    let ix = lab.publish(authority.pubkey(), lab.slot(), lab.hash());
    lab.send(std::slice::from_ref(&ix), &[&authority]).unwrap();
    let account = lab
        .svm
        .get_account(&runtime::integration_config_address())
        .unwrap();
    assert_eq!(account.data.len(), 84);
    assert_eq!(
        runtime::IntegrationDescriptor::decode(&account.data, &account.owner)
            .unwrap()
            .quote_version,
        1
    );
    lab.send(&[ix], &[&authority]).unwrap();
    assert_eq!(
        lab.amm().unwrap().market().unwrap().runtime,
        runtime::Runtime::Nominated
    );
}

#[test]
fn schema_one_supports_mainnet_develop_core_preview_and_rollbacks() {
    assert_supported_upgrade_sequence(
        "tests/fixtures/develop-interface.so",
        0,
        runtime::Runtime::Develop,
    );
}

#[test]
fn develop_profile_uses_global_fees_and_does_not_create_unreferred_trader_accounts() {
    let authority = Keypair::new();
    let mut lab = Lab::new("tests/fixtures/develop-interface.so", &authority);
    lab.seed_market();
    let ix = lab.publish(authority.pubkey(), lab.slot(), lab.hash());
    lab.send(&[ix], &[&authority]).unwrap();
    let mut curve = lab.svm.get_account(&ACTIVE.parse().unwrap()).unwrap();
    // Reserved bytes are inert on this executable: deliberately disagree with global fees.
    curve.data[114] = 1;
    curve.data[115..117].copy_from_slice(&1u16.to_le_bytes());
    curve.data[117..119].copy_from_slice(&1u16.to_le_bytes());
    curve.data[119..121].copy_from_slice(&10_000u16.to_le_bytes());
    curve.data[121] = 2;
    lab.svm.set_account(ACTIVE.parse().unwrap(), curve).unwrap();
    assert_eq!(
        lab.amm().unwrap().market().unwrap().runtime,
        runtime::Runtime::Develop
    );
    assert_upgrade_trade_pair(&mut lab);
}
