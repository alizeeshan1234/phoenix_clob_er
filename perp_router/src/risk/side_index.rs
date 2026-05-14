//! Percolator A/K/F/B lazy side indices — replaces ADL.
//!
//! References Tarun Chitra, "Autodeleveraging: Impossibilities and
//! Optimization", arXiv:2512.01112, 2025.
//!
//! Layout (all fixed-point i128, 1.0 == `FIXED_POINT_ONE`):
//! - **A**: global position scale factor. effective_size = stored × A.
//! - **K**: accumulates mark price drift from successive A scalings so PnL
//!         math stays consistent.
//! - **F**: lazy funding accumulator (per-unit position).
//! - **B**: bankruptcy residual losses booked for audit.
//!
//! All operations are O(1) on GlobalState; per-account effects apply
//! lazily on next touch via [`effective_size`].

use crate::constants::FIXED_POINT_ONE;
use crate::error::PerpRouterError;

/// Compute the effective base-lot size for a position given the current
/// global A scale factor.
///
/// `effective = stored_size × (A / FIXED_POINT_ONE)`
///
/// Note: A may be < FIXED_POINT_ONE (shrunk by shortfall events) but should
/// never exceed FIXED_POINT_ONE in normal operation (positions only shrink).
pub fn effective_size(stored: i64, a: i128) -> Result<i64, PerpRouterError> {
    let scaled = (stored as i128)
        .checked_mul(a)
        .ok_or(PerpRouterError::SideIndexOverflow)?
        / FIXED_POINT_ONE;
    if scaled > i64::MAX as i128 || scaled < i64::MIN as i128 {
        return Err(PerpRouterError::SideIndexOverflow);
    }
    Ok(scaled as i64)
}

/// Funding owed by a position since its last touch:
///
/// `funding_delta = effective_size × (current_F − entry_F) / FIXED_POINT_ONE`
///
/// Positive = the position owes funding; negative = it receives.
pub fn position_funding_owed(
    effective: i64,
    current_f: i128,
    entry_f: i128,
) -> Result<i128, PerpRouterError> {
    let delta = current_f
        .checked_sub(entry_f)
        .ok_or(PerpRouterError::SideIndexOverflow)?;
    (effective as i128)
        .checked_mul(delta)
        .ok_or(PerpRouterError::SideIndexOverflow)
        .map(|v| v / FIXED_POINT_ONE)
}

/// Apply a balance-sheet shortfall by shrinking A proportionally, and book
/// the loss into B. Returns the new (A, B).
///
/// `total_oi` is the total open interest backing the book (absolute, not
/// net). `shortfall` is the unfunded loss in collateral units.
///
/// Math:
/// ```text
///     scale_factor = (total_oi − shortfall) / total_oi    in fixed-point
///     A_new        = A × scale_factor
///     B_new        = B + shortfall
/// ```
/// If `shortfall >= total_oi` the book is fully insolvent; we clamp `A` to
/// zero and force the caller into recovery.
pub fn apply_shortfall(
    a: i128,
    b: i128,
    shortfall: u64,
    total_oi: u64,
) -> Result<(i128, i128), PerpRouterError> {
    if total_oi == 0 {
        // Nothing left to scale; just book the loss.
        let b_new = b
            .checked_add(shortfall as i128)
            .ok_or(PerpRouterError::SideIndexOverflow)?;
        return Ok((0, b_new));
    }
    if shortfall >= total_oi {
        let b_new = b
            .checked_add(shortfall as i128)
            .ok_or(PerpRouterError::SideIndexOverflow)?;
        return Ok((0, b_new));
    }

    let remaining = (total_oi - shortfall) as i128;
    let scale_factor = remaining
        .checked_mul(FIXED_POINT_ONE)
        .ok_or(PerpRouterError::SideIndexOverflow)?
        / (total_oi as i128);

    let a_new = a
        .checked_mul(scale_factor)
        .ok_or(PerpRouterError::SideIndexOverflow)?
        / FIXED_POINT_ONE;
    let b_new = b
        .checked_add(shortfall as i128)
        .ok_or(PerpRouterError::SideIndexOverflow)?;
    Ok((a_new, b_new))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_size_at_unity_is_identity() {
        assert_eq!(effective_size(1_000, FIXED_POINT_ONE).unwrap(), 1_000);
        assert_eq!(effective_size(-5_000, FIXED_POINT_ONE).unwrap(), -5_000);
    }

    #[test]
    fn effective_size_scales_down() {
        // A = 0.5
        let a = FIXED_POINT_ONE / 2;
        assert_eq!(effective_size(1_000, a).unwrap(), 500);
    }

    #[test]
    fn apply_shortfall_proportional_shrink() {
        // OI = 1000, shortfall = 20 → 2% loss
        // A starts at 1.0, becomes 0.98
        let (a_new, b_new) =
            apply_shortfall(FIXED_POINT_ONE, 0, 20, 1_000).unwrap();
        assert_eq!(a_new, FIXED_POINT_ONE * 980 / 1000);
        assert_eq!(b_new, 20);
    }

    #[test]
    fn apply_shortfall_composes_with_prior_a() {
        // Two consecutive 2% shortfalls should compound: A ≈ 0.98^2 ≈ 0.9604
        let (a1, _) = apply_shortfall(FIXED_POINT_ONE, 0, 20, 1_000).unwrap();
        let (a2, _) = apply_shortfall(a1, 0, 20, 1_000).unwrap();
        // 0.9604 * 1e18 == 960_400_000_000_000_000
        assert_eq!(a2, 960_400_000_000_000_000);
    }

    #[test]
    fn apply_shortfall_total_insolvency_clamps_to_zero() {
        let (a, b) = apply_shortfall(FIXED_POINT_ONE, 0, 100, 100).unwrap();
        assert_eq!(a, 0);
        assert_eq!(b, 100);
    }

    #[test]
    fn apply_shortfall_zero_oi_books_loss_only() {
        let (a, b) =
            apply_shortfall(FIXED_POINT_ONE / 2, 50, 30, 0).unwrap();
        assert_eq!(a, 0);
        assert_eq!(b, 80);
    }

    #[test]
    fn position_funding_owed_basic() {
        // size = 100, dF = 0.01 → owed = 1
        let dt_f = FIXED_POINT_ONE / 100;
        let entry = 0i128;
        let current = entry + dt_f;
        assert_eq!(
            position_funding_owed(100, current, entry).unwrap(),
            1
        );
    }
}
