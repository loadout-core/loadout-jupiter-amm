//! Pure curve-transition helpers. No account types, RPC, allocation or Anchor dependency.
use crate::{
    constant_product::{calculate_buy, SwapResult},
    fees,
};

/// Whether the remaining SOL gap can still buy at least one token unit.
pub fn remainder_fillable(
    after: &SwapResult,
    target: u64,
    treasury_bps: u16,
    protocol_bps: u16,
) -> bool {
    if after.new_reserve_sol >= target {
        return true;
    }
    let Some(remaining) = target.checked_sub(after.new_reserve_sol) else {
        return false;
    };
    let can_fill = |net| {
        calculate_buy(
            after.new_virtual_sol,
            after.new_virtual_token,
            after.new_reserve_sol,
            after.new_reserve_token,
            after.new_k,
            net,
        )
        .is_some()
    };
    can_fill(remaining)
        || fees::gross_for_net(remaining, treasury_bps, protocol_bps)
            .and_then(|gross| fees::split_fees(gross, treasury_bps, protocol_bps))
            .is_some_and(|(_, _, net)| can_fill(net))
}

/// Protocol share of the protected-launch charge above the normal treasury charge.
pub fn sniper_protocol_part(
    gross: u64,
    treasury_fee: u64,
    normal_bps: u16,
    share_bps: u16,
) -> Option<u64> {
    if share_bps == 0 {
        return Some(0);
    }
    let normal = fees::calculate_fee(gross, normal_bps)?;
    fees::floor_fee(treasury_fee.saturating_sub(normal), share_bps)
}

/// A fee-funded buy after the trader's swap. `None` leaves the share held in the treasury.
pub fn inline_buyback(
    after: &SwapResult,
    due: u64,
    target: u64,
    treasury_bps: u16,
    protocol_bps: u16,
) -> Option<(u64, SwapResult)> {
    let spend = due.min(target.checked_sub(after.new_reserve_sol)?);
    if spend == 0 {
        return None;
    }
    let bought = calculate_buy(
        after.new_virtual_sol,
        after.new_virtual_token,
        after.new_reserve_sol,
        after.new_reserve_token,
        after.new_k,
        spend,
    )?;
    remainder_fillable(&bought, target, treasury_bps, protocol_bps).then_some((spend, bought))
}
