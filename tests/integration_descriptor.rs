//! Metadata parser tests; synthetic payloads below exercise identity validation only.
mod common;
use common::Snapshot;
use loadout_jupiter_amm::{
    LOADOUT_PROGRAM_ID,
    runtime::{self, Runtime},
};
use sha2::{Digest, Sha256};
use solana_pubkey::Pubkey;
const ACTIVE: &str = "8gMCp3MpUN6nJ9N2nwjuefrqK6rp6Mrvpnqn1pjUvmwn";
fn descriptor(program: &[u8], slot: u64) -> Vec<u8> {
    let mut d = vec![0; 84];
    d[..8].copy_from_slice(&Sha256::digest(b"account:IntegrationConfig")[..8]);
    d[8] = 1;
    d[9..11].copy_from_slice(&2u16.to_le_bytes());
    d[11..19].copy_from_slice(&slot.to_le_bytes());
    d[19..51].copy_from_slice(&Sha256::digest(&program[45..]));
    d[51] = Pubkey::find_program_address(&[b"integration_config"], &LOADOUT_PROGRAM_ID).1;
    d
}
#[test]
fn unknown_binary_requires_valid_matching_descriptor_and_supported_version() {
    let s = Snapshot::load(ACTIVE);
    let a = &s.accounts[&runtime::program_data_address()];
    let mut program = a.data[..45].to_vec();
    program[4..12].copy_from_slice(&42u64.to_le_bytes());
    program.extend_from_slice(&std::fs::read("tests/fixtures/interface.so").unwrap());
    assert!(Runtime::resolve(&program, &a.owner, None, None).is_err());
    let d = descriptor(&program, 42);
    assert_eq!(
        Runtime::resolve(&program, &a.owner, Some((&d, &LOADOUT_PROGRAM_ID)), None).unwrap(),
        Runtime::Preview
    );
    for (offset, value) in [(0, 0), (8, 2), (9, 3), (11, 43), (19, 0), (51, 0), (52, 1)] {
        let mut bad = d.clone();
        bad[offset] = value;
        assert!(
            Runtime::resolve(&program, &a.owner, Some((&bad, &LOADOUT_PROGRAM_ID)), None).is_err(),
            "offset {offset}"
        );
    }
    assert!(Runtime::resolve(&program, &a.owner, Some((&d, &Pubkey::new_unique())), None).is_err());
    assert!(
        Runtime::resolve(
            &program,
            &a.owner,
            Some((&d[..83], &LOADOUT_PROGRAM_ID)),
            None
        )
        .is_err()
    );
    program[4..12].copy_from_slice(&43u64.to_le_bytes());
    assert!(Runtime::resolve(&program, &a.owner, Some((&d, &LOADOUT_PROGRAM_ID)), None).is_err());
    // Two different executables can occupy the same deployment slot: the hash must still match.
    program[4..12].copy_from_slice(&42u64.to_le_bytes());
    let last = program.len() - 1;
    program[last] ^= 1;
    assert!(Runtime::resolve(&program, &a.owner, Some((&d, &LOADOUT_PROGRAM_ID)), None).is_err());
}
#[test]
fn pinned_historical_binary_remains_usable_after_rollback_with_stale_metadata() {
    let s = Snapshot::load(ACTIVE);
    let a = &s.accounts[&runtime::program_data_address()];
    let stale = vec![99; 84];
    assert_eq!(
        Runtime::resolve(&a.data, &a.owner, Some((&stale, &LOADOUT_PROGRAM_ID)), None).unwrap(),
        Runtime::Legacy
    );
}
