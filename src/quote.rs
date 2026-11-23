//! What `loadout-core`'s `buy` and `sell` charge and pay, computed off-chain.
//!
//! Each function reproduces its instruction's checks and arithmetic in the program's order
//! (account constraints first, then the handler), so a quote fails exactly when the instruction
//! would, with the instruction's error. Trades through Jupiter carry no referrer, so the
//! referral split never applies: the fee is the treasury fee plus the protocol fee.

use loadout_math::{
    constant_product::{self, SwapResult},
    fees,
};

use crate::QuoteError;
use crate::state::{BondingCurve, FeeConfig, PROJECT_STATE_LIVE, Project};

/// `DEFAULT_CURVE_ESCAPE_DELAY_SECONDS` in the mainnet build of `loadout-core`: how long sells
/// stay closed on a completed curve when `FeeConfig.curve_escape_delay_seconds` is 0.
pub const DEFAULT_CURVE_ESCAPE_DELAY_SECONDS: u32 = 24 * 60 * 60;

/// The state a quote reads: the curve, its project and the protocol's fee config.
#[derive(Clone, Copy, Debug)]
pub struct Market {
    pub curve: BondingCurve,
    pub project: Project,
    pub fee_config: FeeConfig,
}

/// A trade's outcome: what the trader pays in, receives, and the fees taken (in lamports).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fill {
    pub in_amount: u64,
    pub out_amount: u64,
    pub treasury_fee: u64,
    pub protocol_fee: u64,
    pub after: SwapResult,
}

impl Fill {
    pub fn fee(&self) -> u64 {
        self.treasury_fee + self.protocol_fee
    }
}

/// `buy(sol_amount)` at unix time `now`: tokens out for `sol_amount` lamports. A buy that would
/// carry the curve past its target is filled only up to it and charged only the gross that
/// buys that much, so `in_amount` can be less than `sol_amount`.
pub fn buy(market: &Market, sol_amount: u64, now: i64) -> Result<Fill, QuoteError> {
    let Market { curve, project, .. } = market;
    check_accounts(market)?;
    if curve.reserve_sol >= project.treasury_target {
        return Err(QuoteError::CurveComplete);
    }
    if sol_amount == 0 {
        return Err(QuoteError::ZeroAmount);
    }

    let (normal_bps, protocol_bps, sniper_share) = effective_fees(market);
    let treasury_bps = protected_launch_treasury_bps(project, now, normal_bps)?;
    // The deployed program rejects this before fee splitting or curve math.
    if u32::from(treasury_bps) + u32::from(protocol_bps) >= 10_000 {
        return Err(QuoteError::TradeFeeTooHigh);
    }
    let (mut treasury_fee, mut protocol_fee, mut net_sol_in) =
        split_fees(sol_amount, treasury_bps, protocol_bps)?;
    let mut charged = sol_amount;

    let room = project
        .treasury_target
        .checked_sub(curve.reserve_sol)
        .ok_or(QuoteError::MathOverflow)?;
    if net_sol_in > room {
        let capped = fees::gross_for_net(room, treasury_bps, protocol_bps)
            .ok_or(QuoteError::FeeCalculationFailed)?;
        if capped > sol_amount {
            return Err(QuoteError::InvariantViolation);
        }
        (treasury_fee, protocol_fee, net_sol_in) = split_fees(capped, treasury_bps, protocol_bps)?;
        charged = capped;
    }

    let swap = constant_product::calculate_buy(
        curve.virtual_sol,
        curve.virtual_token,
        curve.reserve_sol,
        curve.reserve_token,
        curve.invariant_k,
        net_sol_in,
    )
    .ok_or(QuoteError::MathOverflow)?;
    if swap.new_k < curve.invariant_k {
        return Err(QuoteError::InvariantViolation);
    }
    if !remainder_fillable(&swap, project.treasury_target, treasury_bps, protocol_bps) {
        return Err(QuoteError::CurveCompletionUnfillable);
    }
    if sniper_share != 0 {
        let normal =
            fees::calculate_fee(charged, normal_bps).ok_or(QuoteError::FeeCalculationFailed)?;
        let sniper_fee = fees::floor_fee(treasury_fee.saturating_sub(normal), sniper_share)
            .ok_or(QuoteError::FeeCalculationFailed)?;
        treasury_fee = treasury_fee
            .checked_sub(sniper_fee)
            .ok_or(QuoteError::MathOverflow)?;
        protocol_fee = protocol_fee
            .checked_add(sniper_fee)
            .ok_or(QuoteError::MathOverflow)?;
    }

    Ok(Fill {
        in_amount: charged,
        out_amount: swap.amount_out,
        treasury_fee,
        protocol_fee,
        after: swap,
    })
}

