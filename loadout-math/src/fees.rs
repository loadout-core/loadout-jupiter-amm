//! Fee calculation helpers.
//!
//! All rounding favours the protocol (ceiling).

/// Calculate fee amount given a gross amount and fee in basis points.
///
/// fee = ceil(amount * fee_bps / 10_000)
///
/// Returns `None` on overflow (shouldn't happen with u64 amounts and u16 bps).
#[inline]
pub fn calculate_fee(amount: u64, fee_bps: u16) -> Option<u64> {
    if fee_bps == 0 || amount == 0 {
        return Some(0);
    }
    let amount = amount as u128;
    let bps = fee_bps as u128;
    // ceil(amount * bps / 10_000)
    let numerator = amount.checked_mul(bps)?;
    let fee = numerator.checked_add(9_999)? / 10_000;
    u64::try_from(fee).ok()
}

/// Calculate fee amount using floor rounding (for sub-splits where protocol keeps dust).
///
/// fee = floor(amount * fee_bps / 10_000)
///
/// Returns `None` on overflow.
#[inline]
pub fn floor_fee(amount: u64, fee_bps: u16) -> Option<u64> {
    if fee_bps == 0 || amount == 0 {
        return Some(0);
    }
    let amount = amount as u128;
    let bps = fee_bps as u128;
    let fee = amount.checked_mul(bps)? / 10_000;
    u64::try_from(fee).ok()
}

/// Split a protocol fee into (referrer_reward, trader_rebate, protocol_keeps).
///
/// Uses floor rounding so `referrer_reward + trader_rebate <= protocol_fee` always.
/// Protocol keeps the dust.
///
/// Returns `None` on overflow.
pub fn split_referral(
    protocol_fee: u64,
    reward_bps: u16,
    rebate_bps: u16,
) -> Option<(u64, u64, u64)> {
    if protocol_fee == 0 || (reward_bps == 0 && rebate_bps == 0) {
        return Some((0, 0, protocol_fee));
    }
    let referrer_reward = floor_fee(protocol_fee, reward_bps)?;
    let trader_rebate = floor_fee(protocol_fee, rebate_bps)?;
    let given_away = referrer_reward.checked_add(trader_rebate)?;
    let protocol_keeps = protocol_fee.checked_sub(given_away)?;
    Some((referrer_reward, trader_rebate, protocol_keeps))
}

/// Split a gross SOL input into (treasury_fee, protocol_fee, net_amount).
///
/// treasury_fee = ceil(amount * treasury_bps / 10_000)
/// protocol_fee = ceil(amount * protocol_bps / 10_000)
/// net = amount - treasury_fee - protocol_fee
///
/// Returns `None` on overflow, when the fees exceed the amount, and for amounts below 3
/// lamports whenever any fee is positive (each ceil-rounded fee takes at least one lamport).
/// A positive net is not guaranteed: at high fee levels it can be 0, so a caller that needs a
/// positive amount checks it itself. `buy` does through the curve math, which
/// rejects a zero input; `sell` pays what its `min_sol_out` bound allows.
pub fn split_fees(
    amount: u64,
    treasury_fee_bps: u16,
    protocol_fee_bps: u16,
) -> Option<(u64, u64, u64)> {
    // Reject dust amounts below what two ceil-rounded fees can take. This alone doesn't leave a
    // positive net; see the doc comment.
    let total_bps = (treasury_fee_bps as u64).checked_add(protocol_fee_bps as u64)?;
    if total_bps > 0 && amount < 3 {
        return None;
    }
    let treasury_fee = calculate_fee(amount, treasury_fee_bps)?;
    let protocol_fee = calculate_fee(amount, protocol_fee_bps)?;
    let total_fee = treasury_fee.checked_add(protocol_fee)?;
    let net = amount.checked_sub(total_fee)?;
    Some((treasury_fee, protocol_fee, net))
}

/// Maximum gross offers `gross_for_net` will try. Covers every fee level up to 99.6%
/// (`10_000 / keep_bps + 2 <= 256`); above that it gives up instead of scanning further.
pub const GROSS_FOR_NET_MAX_SCAN: u64 = 256;

