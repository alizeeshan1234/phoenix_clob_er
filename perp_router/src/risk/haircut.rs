//! Percolator H invariant — withdrawal haircut.
//!
//! Pool can never pay out more than its real balance sheet supports:
//! ```text
//!     Residual = max(0, V − C_tot − I)
//!     h        = min(Residual, total_matured_PnL) / total_matured_PnL
//!     payout   = withdrawal × h
//! ```
//! Collateral is senior (always 100%); only matured PnL is haircut.

use crate::error::PerpRouterError;

/// Compute the haircut as a `(num, den)` ratio. Stored as a ratio (not a
/// pre-divided fraction) so the receipt can carry exact audit values and
/// downstream `apply_h` keeps full precision.
///
/// When `total_matured_pnl == 0` the haircut is undefined; we return
/// `(1, 1)` (no haircut) since there is no junior liability to scale.
pub fn compute_h(
    v_total_pool_value: u64,
    c_total_collateral: u64,
    i_insurance_reserve: u64,
    total_matured_pnl: u64,
) -> (u64, u64) {
    let residual = v_total_pool_value
        .saturating_sub(c_total_collateral)
        .saturating_sub(i_insurance_reserve);

    if total_matured_pnl == 0 {
        return (1, 1);
    }
    let num = residual.min(total_matured_pnl);
    let den = total_matured_pnl;
    (num, den)
}

/// Apply the haircut ratio to an amount. Uses `u128` to avoid overflow.
pub fn apply_h(amount: u64, num: u64, den: u64) -> Result<u64, PerpRouterError> {
    if den == 0 {
        return Err(PerpRouterError::HaircutOverflow);
    }
    let scaled = (amount as u128)
        .checked_mul(num as u128)
        .ok_or(PerpRouterError::HaircutOverflow)?
        / (den as u128);
    if scaled > u64::MAX as u128 {
        return Err(PerpRouterError::HaircutOverflow);
    }
    Ok(scaled as u64)
}

/// Splits a withdrawal request into the senior (collateral, always full) and
/// junior (matured PnL, haircut-scaled) components. Returns
/// `(collateral_payout, pnl_payout, total_payout)`.
pub fn split_withdrawal(
    collateral_request: u64,
    matured_pnl_request: u64,
    h_num: u64,
    h_den: u64,
) -> Result<(u64, u64, u64), PerpRouterError> {
    let collateral_payout = collateral_request;
    let pnl_payout = apply_h(matured_pnl_request, h_num, h_den)?;
    let total = collateral_payout
        .checked_add(pnl_payout)
        .ok_or(PerpRouterError::HaircutOverflow)?;
    Ok((collateral_payout, pnl_payout, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_pool_returns_full_payout() {
        // V=10M, C=8M, I=200k, PnL_owed=1M → Residual=1.8M, capped at 1M.
        // h = 1M / 1M = 1.0
        let (n, d) = compute_h(10_000_000, 8_000_000, 200_000, 1_000_000);
        assert_eq!(n, 1_000_000);
        assert_eq!(d, 1_000_000);
        assert_eq!(apply_h(100_000, n, d).unwrap(), 100_000);
    }

    #[test]
    fn shortfall_pool_scales_payout() {
        // V=10M, C=2M, I=200k, PnL_owed=8M → Residual=7.8M.
        // h = 7.8M / 8M = 0.975
        let (n, d) = compute_h(10_000_000, 2_000_000, 200_000, 8_000_000);
        assert_eq!(n, 7_800_000);
        assert_eq!(d, 8_000_000);
        assert_eq!(apply_h(100_000, n, d).unwrap(), 97_500);
    }

    #[test]
    fn empty_pnl_no_haircut() {
        // total_matured_PnL = 0 → h = 1.0 (collateral-only withdrawal)
        let (n, d) = compute_h(10_000_000, 8_000_000, 200_000, 0);
        assert_eq!((n, d), (1, 1));
        assert_eq!(apply_h(50_000, n, d).unwrap(), 50_000);
    }

    #[test]
    fn underwater_pool_zero_pnl_payout() {
        // V < C + I → Residual = 0 → h = 0
        let (n, d) = compute_h(1_000_000, 5_000_000, 200_000, 1_000_000);
        assert_eq!(n, 0);
        assert_eq!(d, 1_000_000);
        assert_eq!(apply_h(100_000, n, d).unwrap(), 0);
    }

    #[test]
    fn split_withdrawal_senior_collateral_full_junior_pnl_haircut() {
        let (n, d) = compute_h(10_000_000, 2_000_000, 200_000, 8_000_000);
        let (c, p, total) = split_withdrawal(2_000_000, 100_000, n, d).unwrap();
        assert_eq!(c, 2_000_000); // collateral full
        assert_eq!(p, 97_500); // PnL 97.5%
        assert_eq!(total, 2_097_500);
    }

    #[test]
    fn apply_h_zero_den_errors() {
        assert!(apply_h(100, 0, 0).is_err());
    }
}
