#![cfg(feature = "fixture-rpc")]

//! Refreshes the historical local-validator fixtures listed in
//! `tests/fixtures/local-states.json` from a matching local validator:
//!
//!     RPC=http://127.0.0.1:48899 cargo test --features fixture-rpc --test snapshot -- --ignored
//!
//! Each curve's accounts go to `tests/fixtures/accounts/<curve>`, the layout the parity tests
//! and Jupiter's test kit read. The mainnet program dump in `tests/fixtures/loadout.so`
//! is retained rather than replaced by the local validator's program.

use jupiter_amm_test_kit::PoolTest;
use litesvm::LiteSVM;
use loadout_jupiter_amm::{LOADOUT_PROGRAM_ID, LoadoutCurve, NATIVE_MINT, PROTOCOL_FEE_SHARDS};
use solana_account::Account;
use solana_pubkey::{Pubkey, pubkey};

#[test]
#[ignore = "needs RPC: snapshots local curves"]
fn snapshot_local_states() {
    let rpc = std::env::var("RPC").expect("set RPC to the validator's URL");
    let file =
        std::fs::read_to_string("tests/fixtures/local-states.json").expect("local-states.json");
    let json: serde_json::Value = serde_json::from_str(&file).expect("json");
    for (name, state) in json["states"].as_object().expect("states") {
        let curve: Pubkey = state["curve"].as_str().unwrap().parse().unwrap();
        let mint: Pubkey = state["mint"].as_str().unwrap().parse().unwrap();
        let test = PoolTest::new(curve)
            .add_swap(NATIVE_MINT, mint, 1_000_000)
            .add_swap(mint, NATIVE_MINT, 1_000_000);
        test.regenerate_fixtures::<LoadoutCurve>(&rpc)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        println!("{name}: {curve}");
    }
}

/// Writes loadout-core's 16 protocol fee shards into every curve's snapshot, each a
/// system account holding its rent-exempt minimum: the deploy funds them before the program takes
/// fees, but a validator running an older program has none to snapshot. Run after
/// `snapshot_local_states`:
///
///     cargo test --test snapshot add_protocol_fee_shards -- --ignored
#[test]
#[ignore = "writes fixtures"]
fn add_protocol_fee_shards() {
    let lamports = LiteSVM::new().minimum_balance_for_rent_exemption(0);
    for entry in std::fs::read_dir("tests/fixtures/accounts").expect("accounts dir") {
        let dir = entry.expect("entry").path();
        for shard in 0..PROTOCOL_FEE_SHARDS {
            let address = Pubkey::find_program_address(
                &[b"protocol_fee_shard", &[shard]],
                &LOADOUT_PROGRAM_ID,
            )
            .0;
            let account = Account {
                lamports,
                data: vec![],
                owner: pubkey!("11111111111111111111111111111111"),
                executable: false,
                rent_epoch: u64::MAX,
            };
            let file = std::fs::File::create(dir.join(format!("{address}.bin"))).expect("create");
            bincode::serialize_into(file, &account).expect("write account");
        }
        println!("{}: {} shards", dir.display(), PROTOCOL_FEE_SHARDS);
    }
}