/// `sell(token_amount)` at unix time `now`: lamports out, after fees, for `token_amount` tokens.
/// Sells pay the normal treasury fee: the protected-launch fee applies to buys only.
pub fn sell(market: &Market, token_amount: u64, now: i64) -> Result<Fill, QuoteError> {
    let curve = &market.curve;
    check_accounts(market)?;
    if token_amount == 0 {
        return Err(QuoteError::ZeroAmount);
    }
    if !sells_open(market, now)? {
        return Err(QuoteError::CurveComplete);
    }

    let swap = constant_product::calculate_sell(
        curve.virtual_sol,
        curve.virtual_token,
        curve.reserve_sol,
        curve.reserve_token,
        curve.invariant_k,
        token_amount,
    )
    .ok_or(QuoteError::MathOverflow)?;
    let (treasury_bps, protocol_bps, _) = effective_fees(market);
    let (treasury_fee, protocol_fee, net_sol_out) =
        split_fees(swap.amount_out, treasury_bps, protocol_bps)?;
    if swap.new_k < curve.invariant_k {
        return Err(QuoteError::InvariantViolation);
    }

    Ok(Fill {
        in_amount: token_amount,
        out_amount: net_sol_out,
        treasury_fee,
        protocol_fee,
        after: swap,
    })
}

fn effective_fees(market: &Market) -> (u16, u16, u16) {
    let curve = &market.curve;
    if curve.fee_snapshot != 0 {
        (
            curve.treasury_fee_bps,
            curve.protocol_fee_bps,
            curve.sniper_share_bps,
        )
    } else {
        let config = &market.fee_config;
        (
            config.treasury_fee_bps,
            config.protocol_fee_bps,
            config.sniper_share_bps,
        )
    }
}

// Nominated refuses a buy if the remaining SOL gap cannot buy one token unit.
fn remainder_fillable(
    after: &SwapResult,
    target: u64,
    treasury_bps: u16,
    protocol_bps: u16,
) -> bool {
    if after.new_reserve_sol >= target {
        return true;
    }
    let remaining = target - after.new_reserve_sol;
    let can_fill = |net| {
        constant_product::calculate_buy(
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

/// The constraints `buy` and `sell` share on their accounts, in their declaration order.
fn check_accounts(market: &Market) -> Result<(), QuoteError> {
    let Market {
        curve,
        project,
        fee_config,
    } = market;
    if fee_config.paused {
        return Err(QuoteError::ProtocolPaused);
    }
    if project.state != PROJECT_STATE_LIVE {
        return Err(QuoteError::ProjectNotLive);
    }
    if project.paused {
        return Err(QuoteError::ProjectPaused);
    }
    if !curve.is_active {
        return Err(QuoteError::MarketNotActive);
    }
    if curve.project_id != project.project_id {
        return Err(QuoteError::InvalidAccount(
            "curve and project differ".to_string(),
        ));
    }
    Ok(())
}

/// `check_sell_open`: a completed curve takes no sells until its escape delay has passed
/// A curve that completed before `completed_at` existed (0) is treated as long past.
pub fn sells_open(market: &Market, now: i64) -> Result<bool, QuoteError> {
    let Market {
        curve,
        project,
        fee_config,
    } = market;
    if curve.reserve_sol < project.treasury_target || curve.completed_at <= 0 {
        return Ok(true);
    }
    let delay = match fee_config.curve_escape_delay_seconds {
        0 => DEFAULT_CURVE_ESCAPE_DELAY_SECONDS,
        configured => configured,
    };
    let opens_at = curve
        .completed_at
        .checked_add(i64::from(delay))
        .ok_or(QuoteError::MathOverflow)?;
    Ok(now >= opens_at)
}

/// `Project::protected_launch_treasury_bps`: during the protected-launch window the treasury
/// fee falls linearly from `protected_launch_fee_bps` to the normal fee.
pub fn protected_launch_treasury_bps(
    project: &Project,
    now: i64,
    normal_bps: u16,
) -> Result<u16, QuoteError> {
    let duration = project.protected_launch_duration;
    let window_active = duration > 0
        && project.launched_at_ts > 0
        && project
            .launched_at_ts
            .checked_add(i64::from(duration))
            .is_some_and(|end| now < end);
    if !window_active {
        return Ok(normal_bps);
    }
    let elapsed_i64 = now.saturating_sub(project.launched_at_ts).max(0);
    let elapsed = u32::try_from(elapsed_i64).unwrap_or(duration).min(duration);
    let start_bps = i64::from(project.protected_launch_fee_bps);
    let delta = (i64::from(normal_bps) - start_bps)
        .checked_mul(i64::from(elapsed))
        .and_then(|v| v.checked_div(i64::from(duration)))
        .ok_or(QuoteError::MathOverflow)?;
    let bps = start_bps
        .checked_add(delta)
        .ok_or(QuoteError::MathOverflow)?;
    u16::try_from(bps).map_err(|_| QuoteError::MathOverflow)
}

fn split_fees(
    amount: u64,
    treasury_bps: u16,
    protocol_bps: u16,
) -> Result<(u64, u64, u64), QuoteError> {
    fees::split_fees(amount, treasury_bps, protocol_bps).ok_or(QuoteError::FeeCalculationFailed)
}