/// Smallest gross amount whose `split_fees` net is at least `net`.
///
/// A buy that would carry the curve past its graduation target is filled only up to the
/// target, and the buyer pays this gross for it.
///
/// Both fees round up, so net is not monotone in gross: one more lamport of gross can add a
/// lamport to each fee and net one lamport less. The search therefore scans upward from the
/// lower bound instead of bisecting:
/// - `net(g) <= g * keep_bps / 10_000`, so no gross below `ceil(net * 10_000 / keep_bps)` works;
/// - `net(g) > g * keep_bps / 10_000 - 2`, so the answer lies within `10_000 / keep_bps + 2`
///   of that bound.
///
/// Net changes by at most one lamport per lamport of gross, so the result nets exactly `net`,
/// except for `net < 3` with a fee on, where `split_fees`'s 3-lamport minimum can make it net
/// one lamport more.
///
/// Returns `None` when `net == 0`, when the fees take everything (`keep_bps == 0`), when the
/// scan would exceed `GROSS_FOR_NET_MAX_SCAN` (fees above 99.6%), or on overflow.
pub fn gross_for_net(net: u64, treasury_fee_bps: u16, protocol_fee_bps: u16) -> Option<u64> {
    if net == 0 {
        return None;
    }
    let total_bps = (treasury_fee_bps as u64).checked_add(protocol_fee_bps as u64)?;
    let keep_bps = 10_000u64.checked_sub(total_bps).filter(|&k| k > 0)?;

    let lower = (net as u128)
        .checked_mul(10_000)?
        .checked_add(keep_bps as u128 - 1)?
        / keep_bps as u128;
    let window = (10_000 / keep_bps).checked_add(2)?;
    if window > GROSS_FOR_NET_MAX_SCAN {
        return None;
    }

    let mut gross = u64::try_from(lower).ok()?;
    for _ in 0..=window {
        if let Some((_, _, got)) = split_fees(gross, treasury_fee_bps, protocol_fee_bps) {
            if got >= net {
                return Some(gross);
            }
        }
        gross = gross.checked_add(1)?;
    }
    None
}

