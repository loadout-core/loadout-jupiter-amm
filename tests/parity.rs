//! Quote parity: the adapter's quote against `loadout-core`'s real `buy`/`sell` (the mainnet
//! binary in `tests/fixtures/loadout.so`), on committed local-validator snapshots.
//!
//! - Swap amounts excluding trader-account rent and treasury/protocol fees equal the quote.
//!   Raw wallet deltas and separate payer costs have additional explicit assertions.
//! - Every refusal: the quote fails exactly when the program does, with its error code.
//! - Buys also go through Jupiter's own test kit (`assert_pool_parity`).
//!
//! The local snapshots (`tests/fixtures/local-states.json`) were made on a local validator by
//! `scripts/local-states.mjs`. Time-dependent cases (the protected-launch fee, the escape delay)
//! move the clock rather than the accounts. States no snapshot captured (a paused protocol, a
//! paused project, an inactive curve) change that one byte in a copy, and are tested as refusals.

mod common;

use common::{Executed, Snapshot, amm_at, execute};
use jupiter_amm_interface::Amm;
use jupiter_amm_test_kit::{PoolTest, assert_pool_parity};
use loadout_jupiter_amm::quote::{self, Fill};
use loadout_jupiter_amm::{NATIVE_MINT, QuoteError, encode_instruction};
use solana_pubkey::Pubkey;

const ACTIVE: &str = "8gMCp3MpUN6nJ9N2nwjuefrqK6rp6Mrvpnqn1pjUvmwn";
const PROTECTED: &str = "5o56bndGq6YFZFwCTuCmZv42g89bi6gdhC2TEeRYaFbo";
const NEAR_TARGET: &str = "9pN9gVHZXHYoPdW44i7RCS14kBcQdsFGqh3NL1F6tTvX";
const COMPLETED: &str = "3XPTRRAfv2BjTeJx29FUvb5rXGxZffx5Pimjdw9ZVUcZ";
const GRADUATED: &str = "J4WEXs4WBgga81c1JEGXL2rvxwzCRskrz5JhC5zxT97C";
// Public devnet state captured at finalized slot 520718692. Execution uses the
// mainnet program dump, not a claim about the program deployed on devnet.
const DEVNET: &str = "6CECaVm4ywtZUiZVGZA9y1VUeXCwYuQa4EqDt5GHYwx";

#[test]
fn upstream_flow_types_preserve_on_chain_quote_amounts_and_fees() {
    use jupiter_amm_interface::{FlowType, QuoteParams, SwapMode};
    let snapshot = Snapshot::load(ACTIVE);
    let (amm, _) = amm_at(&snapshot, None);
    for side in [Side::Buy, Side::Sell] {
        let (input_mint, output_mint) = mints(&snapshot, side);
        let amount = match side {
            Side::Buy => 100_000_000,
            Side::Sell => 1_000_000,
        };
        let expected = common::quote(&snapshot, input_mint, output_mint, amount, None).unwrap();
        for flow_type in [
            FlowType::Binary,
            FlowType::Ultra,
            FlowType::Manual,
            FlowType::WhitelistedIntegrators,
            FlowType::PaidApi,
            FlowType::FreeApi,
        ] {
            let actual = amm
                .quote(&QuoteParams {
                    amount,
                    input_mint,
                    output_mint,
                    swap_mode: SwapMode::ExactIn,
                    flow_type,
                })
                .unwrap();
            assert_eq!(
                (
                    actual.in_amount,
                    actual.out_amount,
                    actual.fee_amount,
                    actual.fee_mint,
                    actual.fee_pct
                ),
                (
                    expected.in_amount,
                    expected.out_amount,
                    expected.fee_amount,
                    expected.fee_mint,
                    expected.fee_pct
                ),
                "{side:?} {flow_type:?}"
            );
        }
    }
}

const SOL: u64 = 1_000_000_000;
const TOKEN: u64 = 1_000_000;

#[derive(Clone, Copy, Debug)]
enum Side {
    Buy,
    Sell,
}

fn mints(snapshot: &Snapshot, side: Side) -> (Pubkey, Pubkey) {
    let mint = amm_at(snapshot, None).0.accounts().token_mint;
    match side {
        Side::Buy => (NATIVE_MINT, mint),
        Side::Sell => (mint, NATIVE_MINT),
    }
}

