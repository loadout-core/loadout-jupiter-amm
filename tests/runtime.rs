//! Executable identity and refresh failures are part of the quoting boundary.
mod common;
use common::{Snapshot, amm_at};
use jupiter_amm_interface::{AccountProvider, Amm};
use loadout_jupiter_amm::{
    NATIVE_MINT,
    runtime::{Runtime, program_data_address},
};
use solana_account::{Account, ReadableAccount};
use solana_pubkey::Pubkey;
use std::collections::HashMap;
const ACTIVE: &str = "8gMCp3MpUN6nJ9N2nwjuefrqK6rp6Mrvpnqn1pjUvmwn";
struct Provider<'a>(&'a HashMap<Pubkey, Account>);
impl<'a> AccountProvider for Provider<'a> {
    fn get(&self, key: &Pubkey) -> Option<impl ReadableAccount + use<'_, 'a>> {
        self.0.get(key).cloned()
    }
}
#[test]
fn executable_identity_is_independent_of_deployment_slot_and_authority() {
    let s = Snapshot::load(ACTIVE);
    let mut a = s.accounts[&program_data_address()].clone();
    assert_eq!(Runtime::decode(&a.data, &a.owner).unwrap(), Runtime::Legacy);
    a.data[4..12].copy_from_slice(&u64::MAX.to_le_bytes());
    a.data[12] = 1;
    a.data[13..45].fill(9);
    assert_eq!(Runtime::decode(&a.data, &a.owner).unwrap(), Runtime::Legacy);
    let end = a.data.len() - 1;
    a.data[end] ^= 1;
    assert!(Runtime::decode(&a.data, &a.owner).is_err());
}
#[test]
fn failed_refresh_invalidates_a_previously_quoteable_market() {
    let mut s = Snapshot::load(ACTIVE);
    let (mut amm, _) = amm_at(&s, None);
    let token = amm.accounts().token_mint;
    assert!(amm.is_active());
    s.accounts.get_mut(&program_data_address()).unwrap().owner = Pubkey::new_unique();
    assert!(amm.update(Provider(&s.accounts)).is_err());
    assert!(!amm.is_active());
    assert!(amm.market().is_err());
    assert!(
        amm.quote(&jupiter_amm_interface::QuoteParams {
            amount: 10_000_000,
            input_mint: NATIVE_MINT,
            output_mint: token,
            swap_mode: jupiter_amm_interface::SwapMode::ExactIn,
            flow_type: Default::default(),
        })
        .is_err()
    );
}

#[test]
fn deployment_parameters_are_strict_and_cannot_relabel_a_known_binary() {
    use loadout_jupiter_amm::runtime::Deployment;
    use serde_json::json;
    use sha2::{Digest, Sha256};
    let s = Snapshot::load(ACTIVE);
    let a = &s.accounts[&program_data_address()];
    let hash = format!("{:x}", Sha256::digest(&a.data[45..]));
    let value = json!({"loadout_deployment": {"profile": "nominated", "sha256": hash}});
    let d = Deployment::from_params(Some(&value)).unwrap();
    assert!(Runtime::decode_with_deployment(&a.data, &a.owner, d).is_err());
    for hash in ["", "abc", &"g".repeat(64), &"é".repeat(32)] {
        assert!(
            Deployment::from_params(Some(&json!({"loadout_deployment": {
                "profile": "nominated", "sha256": hash
            }})))
            .is_err()
        );
    }
    assert!(
        Deployment::from_params(Some(&json!({"loadout_deployment": {
            "profile": "future-unknown", "sha256": "0".repeat(64)
        }})))
        .is_err()
    );
}

#[test]
fn another_binary_requires_an_exact_explicit_behavior_registration() {
    use loadout_jupiter_amm::runtime::Deployment;
    use sha2::{Digest, Sha256};
    let s = Snapshot::load(ACTIVE);
    let mut a = s.accounts[&program_data_address()].clone();
    // Test configuration parsing only: changing bytes does not establish program compatibility.
    let end = a.data.len() - 1;
    a.data[end] ^= 1;
    assert!(Runtime::decode(&a.data, &a.owner).is_err());
    let deployment = Deployment {
        sha256: Sha256::digest(&a.data[45..]).into(),
        runtime: Runtime::Legacy,
    };
    assert_eq!(
        Runtime::decode_with_deployment(&a.data, &a.owner, Some(deployment)).unwrap(),
        Runtime::Legacy
    );
    a.data[end] ^= 2;
    assert!(Runtime::decode_with_deployment(&a.data, &a.owner, Some(deployment)).is_err());
}

#[test]
fn preview_matches_the_current_mainnet_programdata_capacity() {
    let s = Snapshot::load(ACTIVE);
    let a = &s.accounts[&program_data_address()];
    let mut data = a.data[..45].to_vec();
    let program = std::fs::read("tests/fixtures/next.so").unwrap();
    data.extend_from_slice(&program);
    data.resize(a.data.len(), 0);
    assert_eq!(Runtime::decode(&data, &a.owner).unwrap(), Runtime::Preview);
    // The allocation size is part of the identity: arbitrary extensions are not accepted.
    data.push(0);
    assert!(Runtime::decode(&data, &a.owner).is_err());
}
