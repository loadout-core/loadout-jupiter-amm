//! Cross-release regression tests. The program fixture, not the quote, is the execution oracle.
mod common;

use common::{Snapshot, amm_at, execute};
use loadout_jupiter_amm::{NATIVE_MINT, quote};

const ACTIVE: &str = "8gMCp3MpUN6nJ9N2nwjuefrqK6rp6Mrvpnqn1pjUvmwn";

fn market(path: &'static str) -> Snapshot {
    let mut s = Snapshot::load(ACTIVE);
    s.program_path = path;
    let program = std::fs::read(path).unwrap();
    let data = &mut s
        .accounts
        .get_mut(&loadout_jupiter_amm::runtime::program_data_address())
        .unwrap()
        .data;
    data.truncate(45);
    data.extend_from_slice(&program);
    let curve = &mut s.accounts.get_mut(&s.curve).unwrap().data;
    curve[114] = 1;
    curve[115..117].copy_from_slice(&100u16.to_le_bytes());
    curve[117..119].copy_from_slice(&100u16.to_le_bytes());
    s
}

#[test]
fn project_fee_snapshot_matches_execution_on_both_releases() {
    for path in ["tests/fixtures/nominated.so", "tests/fixtures/next.so"] {
        let s = market(path);
        let (amm, _) = amm_at(&s, None);
        let m = amm.market().unwrap();
        let fill = quote::buy(&m, 1_000_000_000, s.clock().unix_timestamp).unwrap();
        let done = execute(&s, NATIVE_MINT, m.project.token_mint, 1_000_000_000, None).unwrap();
        assert_eq!(fill.out_amount, done.tokens, "{path}: token output");
        assert_eq!(fill.treasury_fee, done.treasury_fee, "{path}: treasury");
        assert_eq!(fill.protocol_fee, done.protocol_fee, "{path}: protocol");
    }
}

#[test]
fn consecutive_buys_and_sells_match_wallet_fees_rent_and_post_state() {
    use loadout_jupiter_amm::state::BondingCurve;
    for path in ["tests/fixtures/nominated.so", "tests/fixtures/next.so"] {
        for buyback in [false, true] {
            let mut s = market(path);
            let accounts = *amm_at(&s, None).0.accounts();
            if buyback {
                // The current release refuses enabling inline buybacks; only the preview fixture
                // is configured with this flag. Both releases still exercise treasury fee snapshots.
                if path == "tests/fixtures/next.so" {
                    s.accounts.get_mut(&s.curve).unwrap().data[121] = 2;
                    s.accounts.get_mut(&accounts.project).unwrap().data[793..795]
                        .copy_from_slice(&5_000u16.to_le_bytes());
                }
            }
            let mut session = common::Session::new(&s, None);
            let mut held_tokens = 0;
            for buy in [true, true, false, true, false] {
                let before = session.snapshot();
                let m = amm_at(&before, None).0.market().unwrap();
                let amount = if buy { 10_000_000 } else { held_tokens / 2 };
                let fill = if buy {
                    quote::buy(&m, amount, before.clock().unix_timestamp)
                } else {
                    quote::sell(&m, amount, before.clock().unix_timestamp)
                }
                .unwrap();
                let (input, output) = if buy {
                    (NATIVE_MINT, m.project.token_mint)
                } else {
                    (m.project.token_mint, NATIVE_MINT)
                };
                let report = session
                    .trade(input, output, amount, fill.out_amount)
                    .unwrap();
                assert_eq!(
                    report.swap.sol,
                    if buy { fill.in_amount } else { fill.out_amount }
                );
                assert_eq!(
                    report.swap.tokens,
                    if buy { fill.out_amount } else { fill.in_amount }
                );
                assert_eq!(
                    report.swap.treasury_fee + fill.buyback_sol,
                    fill.treasury_fee
                );
                assert_eq!(report.swap.protocol_fee, fill.protocol_fee);
                let after = session.snapshot();
                let curve = BondingCurve::decode(&after.accounts[&s.curve].data).unwrap();
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
                if buyback && path == "tests/fixtures/next.so" {
                    assert!(fill.buyback_sol > 0);
                    assert!(fill.buyback_tokens > 0);
                } else {
                    assert_eq!(fill.buyback_sol, 0);
                }
                held_tokens = if buy {
                    held_tokens + fill.out_amount
                } else {
                    held_tokens - amount
                };
            }
        }
    }
}

#[test]
fn protected_launch_sniper_split_matches_both_executables() {
    for path in ["tests/fixtures/nominated.so", "tests/fixtures/next.so"] {
        let mut s = market(path);
        let accounts = *amm_at(&s, None).0.accounts();
        let now = s.clock().unix_timestamp;
        let p = &mut s.accounts.get_mut(&accounts.project).unwrap().data;
        p[722..730].copy_from_slice(&now.to_le_bytes());
        p[786..790].copy_from_slice(&60u32.to_le_bytes());
        p[790..792].copy_from_slice(&2_000u16.to_le_bytes());
        s.accounts.get_mut(&s.curve).unwrap().data[119..121]
            .copy_from_slice(&5_000u16.to_le_bytes());
        let m = amm_at(&s, None).0.market().unwrap();
        let fill = quote::buy(&m, 1_000_000_000, now).unwrap();
        // 20% treasury charge, less half of its 19% excess; 1% protocol plus that half.
        assert_eq!(
            (fill.treasury_fee, fill.protocol_fee),
            (105_000_000, 105_000_000)
        );
        let done = execute(&s, NATIVE_MINT, m.project.token_mint, 1_000_000_000, None).unwrap();
        assert_eq!(
            (done.treasury_fee, done.protocol_fee),
            (105_000_000, 105_000_000)
        );
        assert_eq!(fill.out_amount, done.tokens);
    }
}

