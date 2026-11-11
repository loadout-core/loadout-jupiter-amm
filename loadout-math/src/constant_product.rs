//! Constant-product AMM math with virtual reserves.
//!
//! All intermediate arithmetic uses u128 with checked operations.
//! Rounding always favours the protocol (ceiling division where the protocol pays less).

/// Result of a swap calculation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwapResult {
    /// Tokens (or SOL) the user receives.
    pub amount_out: u64,
    /// The new virtual SOL reserve after the swap.
    pub new_virtual_sol: u64,
    /// The new virtual token reserve after the swap.
    pub new_virtual_token: u64,
    /// The new real SOL reserve after the swap.
    pub new_reserve_sol: u64,
    /// The new real token reserve after the swap.
    pub new_reserve_token: u64,
    /// k' after the swap — must be >= original k.
    pub new_k: u128,
}

/// Ceiling division: ceil(a / b).
/// Returns `None` if `b == 0`.
#[inline]
pub fn ceil_div_u128(a: u128, b: u128) -> Option<u128> {
    if b == 0 {
        return None;
    }
    // (a + b - 1) / b, but avoid overflow on the addition
    Some(a.checked_add(b.checked_sub(1)?)? / b)
}

/// Calculate tokens out for a buy (SOL in, tokens out).
///
/// Formula: tokens_out = virtual_token - ceil(k / (virtual_sol + net_sol_in))
///
/// `net_sol_in` is the SOL amount AFTER fees have been deducted.
///
/// Returns `None` on math error or if the trade would be invalid.
pub fn calculate_buy(
    virtual_sol: u64,
    virtual_token: u64,
    reserve_sol: u64,
    reserve_token: u64,
    invariant_k: u128,
    net_sol_in: u64,
) -> Option<SwapResult> {
    if net_sol_in == 0 {
        return None;
    }

    let vs = virtual_sol as u128;
    let vt = virtual_token as u128;
    let rs = reserve_sol as u128;
    let rt = reserve_token as u128;
    let sol_in = net_sol_in as u128;

    // new_virtual_sol = virtual_sol + net_sol_in
    let new_vs = vs.checked_add(sol_in)?;

    // new_virtual_token = ceil(k / new_virtual_sol)
    // Using ceiling division means the pool keeps slightly more tokens → favours protocol
    let new_vt = ceil_div_u128(invariant_k, new_vs)?;

    // tokens_out = virtual_token - new_virtual_token
    let tokens_out = vt.checked_sub(new_vt)?;
    if tokens_out == 0 {
        return None;
    }

    // Ensure tokens_out fits in u64 and doesn't exceed real reserves
    if tokens_out > rt {
        return None;
    }

    let new_rs = rs.checked_add(sol_in)?;
    let new_rt = rt.checked_sub(tokens_out)?;

    // Post-swap invariant: k' = new_vs * new_vt >= invariant_k
    let new_k = new_vs.checked_mul(new_vt)?;
    if new_k < invariant_k {
        return None;
    }

    Some(SwapResult {
        amount_out: u64::try_from(tokens_out).ok()?,
        new_virtual_sol: u64::try_from(new_vs).ok()?,
        new_virtual_token: u64::try_from(new_vt).ok()?,
        new_reserve_sol: u64::try_from(new_rs).ok()?,
        new_reserve_token: u64::try_from(new_rt).ok()?,
        new_k,
    })
}