/// The adapter's typed quote: `quote::buy`/`quote::sell` on the updated market.
fn typed_quote(
    snapshot: &Snapshot,
    side: Side,
    amount: u64,
    at: Option<i64>,
) -> Result<Fill, QuoteError> {
    let (amm, _) = amm_at(snapshot, at);
    let market = amm.market().expect("market");
    let now = at.unwrap_or(snapshot.clock().unix_timestamp);
    match side {
        Side::Buy => quote::buy(&market, amount, now),
        Side::Sell => quote::sell(&market, amount, now),
    }
}

/// Asserts the quote equals what the program does, to the unit, or that both refuse with the
/// same error. Returns the fill when the trade went through.
fn assert_parity(snapshot: &Snapshot, side: Side, amount: u64, at: Option<i64>) -> Option<Fill> {
    let (input, output) = mints(snapshot, side);
    let quoted = typed_quote(snapshot, side, amount, at);
    let executed = execute(snapshot, input, output, amount, at);
    let label = format!("{side:?} {amount} at {at:?} on {}", snapshot.curve);
    match (&quoted, &executed) {
        (Ok(fill), Ok(done)) => {
            let expected = match side {
                Side::Buy => Executed {
                    sol: fill.in_amount,
                    tokens: fill.out_amount,
                    treasury_fee: fill.treasury_fee,
                    protocol_fee: fill.protocol_fee,
                },
                Side::Sell => Executed {
                    sol: fill.out_amount,
                    tokens: fill.in_amount,
                    treasury_fee: fill.treasury_fee,
                    protocol_fee: fill.protocol_fee,
                },
            };
            assert_eq!(done, &expected, "{label}: executed vs quoted");
            // Jupiter's `Amm::quote` reports the same fill.
            let q = common::quote(snapshot, input, output, amount, at).expect("Amm::quote");
            assert_eq!(
                (q.in_amount, q.out_amount, q.fee_amount, q.fee_mint),
                (fill.in_amount, fill.out_amount, fill.fee(), NATIVE_MINT),
                "{label}: Amm::quote"
            );
            Some(*fill)
        }
        (Err(error), Err(failure)) => {
            let code = error
                .code()
                .unwrap_or_else(|| panic!("{label}: {error:?} has no program code"));
            assert!(
                failure.contains(&format!("Custom({code})")),
                "{label}: quote refused with {error:?} ({code}), program with {failure}"
            );
            assert!(
                common::quote(snapshot, input, output, amount, at).is_err(),
                "{label}: Amm::quote should refuse too"
            );
            None
        }
        _ => panic!("{label}: quote {quoted:?}, program {executed:?}"),
    }
}

#[test]
fn buys_and_sells_on_an_active_curve() {
    let s = Snapshot::load(ACTIVE);
    for amount in [3, 1_000, 10_000, 1_000_000, 50_000_000, SOL / 2, SOL] {
        assert_parity(&s, Side::Buy, amount, None).expect("buy goes through");
    }
    // Sells up to what the curve has sold so far.
    for amount in [100 * TOKEN, 1_000_000 * TOKEN, 5_000_000 * TOKEN] {
        assert_parity(&s, Side::Sell, amount, None).expect("sell goes through");
    }
}

#[test]
fn captured_devnet_state_matches_mainnet_program_execution() {
    let snapshot = Snapshot::load(DEVNET);
    let (input, token) = mints(&snapshot, Side::Buy);
    let mut session = common::Session::new(&snapshot, None);
    let quote = common::quote(&snapshot, input, token, SOL / 100, None).unwrap();
    let bought = session
        .trade(input, token, SOL / 100, quote.out_amount)
        .unwrap();
    assert_eq!(bought.swap.sol, quote.in_amount);
    assert_eq!(bought.swap.tokens, quote.out_amount);
    assert_eq!(
        bought.swap.treasury_fee + bought.swap.protocol_fee,
        quote.fee_amount
    );
    let updated = session.snapshot();
    let sell = common::quote(&updated, token, input, bought.swap.tokens / 2, None).unwrap();
    let sold = session
        .trade(token, input, sell.in_amount, sell.out_amount)
        .unwrap();
    assert_eq!(sold.swap.sol, sell.out_amount);
    assert_eq!(sold.swap.tokens, sell.in_amount);
    assert_eq!(
        sold.swap.treasury_fee + sold.swap.protocol_fee,
        sell.fee_amount
    );
}

