//! The raw readers against real accounts (the snapshots), and their refusals.

mod common;

use common::Snapshot;
use loadout_jupiter_amm::ProjectAccounts;
use loadout_jupiter_amm::state::{BondingCurve, FeeConfig, Project};

const ACTIVE: &str = "8gMCp3MpUN6nJ9N2nwjuefrqK6rp6Mrvpnqn1pjUvmwn";

#[test]
fn reads_a_real_curve_its_project_and_the_fee_config() {
    let s = Snapshot::load(ACTIVE);
    let curve = BondingCurve::decode(&s.accounts[&s.curve].data).unwrap();
    let accounts = ProjectAccounts::derive(curve.project_id);
    assert_eq!(
        accounts.bonding_curve, s.curve,
        "the curve is its project's PDA"
    );
    let project = Project::decode(&s.accounts[&accounts.project].data).unwrap();
    let fee_config = FeeConfig::decode(&s.accounts[&accounts.fee_config].data).unwrap();

    assert_eq!(project.project_id, curve.project_id);
    assert_eq!(
        project.token_mint, accounts.token_mint,
        "the mint is the project's PDA"
    );
    assert_eq!(project.state, 1, "Live");
    assert!(curve.is_active && !project.paused && !fee_config.paused);
    // The vault holds the curve's real tokens; the curve account holds its real SOL plus rent.
    let vault = &s.accounts[&accounts.token_vault].data;
    assert_eq!(
        u64::from_le_bytes(vault[64..72].try_into().unwrap()),
        curve.reserve_token
    );
    assert!(s.accounts[&s.curve].lamports > curve.reserve_sol);
    // Constant product: k is the product of the virtual reserves (rounding can only raise k).
    assert!(
        curve.invariant_k
            >= u128::from(curve.virtual_sol) * u128::from(curve.virtual_token)
                - u128::from(curve.virtual_sol)
    );
    assert!(fee_config.treasury_fee_bps > 0 && fee_config.protocol_fee_bps > 0);
    assert_eq!(
        fee_config.protocol_fee_recipient.to_bytes(),
        s.accounts[&accounts.fee_config].data[72..104]
    );
}

#[test]
fn refuses_other_accounts() {
    let s = Snapshot::load(ACTIVE);
    let curve = s.accounts[&s.curve].data.clone();
    let accounts = ProjectAccounts::derive(BondingCurve::decode(&curve).unwrap().project_id);
    let project = s.accounts[&accounts.project].data.clone();
    // Another account type, a wrong discriminator, a wrong size, a non-0/1 bool.
    assert!(BondingCurve::decode(&project).is_err());
    assert!(Project::decode(&curve).is_err());
    let mut wrong = curve.clone();
    wrong[0] ^= 1;
    assert!(BondingCurve::decode(&wrong).is_err());
    assert!(BondingCurve::decode(&curve[..153]).is_err());
    let mut flag = curve.clone();
    flag[88] = 2;
    assert!(BondingCurve::decode(&flag).is_err());
}