/// Split referrer_reward between L1 (direct) and L2 (parent) referrers.
/// `l2_bps` is the fraction of referrer_reward that goes to L2.
/// Returns (l1_reward, l2_reward). Uses floor rounding; dust stays with L1.
pub fn split_l2_reward(referrer_reward: u64, l2_bps: u16) -> Option<(u64, u64)> {
    if referrer_reward == 0 || l2_bps == 0 {
        return Some((referrer_reward, 0));
    }
    let l2_reward = floor_fee(referrer_reward, l2_bps)?;
    let l1_reward = referrer_reward.checked_sub(l2_reward)?;
    Some((l1_reward, l2_reward))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOL: u64 = 1_000_000_000;

    #[test]
    fn test_fee_basic() {
        // 2% of 1 SOL = 0.02 SOL = 20_000_000 lamports
        assert_eq!(calculate_fee(SOL, 200), Some(20_000_000));
    }

    #[test]
    fn test_fee_ceiling_rounding() {
        // 1.5% of 1 lamport: ceil(1 * 150 / 10000) = ceil(0.015) = 1
        assert_eq!(calculate_fee(1, 150), Some(1));
        // 0.5% of 1 lamport: ceil(1 * 50 / 10000) = ceil(0.005) = 1
        assert_eq!(calculate_fee(1, 50), Some(1));
    }

    #[test]
    fn test_fee_zero_amount() {
        assert_eq!(calculate_fee(0, 200), Some(0));
    }

    #[test]
    fn test_fee_zero_bps() {
        assert_eq!(calculate_fee(SOL, 0), Some(0));
    }

    #[test]
    fn test_split_fees() {
        let amount = 100 * SOL; // 100 SOL
        let (treasury, protocol, net) = split_fees(amount, 150, 50).unwrap();
        // treasury = 1.5% of 100 SOL = 1.5 SOL
        assert_eq!(treasury, 1_500_000_000);
        // protocol = 0.5% of 100 SOL = 0.5 SOL
        assert_eq!(protocol, 500_000_000);
        // net = 98 SOL
        assert_eq!(net, 98_000_000_000);
        // Verify they sum correctly
        assert_eq!(treasury + protocol + net, amount);
    }

    #[test]
    fn test_split_fees_small_amount() {
        // 100 lamports
        let (treasury, protocol, net) = split_fees(100, 150, 50).unwrap();
        // treasury = ceil(100 * 150 / 10000) = ceil(1.5) = 2
        assert_eq!(treasury, 2);
        // protocol = ceil(100 * 50 / 10000) = ceil(0.5) = 1
        assert_eq!(protocol, 1);
        // net = 100 - 2 - 1 = 97
        assert_eq!(net, 97);
    }

    #[test]
    fn test_split_fees_rounding_favours_protocol() {
        // With rounding up, total fees >= exact percentage
        // This means the user pays slightly more, protocol keeps slightly more.
        // Very small amounts (1, 3) may have fees exceeding the amount — split_fees
        // returns None for those, which is correct (rejects dust amounts).
        for amount in [7u64, 99, 101, 999, 10_001, SOL] {
            let (t, p, n) = split_fees(amount, 150, 50).unwrap();
            assert_eq!(t + p + n, amount, "fees must sum to original amount");
            // Verify fees are at least the floor
            let exact_t = (amount as u128 * 150) / 10_000;
            let exact_p = (amount as u128 * 50) / 10_000;
            assert!(t as u128 >= exact_t);
            assert!(p as u128 >= exact_p);
        }
    }

    #[test]
    fn test_split_fees_dust_rejected() {
        // Amount=1: ceil(1*150/10000)=1, ceil(1*50/10000)=1 → total fees=2 > 1 → None
        assert!(split_fees(1, 150, 50).is_none());
        // Amount=3: ceil(3*150/10000)=1, ceil(3*50/10000)=1 → total fees=2, net=1 → valid
        assert!(split_fees(3, 150, 50).is_some());
    }

    #[test]
    fn test_fee_large_value() {
        // Max realistic: 2000 SOL
        let amount = 2_000 * SOL;
        let fee = calculate_fee(amount, 200).unwrap();
        assert_eq!(fee, 40 * SOL); // 2% of 2000 = 40 SOL
    }

    // ── floor_fee tests ──

    #[test]
    fn test_floor_fee_basic() {
        // 60% of 500_000_000 (0.5 SOL) = 300_000_000
        assert_eq!(floor_fee(500_000_000, 6000), Some(300_000_000));
    }

    #[test]
    fn test_floor_fee_rounds_down() {
        // floor(1 * 6000 / 10000) = floor(0.6) = 0
        assert_eq!(floor_fee(1, 6000), Some(0));
        // floor(3 * 6000 / 10000) = floor(1.8) = 1
        assert_eq!(floor_fee(3, 6000), Some(1));
    }

    #[test]
    fn test_floor_fee_zero() {
        assert_eq!(floor_fee(0, 6000), Some(0));
        assert_eq!(floor_fee(SOL, 0), Some(0));
    }

    // ── split_referral tests ──

    #[test]
    fn test_split_referral_standard() {
        // 0.5% of 10 SOL = 0.05 SOL = 50_000_000 protocol fee
        let protocol_fee = 50_000_000u64;
        let (reward, rebate, keeps) = split_referral(protocol_fee, 6000, 2000).unwrap();
        // 60% = 30_000_000, 20% = 10_000_000, protocol keeps 20% = 10_000_000
        assert_eq!(reward, 30_000_000);
        assert_eq!(rebate, 10_000_000);
        assert_eq!(keeps, 10_000_000);
        assert_eq!(reward + rebate + keeps, protocol_fee);
    }

    #[test]
    fn test_split_referral_zero_fee() {
        let (r, t, p) = split_referral(0, 6000, 2000).unwrap();
        assert_eq!((r, t, p), (0, 0, 0));
    }

    #[test]
    fn test_split_referral_zero_bps() {
        let (r, t, p) = split_referral(1000, 0, 0).unwrap();
        assert_eq!((r, t, p), (0, 0, 1000));
    }

    #[test]
    fn test_split_referral_dust_kept_by_protocol() {
        // Small fee where floor rounding leaves dust
        // protocol_fee = 7, reward_bps = 6000, rebate_bps = 2000
        // reward = floor(7 * 6000 / 10000) = floor(4.2) = 4
        // rebate = floor(7 * 2000 / 10000) = floor(1.4) = 1
        // keeps = 7 - 4 - 1 = 2  (> exact 20% of 1.4)
        let (reward, rebate, keeps) = split_referral(7, 6000, 2000).unwrap();
        assert_eq!(reward, 4);
        assert_eq!(rebate, 1);
        assert_eq!(keeps, 2);
        assert_eq!(reward + rebate + keeps, 7);
    }

    #[test]
    fn test_split_referral_sums_correctly() {
        // Test across many amounts that the sum always equals protocol_fee
        for fee in [1u64, 2, 3, 5, 10, 99, 100, 999, 10_000, SOL, 100 * SOL] {
            let (r, t, p) = split_referral(fee, 6000, 2000).unwrap();
            assert_eq!(r + t + p, fee, "split must sum for fee={fee}");
            // Floor rounding: referrer+rebate never exceeds protocol_fee
            assert!(r + t <= fee);
        }
    }

    // ── split_l2_reward tests ──

    #[test]
    fn test_split_l2_basic() {
        // 25% of 300_000_000 (0.3 SOL referrer reward) = 75_000_000 to L2
        let (l1, l2) = split_l2_reward(300_000_000, 2500).unwrap();
        assert_eq!(l2, 75_000_000);
        assert_eq!(l1, 225_000_000);
        assert_eq!(l1 + l2, 300_000_000);
    }

    #[test]
    fn test_split_l2_zero_bps() {
        let (l1, l2) = split_l2_reward(300_000_000, 0).unwrap();
        assert_eq!(l1, 300_000_000);
        assert_eq!(l2, 0);
    }

    #[test]
    fn test_split_l2_zero_reward() {
        let (l1, l2) = split_l2_reward(0, 2500).unwrap();
        assert_eq!(l1, 0);
        assert_eq!(l2, 0);
    }

    #[test]
    fn test_split_l2_full_to_l2() {
        // 100% to L2
        let (l1, l2) = split_l2_reward(100, 10_000).unwrap();
        assert_eq!(l1, 0);
        assert_eq!(l2, 100);
    }

    #[test]
    fn test_fee_100_percent() {
        // 100% fee = 10000 bps
        assert_eq!(calculate_fee(SOL, 10_000), Some(SOL));
        assert_eq!(calculate_fee(1, 10_000), Some(1));
    }

    #[test]
    fn test_fee_tiny_bps() {
        // 1 bps = 0.01% of 1 lamport → ceil(1*1/10000) = ceil(0.0001) = 1
        assert_eq!(calculate_fee(1, 1), Some(1));
        // 1 bps of 10000 lamports → ceil(10000*1/10000) = 1
        assert_eq!(calculate_fee(10_000, 1), Some(1));
    }

    #[test]
    fn test_split_referral_full_split() {
        // reward + rebate = 10000 (100%), protocol keeps nothing
        let (r, t, p) = split_referral(1000, 8000, 2000).unwrap();
        assert_eq!(r, 800);
        assert_eq!(t, 200);
        assert_eq!(p, 0);
    }

    // ── gross_for_net tests ──

    /// Brute force: the smallest gross >= 1 with `split_fees` net >= `net`.
    fn smallest_gross_brute(net: u64, t: u16, p: u16, from: u64) -> u64 {
        let mut g = from;
        loop {
            if let Some((_, _, n)) = split_fees(g, t, p) {
                if n >= net {
                    return g;
                }
            }
            g += 1;
        }
    }

    const FEE_LEVELS: [(u16, u16); 9] = [
        (0, 0),
        (1, 0),
        (0, 1),
        (400, 100),
        (500, 100),
        (1_000, 0),
        (3_333, 3_333),
        (9_000, 500),
        (9_899, 1),
    ];

    #[test]
    fn test_gross_for_net_is_smallest_for_small_nets() {
        for (t, p) in FEE_LEVELS {
            // From gross 1 for the smallest nets, which also checks the lower bound itself.
            for net in 1..=300u64 {
                let g = gross_for_net(net, t, p).unwrap();
                assert_eq!(
                    g,
                    smallest_gross_brute(net, t, p, 1),
                    "net={net} t={t} p={p}"
                );
            }
            let keep = 10_000 - t as u64 - p as u64;
            for net in 301..=3_000u64 {
                let floor = net * 10_000 / keep;
                let g = gross_for_net(net, t, p).unwrap();
                assert_eq!(
                    g,
                    smallest_gross_brute(net, t, p, floor - 2),
                    "net={net} t={t} p={p}"
                );
            }
        }
    }

    #[test]
    fn test_gross_for_net_is_smallest_for_large_nets() {
        // Below `ceil(net * 10_000 / keep)` nothing nets enough, so brute force from there.
        for (t, p) in FEE_LEVELS {
            let keep = 10_000 - t as u64 - p as u64;
            for net in [
                SOL - 1,
                SOL,
                85 * SOL - 1,
                85 * SOL,
                85 * SOL + 7,
                500_000_000,
                1_000 * SOL + 3,
                u64::MAX / 20_000,
            ] {
                let floor = ((net as u128 * 10_000) / keep as u128) as u64;
                let g = gross_for_net(net, t, p).unwrap();
                assert_eq!(
                    g,
                    smallest_gross_brute(net, t, p, floor.saturating_sub(2).max(1)),
                    "net={net} t={t} p={p}"
                );
            }
        }
    }

    #[test]
    fn test_gross_for_net_nets_exactly() {
        // Minimal gross nets exactly `net`, so a capped buy lands reserve_sol on the target. The
        // only exception is the 3-lamport dust floor of split_fees.
        for (t, p) in FEE_LEVELS {
            for net in (1..=2_000u64).chain([SOL, 85 * SOL, 85 * SOL + 1, 1_000 * SOL + 3]) {
                let g = gross_for_net(net, t, p).unwrap();
                let (tf, pf, n) = split_fees(g, t, p).unwrap();
                assert_eq!(tf + pf + n, g);
                if net >= 3 || t as u32 + p as u32 == 0 {
                    assert_eq!(n, net, "net={net} t={t} p={p}");
                } else {
                    assert!(n >= net && n <= 3, "net={net} t={t} p={p} got {n}");
                }
            }
        }
    }

    #[test]
    fn test_gross_for_net_never_exceeds_a_gross_that_already_nets_enough() {
        // What buy.rs relies on: when the offered gross nets more than the remaining room, the
        // capped gross is at most the offer, so the buyer is never charged more than they
        // offered. (It can equal the offer at split_fees's 3-lamport floor.)
        for (t, p) in FEE_LEVELS {
            for offered in [3u64, 10, 999, 10_001, SOL, 150 * SOL] {
                let Some((_, _, offered_net)) = split_fees(offered, t, p) else {
                    continue;
                };
                for room in [1u64, 2, offered_net / 2, offered_net.saturating_sub(1)] {
                    if room == 0 || room >= offered_net {
                        continue;
                    }
                    let g = gross_for_net(room, t, p).unwrap();
                    assert!(g <= offered, "room={room} offered={offered} t={t} p={p}");
                }
            }
        }
    }

    #[test]
    fn test_gross_for_net_rejects_what_it_cannot_serve() {
        assert_eq!(gross_for_net(0, 400, 100), None);
        // Fees take everything.
        assert_eq!(gross_for_net(SOL, 10_000, 0), None);
        assert_eq!(gross_for_net(SOL, 9_000, 1_000), None);
        // Above 99.6% the scan window exceeds GROSS_FOR_NET_MAX_SCAN.
        assert_eq!(gross_for_net(SOL, 9_970, 0), None);
        assert!(gross_for_net(SOL, 9_960, 0).is_some());
        // No gross fits in a u64.
        assert_eq!(gross_for_net(u64::MAX, 400, 100), None);
    }

    #[test]
    fn test_split_l2_sums_correctly() {
        for reward in [1u64, 3, 7, 99, 1000, SOL, 100 * SOL] {
            let (l1, l2) = split_l2_reward(reward, 2500).unwrap();
            assert_eq!(l1 + l2, reward, "L2 split must sum for reward={reward}");
        }
    }
}