#[test]
fn dust_and_impossible_trades_are_refused_like_the_program() {
    let s = Snapshot::load(ACTIVE);
    // Zero; dust whose ceil-rounded fees take everything; more tokens than the curve sold.
    for (side, amount) in [
        (Side::Buy, 0),
        (Side::Buy, 1),
        (Side::Buy, 2),
        (Side::Sell, 0),
        (Side::Sell, 1),
        (Side::Sell, 900_000_000 * TOKEN),
    ] {
        assert_parity(&s, side, amount, None);
    }
}

#[test]
fn the_last_buy_is_filled_only_up_to_the_target() {
    let s = Snapshot::load(NEAR_TARGET);
    let (amm, _) = amm_at(&s, None);
    let market = amm.market().unwrap();
    let room = market.project.treasury_target - market.curve.reserve_sol;
    // Below, at and past the room: past it, the buyer is charged less than they sent.
    for amount in [room / 2, room, room + 1, 2 * room, SOL, 100 * SOL] {
        let fill = assert_parity(&s, Side::Buy, amount, None).expect("buy goes through");
        if amount >= 2 * room {
            assert!(
                fill.in_amount < amount,
                "capped: charged {} of {amount}",
                fill.in_amount
            );
        }
    }
    // Sells still work below the target.
    assert_parity(&s, Side::Sell, 1_000 * TOKEN, None).expect("sell goes through");
}

#[test]
fn the_protected_launch_fee_falls_over_its_window() {
    let s = Snapshot::load(PROTECTED);
    let (amm, _) = amm_at(&s, None);
    let project = amm.market().unwrap().project;
    let (start, duration) = (
        project.launched_at_ts,
        i64::from(project.protected_launch_duration),
    );
    let mut previous_fee = u64::MAX;
    for at in [
        start,
        start + 1,
        start + duration / 4,
        start + duration / 2,
        start + duration - 1,
        start + duration,
        start + 2 * duration,
    ] {
        let fill = assert_parity(&s, Side::Buy, SOL / 10, Some(at)).expect("buy goes through");
        assert!(fill.treasury_fee <= previous_fee, "the fee never rises");
        previous_fee = fill.treasury_fee;
    }
    // Sells pay the normal fee throughout.
    assert_parity(&s, Side::Sell, 1_000 * TOKEN, Some(start + 1)).expect("sell goes through");
}

#[test]
fn a_completed_curve_refuses_trades_until_its_escape_delay_has_passed() {
    let s = Snapshot::load(COMPLETED);
    let (amm, _) = amm_at(&s, None);
    let market = amm.market().unwrap();
    let completed_at = market.curve.completed_at;
    assert!(completed_at > 0, "the snapshot is of a completed curve");
    let delay = match market.fee_config.curve_escape_delay_seconds {
        0 => quote::DEFAULT_CURVE_ESCAPE_DELAY_SECONDS,
        configured => configured,
    };
    let open = completed_at + i64::from(delay);
    // Buys are refused for good; sells until the delay has passed.
    for at in [None, Some(open - 1)] {
        assert!(assert_parity(&s, Side::Buy, SOL / 10, at).is_none());
        assert!(assert_parity(&s, Side::Sell, 1_000 * TOKEN, at).is_none());
        assert!(!amm_at(&s, at).0.is_active(), "not routable at {at:?}");
    }
    assert!(assert_parity(&s, Side::Buy, SOL / 10, Some(open)).is_none());
    assert_parity(&s, Side::Sell, 1_000 * TOKEN, Some(open)).expect("sell goes through");
    assert!(
        amm_at(&s, Some(open)).0.is_active(),
        "routable for sells once the escape opens"
    );
}

#[test]
fn a_graduated_project_is_not_routable() {
    let s = Snapshot::load(GRADUATED);
    assert!(assert_parity(&s, Side::Buy, SOL / 10, None).is_none());
    assert!(assert_parity(&s, Side::Sell, 1_000 * TOKEN, None).is_none());
    assert!(!amm_at(&s, None).0.is_active());
}