#[test]
fn unfillable_final_gap_is_refused_on_both_new_executables() {
    for path in ["tests/fixtures/nominated.so", "tests/fixtures/next.so"] {
        let mut s = market(path);
        let accounts = *amm_at(&s, None).0.accounts();
        let supply = 1_000_000_000u64;
        let vs = 30_000_000_000u64;
        let target = 85_000_000_000u64;
        let curve = &mut s.accounts.get_mut(&s.curve).unwrap().data;
        for (offset, value) in [(16, 0), (24, supply), (32, vs), (40, supply)] {
            curve[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        curve[48..64].copy_from_slice(&(u128::from(vs) * u128::from(supply)).to_le_bytes());
        s.accounts.get_mut(&accounts.token_vault).unwrap().data[64..72]
            .copy_from_slice(&supply.to_le_bytes());
        s.accounts.get_mut(&accounts.project).unwrap().data[88..96]
            .copy_from_slice(&target.to_le_bytes());
        let m = amm_at(&s, None).0.market().unwrap();
        let gross = loadout_math::fees::gross_for_net(target - 1, 100, 100).unwrap();
        let error = quote::buy(&m, gross, s.clock().unix_timestamp).unwrap_err();
        assert_eq!(error.code(), Some(12221));
        let failure = execute(&s, NATIVE_MINT, m.project.token_mint, gross, None).unwrap_err();
        assert!(failure.contains("Custom(12221)"), "{path}: {failure}");
    }
}

#[path = "common/kit.rs"]
mod kit;

#[test]
fn upstream_jupiter_kit_matches_buys_on_both_new_executables() {
    use jupiter_amm_test_kit::{PoolSnapshot, PoolTest, assert_pool_parity};
    for preview in [false, true] {
        let path = if preview {
            "tests/fixtures/next.so"
        } else {
            "tests/fixtures/nominated.so"
        };
        let s = market(path);
        let token = amm_at(&s, None).0.accounts().token_mint;
        // Isolated output, never changing the committed snapshots or program fixtures.
        let dir =
            std::env::temp_dir().join(format!("loadout-kit-{}-{}", std::process::id(), preview));
        PoolSnapshot::from_accounts(s.accounts.clone())
            .save_dir(&dir)
            .unwrap();
        let mut test = PoolTest::new(s.curve)
            .add_swap(NATIVE_MINT, token, 10_000_000)
            .add_swap(NATIVE_MINT, token, 1_000_000_000);
        test.fixtures_dir = dir.clone();
        let encode = |swap: &jupiter_amm_interface::Swap, amount| {
            loadout_jupiter_amm::encode_instruction(swap, amount, 0).unwrap()
        };
        if preview {
            assert_pool_parity::<kit::FixtureAmm<true>>(&test, encode);
        } else {
            assert_pool_parity::<kit::FixtureAmm<false>>(&test, encode);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn slippage_failure_preserves_state_and_first_trade_costs_on_both_releases() {
    for path in ["tests/fixtures/nominated.so", "tests/fixtures/next.so"] {
        let s = market(path);
        let m = amm_at(&s, None).0.market().unwrap();
        let fill = quote::buy(&m, 10_000_000, s.clock().unix_timestamp).unwrap();
        let mut session = common::Session::new(&s, None);
        let failure = session
            .trade(
                NATIVE_MINT,
                m.project.token_mint,
                10_000_000,
                fill.out_amount + 1,
            )
            .unwrap_err();
        assert!(failure.contains("Custom(12022)"), "{failure}");
        let report = session
            .trade(
                NATIVE_MINT,
                m.project.token_mint,
                10_000_000,
                fill.out_amount,
            )
            .unwrap();
        assert_eq!(
            report.rent,
            if path == "tests/fixtures/nominated.so" {
                2_964_960
            } else {
                0
            }
        );
        assert_eq!(report.swap.tokens, fill.out_amount);
        assert_eq!(
            report.wallet_delta,
            -i128::from(fill.in_amount) - i128::from(report.rent)
        );
    }
}

#[path = "common/execution.rs"]
mod composition;

#[test]
fn both_releases_compose_native_sol_buys_and_sells_with_wallet_and_pda_signers() {
    for path in ["tests/fixtures/nominated.so", "tests/fixtures/next.so"] {
        for pda in [false, true] {
            let mut lab = composition::Lab::new(market(path), pda);
            let m = amm_at(&lab.snapshot(), None).0.market().unwrap();
            let q = quote::buy(&m, 10_000_000, lab.snapshot().clock().unix_timestamp).unwrap();
            let before = lab.balance(lab.authority);
            let buy = lab.relay(
                lab.swap(true, q.in_amount, q.out_amount, lab.authority_token),
                pda,
            );
            let sent = lab.send(&[buy]);
            assert!(sent.error.is_none(), "{path} pda={pda}: {sent:?}");
            assert_eq!(lab.tokens(lab.authority_token), q.out_amount);
            assert_eq!(
                before - lab.balance(lab.authority),
                q.in_amount + lab.rent()
            );
            let m = amm_at(&lab.snapshot(), None).0.market().unwrap();
            let q =
                quote::sell(&m, q.out_amount / 2, lab.snapshot().clock().unix_timestamp).unwrap();
            let before = lab.balance(lab.authority);
            let tokens = lab.tokens(lab.authority_token);
            let sell = lab.relay(
                lab.swap(false, q.in_amount, q.out_amount, lab.authority_token),
                pda,
            );
            let sent = lab.send(&[sell]);
            assert!(sent.error.is_none(), "{path} pda={pda}: {sent:?}");
            assert_eq!(lab.balance(lab.authority) - before, q.out_amount);
            assert_eq!(tokens - lab.tokens(lab.authority_token), q.in_amount);
        }
    }
}