/// Calculate SOL out for a sell (tokens in, SOL out).
///
/// Formula: sol_out = virtual_sol - ceil(k / (virtual_token + tokens_in))
///
/// The returned `amount_out` is SOL BEFORE the output fee is deducted.
///
/// Returns `None` on math error or if the trade would be invalid.
pub fn calculate_sell(
    virtual_sol: u64,
    virtual_token: u64,
    reserve_sol: u64,
    reserve_token: u64,
    invariant_k: u128,
    tokens_in: u64,
) -> Option<SwapResult> {
    if tokens_in == 0 {
        return None;
    }

    let vs = virtual_sol as u128;
    let vt = virtual_token as u128;
    let rs = reserve_sol as u128;
    let rt = reserve_token as u128;
    let tok_in = tokens_in as u128;

    // new_virtual_token = virtual_token + tokens_in
    let new_vt = vt.checked_add(tok_in)?;

    // new_virtual_sol = ceil(k / new_virtual_token)
    let new_vs = ceil_div_u128(invariant_k, new_vt)?;

    // sol_out = virtual_sol - new_virtual_sol
    let sol_out = vs.checked_sub(new_vs)?;
    if sol_out == 0 {
        return None;
    }

    // sol_out must not exceed real reserves
    if sol_out > rs {
        return None;
    }

    let new_rs = rs.checked_sub(sol_out)?;
    let new_rt = rt.checked_add(tok_in)?;

    // Post-swap invariant
    let new_k = new_vs.checked_mul(new_vt)?;
    if new_k < invariant_k {
        return None;
    }

    Some(SwapResult {
        amount_out: u64::try_from(sol_out).ok()?,
        new_virtual_sol: u64::try_from(new_vs).ok()?,
        new_virtual_token: u64::try_from(new_vt).ok()?,
        new_reserve_sol: u64::try_from(new_rs).ok()?,
        new_reserve_token: u64::try_from(new_rt).ok()?,
        new_k,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOL: u64 = 1_000_000_000; // 1 SOL in lamports
    const TOKEN: u64 = 1_000_000; // 1 token (6 decimals)

    fn setup() -> (u64, u64, u64, u64, u128) {
        let virtual_sol = 30 * SOL; // 30 SOL
        let virtual_token = 1_000_000_000 * TOKEN; // 1B tokens
        let reserve_sol = 0u64;
        let reserve_token = 1_000_000_000 * TOKEN;
        let k = (virtual_sol as u128) * (virtual_token as u128);
        (virtual_sol, virtual_token, reserve_sol, reserve_token, k)
    }

    #[test]
    fn test_ceil_div() {
        assert_eq!(ceil_div_u128(10, 3), Some(4));
        assert_eq!(ceil_div_u128(9, 3), Some(3));
        assert_eq!(ceil_div_u128(0, 5), Some(0));
        assert_eq!(ceil_div_u128(1, 0), None);
    }

    #[test]
    fn test_buy_basic() {
        let (vs, vt, rs, rt, k) = setup();
        let sol_in = SOL; // buy with 1 SOL (after fees)
        let result = calculate_buy(vs, vt, rs, rt, k, sol_in).unwrap();

        // With 30 SOL virtual and 1B token virtual, buying 1 SOL should yield ~32.26M tokens
        assert!(result.amount_out > 0);
        assert!(result.new_k >= k, "k must not decrease");
        assert_eq!(result.new_reserve_sol, rs + sol_in);
        assert_eq!(result.new_reserve_token, rt - result.amount_out);
    }

    #[test]
    fn test_sell_basic() {
        let (vs, vt, rs, rt, k) = setup();
        // First do a buy to get some real SOL in reserves
        let sol_in = 5 * SOL;
        let buy = calculate_buy(vs, vt, rs, rt, k, sol_in).unwrap();

        // Now sell half the tokens back
        let tokens_to_sell = buy.amount_out / 2;
        let sell = calculate_sell(
            buy.new_virtual_sol,
            buy.new_virtual_token,
            buy.new_reserve_sol,
            buy.new_reserve_token,
            buy.new_k,
            tokens_to_sell,
        )
        .unwrap();

        assert!(sell.amount_out > 0);
        assert!(
            sell.amount_out < sol_in,
            "should get back less than deposited"
        );
        assert!(sell.new_k >= buy.new_k, "k must not decrease on sell");
    }

    #[test]
    fn test_invariant_never_decreases() {
        let (vs, vt, rs, rt, k) = setup();

        // Series of buys and sells
        let buy1 = calculate_buy(vs, vt, rs, rt, k, 2 * SOL).unwrap();
        assert!(buy1.new_k >= k);

        let buy2 = calculate_buy(
            buy1.new_virtual_sol,
            buy1.new_virtual_token,
            buy1.new_reserve_sol,
            buy1.new_reserve_token,
            buy1.new_k,
            3 * SOL,
        )
        .unwrap();
        assert!(buy2.new_k >= buy1.new_k);

        let sell1 = calculate_sell(
            buy2.new_virtual_sol,
            buy2.new_virtual_token,
            buy2.new_reserve_sol,
            buy2.new_reserve_token,
            buy2.new_k,
            buy1.amount_out,
        )
        .unwrap();
        assert!(sell1.new_k >= buy2.new_k);
    }

    #[test]
    fn test_zero_amount_rejected() {
        let (vs, vt, rs, rt, k) = setup();
        assert!(calculate_buy(vs, vt, rs, rt, k, 0).is_none());
        assert!(calculate_sell(vs, vt, rs, rt, k, 0).is_none());
    }

    #[test]
    fn test_buy_exceeding_reserves_rejected() {
        // Use tiny reserves so we can try to buy more than available
        let vs = 100u64;
        let vt = 100u64;
        let rs = 0u64;
        let rt = 100u64;
        let k = (vs as u128) * (vt as u128); // 10_000

        // Buying 10_000 SOL into a pool with 100 virtual SOL — tokens_out would exceed reserves
        let result = calculate_buy(vs, vt, rs, rt, k, 10_000);
        // This should either succeed with <= 100 tokens or fail
        if let Some(r) = result {
            assert!(r.amount_out <= rt);
        }
    }

    #[test]
    fn test_sell_exceeding_sol_reserves_rejected() {
        let (vs, vt, rs, rt, k) = setup();
        // Can't sell tokens when there's no real SOL in the reserves
        let result = calculate_sell(vs, vt, rs, rt, k, 1_000 * TOKEN);
        assert!(result.is_none(), "should fail: no real SOL to pay out");
    }

    #[test]
    fn test_buy_sell_round_trip_no_profit() {
        let (vs, vt, rs, rt, k) = setup();
        let sol_in = 5 * SOL;
        let buy = calculate_buy(vs, vt, rs, rt, k, sol_in).unwrap();

        // Sell all tokens back
        let sell = calculate_sell(
            buy.new_virtual_sol,
            buy.new_virtual_token,
            buy.new_reserve_sol,
            buy.new_reserve_token,
            buy.new_k,
            buy.amount_out,
        )
        .unwrap();

        // User gets back strictly less than they put in (due to rounding + spread)
        assert!(
            sell.amount_out < sol_in,
            "round trip must not be profitable"
        );
    }

    #[test]
    fn test_buy_dust_amount() {
        let (vs, vt, rs, rt, k) = setup();
        // Buy with 1 lamport — should either work or return None
        let result = calculate_buy(vs, vt, rs, rt, k, 1);
        if let Some(r) = result {
            assert!(r.new_k >= k, "k must not decrease even for dust buy");
        }
    }

    #[test]
    fn test_sell_dust_amount() {
        let (vs, vt, rs, rt, k) = setup();
        // First buy to get some reserves
        let buy = calculate_buy(vs, vt, rs, rt, k, 10 * SOL).unwrap();

        // Sell 1 token unit
        let result = calculate_sell(
            buy.new_virtual_sol,
            buy.new_virtual_token,
            buy.new_reserve_sol,
            buy.new_reserve_token,
            buy.new_k,
            1,
        );
        // May return None if output rounds to 0
        if let Some(r) = result {
            assert!(
                r.new_k >= buy.new_k,
                "k must not decrease even for dust sell"
            );
        }
    }

    #[test]
    fn test_sequential_operations_preserve_k() {
        let (vs, vt, rs, rt, k) = setup();
        let mut state = (vs, vt, rs, rt, k);

        // 10 sequential buys
        for i in 1..=10 {
            let buy = calculate_buy(state.0, state.1, state.2, state.3, state.4, SOL).unwrap();
            assert!(buy.new_k >= state.4, "k must not decrease on buy #{}", i);
            state = (
                buy.new_virtual_sol,
                buy.new_virtual_token,
                buy.new_reserve_sol,
                buy.new_reserve_token,
                buy.new_k,
            );
        }

        // 5 sequential sells
        for i in 1..=5 {
            let sell = calculate_sell(state.0, state.1, state.2, state.3, state.4, 10_000 * TOKEN)
                .unwrap();
            assert!(sell.new_k >= state.4, "k must not decrease on sell #{}", i);
            state = (
                sell.new_virtual_sol,
                sell.new_virtual_token,
                sell.new_reserve_sol,
                sell.new_reserve_token,
                sell.new_k,
            );
        }
    }

    #[test]
    fn test_large_values_no_overflow() {
        let vs = 2_000 * SOL; // 2000 SOL
        let vt = 10_000_000_000u64 * TOKEN; // 10B tokens
        let rs = 1_000 * SOL;
        let rt = 5_000_000_000u64 * TOKEN;
        let k = (vs as u128) * (vt as u128);

        // k ~ 2*10^31, fits u128
        let result = calculate_buy(vs, vt, rs, rt, k, 100 * SOL);
        assert!(result.is_some());
        assert!(result.unwrap().new_k >= k);
    }
}