#[test]
fn paused_and_inactive_states_are_refused_like_the_program() {
    let s = Snapshot::load(ACTIVE);
    let accounts = *amm_at(&s, None).0.accounts();
    for (key, offset, what) in [
        (accounts.fee_config, 108, "FeeConfig.paused"),
        (accounts.project, 792, "Project.paused"),
        (accounts.bonding_curve, 88, "BondingCurve.is_active = false"),
    ] {
        let value = if offset == 88 { 0 } else { 1 };
        let changed = s.with_byte(&key, offset, value);
        assert!(
            assert_parity(&changed, Side::Buy, SOL / 10, None).is_none(),
            "{what}"
        );
        assert!(
            assert_parity(&changed, Side::Sell, 1_000 * TOKEN, None).is_none(),
            "{what}"
        );
        assert!(
            !amm_at(&changed, None).0.is_active(),
            "{what}: not routable"
        );
    }
    assert!(amm_at(&s, None).0.is_active());
}

/// Jupiter's own check for buys: it runs the instruction the adapter returns and compares the
/// tokens received with `quote.out_amount`. (Its sells would measure a wrapped-SOL account,
/// which `sell` doesn't pay into: it pays native SOL; `assert_parity` covers sells.)
#[test]
fn jupiter_test_kit_buys() {
    for curve in [ACTIVE, PROTECTED, NEAR_TARGET, DEVNET] {
        let s = Snapshot::load(curve);
        let (_, mint) = mints(&s, Side::Buy);
        let test = PoolTest::new(s.curve)
            .add_swap(NATIVE_MINT, mint, 1_000_000)
            .add_swap(NATIVE_MINT, mint, 50_000_000)
            .add_swap(NATIVE_MINT, mint, SOL);
        assert_pool_parity::<loadout_jupiter_amm::LoadoutCurve>(&test, |swap, amount| {
            encode_instruction(swap, amount, 0).expect("a Loadout swap")
        });
    }
}

#[test]
fn total_buy_fee_at_or_above_one_hundred_percent_matches_program_error() {
    let s = Snapshot::load(PROTECTED);
    let (amm, _) = amm_at(&s, None);
    let market = amm.market().unwrap();
    let start = market.project.launched_at_ts;
    let treasury = market.project.protected_launch_fee_bps;
    for total in [10_000u16, 10_001] {
        let mut changed = s.clone();
        let protocol = total
            .checked_sub(treasury)
            .expect("protected fee below 100%");
        changed
            .accounts
            .get_mut(&amm.accounts().fee_config)
            .unwrap()
            .data[104..106]
            .copy_from_slice(&protocol.to_le_bytes());
        let error = typed_quote(&changed, Side::Buy, SOL / 10, Some(start)).unwrap_err();
        assert_eq!(
            error.code(),
            Some(12220),
            "TradeFeeTooHigh from the deployed program"
        );
        assert!(assert_parity(&changed, Side::Buy, SOL / 10, Some(start)).is_none());
    }
}

#[test]
fn first_and_existing_trader_wallet_costs_are_explicit() {
    let s = Snapshot::load(ACTIVE);
    let (input, token) = mints(&s, Side::Buy);
    let mut session = common::Session::new(&s, None);
    let mut bought = 0;
    for first in [true, false] {
        let current = session.snapshot();
        let q = common::quote(&current, input, token, SOL / 10, None).unwrap();
        let report = session.trade(input, token, SOL / 10, q.out_amount).unwrap();
        assert_eq!(report.swap.sol, q.in_amount);
        assert_eq!(report.swap.tokens, q.out_amount);
        assert_eq!(
            report.wallet_delta,
            -i128::from(q.in_amount) - i128::from(report.rent)
        );
        assert_eq!(report.rent, if first { 2_964_960 } else { 0 });
        assert!(report.transaction_fee > 0);
        assert!(report.ata_rent > 0 && report.ata_transaction_fee > 0);
        bought += q.out_amount;
    }
    let current = session.snapshot();
    let q = common::quote(&current, token, input, bought / 2, None).unwrap();
    let report = session
        .trade(token, input, bought / 2, q.out_amount)
        .unwrap();
    assert_eq!(report.rent, 0);
    assert_eq!(report.wallet_delta, i128::from(q.out_amount));
    assert_eq!(report.swap.tokens, q.in_amount);
}

#[test]
fn first_sell_reports_rent_separately_from_sol_output() {
    let s = Snapshot::load(ACTIVE);
    let (token, sol) = mints(&s, Side::Sell);
    let amount = 1_000 * TOKEN;
    let q = common::quote(&s, token, sol, amount, None).unwrap();
    let mut session = common::Session::new(&s, None);
    session.fund_tokens(amount);
    let report = session.trade(token, sol, amount, q.out_amount).unwrap();
    assert_eq!(report.rent, 2_964_960);
    assert_eq!(report.swap.sol, q.out_amount);
    assert_eq!(
        report.wallet_delta,
        i128::from(q.out_amount) - i128::from(report.rent)
    );
}

