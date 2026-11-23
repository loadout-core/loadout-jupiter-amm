mod common;

use common::{Session, Snapshot, amm_at, execute};
use jupiter_amm_interface::{AccountProvider, Amm};
use loadout_jupiter_amm::{LOADOUT_PROGRAM_ID, NATIVE_MINT, quote};
use solana_account::ReadableAccount;
use solana_pubkey::{Pubkey, pubkey};

const ACTIVE: &str = "8gMCp3MpUN6nJ9N2nwjuefrqK6rp6Mrvpnqn1pjUvmwn";

fn with_fee_snapshot() -> Snapshot {
    let mut snapshot = Snapshot::load(ACTIVE);
    let curve = &mut snapshot.accounts.get_mut(&snapshot.curve).unwrap().data;
    curve[114] = 1;
    curve[115..117].copy_from_slice(&100u16.to_le_bytes());
    curve[117..119].copy_from_slice(&100u16.to_le_bytes());
    snapshot
}

#[test]
fn captured_curve_fees_match_mainnet_execution() {
    // A captured fee split deliberately differs from the current global config.
    let snapshot = with_fee_snapshot();
    let mut session = Session::new(&snapshot, None);
    let market = amm_at(&snapshot, None).0.market().unwrap();
    let mint = market.project.token_mint;
    let fill = quote::buy(&market, 10_000_000, snapshot.clock().unix_timestamp).unwrap();
    let trade = session.trade(NATIVE_MINT, mint, 10_000_000, 0).unwrap();
    assert_eq!(trade.swap.tokens, fill.out_amount);
    assert_eq!(trade.swap.treasury_fee, fill.treasury_fee);
    assert_eq!(trade.swap.protocol_fee, fill.protocol_fee);

    let snapshot = session.snapshot();
    let market = amm_at(&snapshot, None).0.market().unwrap();
    let amount = fill.out_amount / 2;
    let fill = quote::sell(&market, amount, snapshot.clock().unix_timestamp).unwrap();
    let trade = session.trade(mint, NATIVE_MINT, amount, 0).unwrap();
    assert_eq!(trade.swap.sol, fill.out_amount);
    assert_eq!(trade.swap.treasury_fee, fill.treasury_fee);
    assert_eq!(trade.swap.protocol_fee, fill.protocol_fee);
    let after = amm_at(&session.snapshot(), None).0.market().unwrap().curve;
    assert_eq!(after.reserve_sol, fill.after.new_reserve_sol);
    assert_eq!(after.reserve_token, fill.after.new_reserve_token);
}

#[test]
fn protected_fee_excess_is_split_at_the_captured_rate() {
    let mut snapshot = with_fee_snapshot();
    let now = snapshot.clock().unix_timestamp;
    let project_key = amm_at(&snapshot, None).0.accounts().project;
    let project = &mut snapshot.accounts.get_mut(&project_key).unwrap().data;
    project[722..730].copy_from_slice(&now.to_le_bytes());
    project[786..790].copy_from_slice(&60u32.to_le_bytes());
    project[790..792].copy_from_slice(&2_000u16.to_le_bytes());
    snapshot.accounts.get_mut(&snapshot.curve).unwrap().data[119..121]
        .copy_from_slice(&5_000u16.to_le_bytes());
    let market = amm_at(&snapshot, None).0.market().unwrap();
    let amount = 1_000_000_000;
    let fill = quote::buy(&market, amount, now).unwrap();
    // Half the 19% excess treasury charge moves to the protocol.
    assert_eq!(
        (fill.treasury_fee, fill.protocol_fee),
        (105_000_000, 105_000_000)
    );
    let trade = execute(
        &snapshot,
        NATIVE_MINT,
        market.project.token_mint,
        amount,
        None,
    )
    .unwrap();
    assert_eq!(
        (trade.treasury_fee, trade.protocol_fee),
        (105_000_000, 105_000_000)
    );
    assert_eq!(trade.tokens, fill.out_amount);
}

struct Provider<'a>(&'a Snapshot);

impl<'a> AccountProvider for Provider<'a> {
    fn get(&self, key: &Pubkey) -> Option<impl ReadableAccount + use<'_, 'a>> {
        self.0.accounts.get(key).cloned()
    }
}

#[test]
fn refresh_accepts_only_the_deployed_nominated_payload() {
    let mut snapshot = Snapshot::load(ACTIVE);
    let (mut amm, _) = amm_at(&snapshot, None);
    let loader = pubkey!("BPFLoaderUpgradeab1e11111111111111111111111");
    let (program_data, _) = Pubkey::find_program_address(&[LOADOUT_PROGRAM_ID.as_ref()], &loader);
    assert!(amm.get_accounts_to_update().contains(&program_data));
    assert!(amm.is_active());

    let account = snapshot.accounts.get_mut(&program_data).unwrap();
    account.data.extend([0; 8]);
    amm.update(Provider(&snapshot)).unwrap();
    assert!(amm.is_active());

    *snapshot
        .accounts
        .get_mut(&program_data)
        .unwrap()
        .data
        .last_mut()
        .unwrap() = 1;
    assert!(amm.update(Provider(&snapshot)).is_err());
    assert!(!amm.is_active());
    assert!(amm.market().is_err());

    let mut snapshot = Snapshot::load(ACTIVE);
    amm.update(Provider(&snapshot)).unwrap();
    assert!(amm.is_active());
    snapshot.accounts.remove(&program_data);
    assert!(amm.update(Provider(&snapshot)).is_err());
    assert!(!amm.is_active());
}