#[test]
fn slippage_failure_rolls_back_first_trade_account_creation_and_all_swap_writes() {
    let s = Snapshot::load(ACTIVE);
    for side in [Side::Buy, Side::Sell] {
        let (input, output) = mints(&s, side);
        let amount = if matches!(side, Side::Buy) {
            SOL / 10
        } else {
            1_000 * TOKEN
        };
        let q = common::quote(&s, input, output, amount, None).unwrap();
        let mut session = common::Session::new(&s, None);
        if matches!(side, Side::Sell) {
            session.fund_tokens(amount);
        }
        let failure = session
            .trade(input, output, amount, q.out_amount + 1)
            .unwrap_err();
        assert!(failure.contains("Custom(12022)"), "{failure}");
        // Retrying at the exact quote succeeds; the failed attempt created no trader accounts.
        let report = session.trade(input, output, amount, q.out_amount).unwrap();
        assert_eq!(report.rent, 2_964_960);
    }
}

#[test]
fn capped_buy_charges_only_consumed_sol_plus_observed_rent() {
    let s = Snapshot::load(NEAR_TARGET);
    let (sol, token) = mints(&s, Side::Buy);
    let offered = 100 * SOL;
    let q = common::quote(&s, sol, token, offered, None).unwrap();
    assert!(q.in_amount < offered);
    let mut session = common::Session::new(&s, None);
    let report = session.trade(sol, token, offered, q.out_amount).unwrap();
    assert_eq!(report.swap.sol, q.in_amount);
    assert_eq!(report.swap.tokens, q.out_amount);
    assert_eq!(
        report.wallet_delta,
        -i128::from(q.in_amount) - i128::from(report.rent)
    );
}

#[test]
fn just_below_one_hundred_percent_still_matches_the_program() {
    let mut s = Snapshot::load(PROTECTED);
    let (amm, _) = amm_at(&s, None);
    let project = amm.market().unwrap().project;
    let protocol = 9_999u16 - project.protected_launch_fee_bps;
    s.accounts.get_mut(&amm.accounts().fee_config).unwrap().data[104..106]
        .copy_from_slice(&protocol.to_le_bytes());
    assert_parity(&s, Side::Buy, SOL / 10, Some(project.launched_at_ts))
        .expect("positive net below 100%");
}

/// Synthetic small-supply boundaries executed against the mainnet program dump.
#[test]
fn small_supply_boundary_quotes_match_deployed_execution() {
    for gap in [1u64, 440, 441, 0] {
        let mut s = Snapshot::load(ACTIVE);
        let accounts = *amm_at(&s, None).0.accounts();
        let supply = 1_000 * TOKEN;
        let vs = 30 * SOL;
        let d = &mut s.accounts.get_mut(&s.curve).unwrap().data;
        for (offset, value) in [(16, 0), (24, supply), (32, vs), (40, supply)] {
            d[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        d[48..64].copy_from_slice(&(u128::from(vs) * u128::from(supply)).to_le_bytes());
        s.accounts.get_mut(&accounts.token_vault).unwrap().data[64..72]
            .copy_from_slice(&supply.to_le_bytes());
        let p = &mut s.accounts.get_mut(&accounts.project).unwrap().data;
        p[88..96].copy_from_slice(&(85 * SOL).to_le_bytes());
        p[786..790].copy_from_slice(&0u32.to_le_bytes());
        let f = &mut s.accounts.get_mut(&accounts.fee_config).unwrap().data;
        f[104..106].copy_from_slice(&100u16.to_le_bytes());
        f[106..108].copy_from_slice(&400u16.to_le_bytes());
        let gross = loadout_math::fees::gross_for_net(85 * SOL - gap, 400, 100).unwrap();
        if matches!(gap, 1 | 440) {
            let error = typed_quote(&s, Side::Buy, gross, None).unwrap_err();
            assert_eq!(error.code(), Some(12221), "unfillable gap {gap}");
            assert!(assert_parity(&s, Side::Buy, gross, None).is_none());
            continue;
        }
        let fill = assert_parity(&s, Side::Buy, gross, None);
        assert!(
            fill.is_some(),
            "deployed Nominated program accepts gap {gap}"
        );
    }
}
