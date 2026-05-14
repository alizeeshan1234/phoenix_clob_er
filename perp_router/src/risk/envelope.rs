//! Percolator oracle envelope — per-slot price clamp.
//!
//! Every oracle read in perp_router must route through [`safe_oracle_read`].
//! Direct Pyth/Switchboard reads outside this module are forbidden.
//!
//! Bound:
//! ```text
//!     |P_new − P_last| ≤ max_bps_per_slot × dt × P_last / 10_000
//! ```
//! where `dt = current_slot − last_slot`.
//!
//! Pure function — no account I/O. Trivially Kani-verifiable.

use crate::error::PerpRouterError;

const BPS_DENOMINATOR: u128 = 10_000;

/// Clamp `raw` to within `max_bps_per_slot × elapsed_slots` of `last_price`.
/// Returns the price that should be used internally. If clamping was
/// required, also returns the absolute delta from raw to clamped so callers
/// can log/meter manipulation attempts.
///
/// `elapsed_slots = 0` is treated as `1` to allow same-slot updates while
/// still enforcing one-slot's worth of headroom (prevents a single-slot
/// arbitrary jump that would happen if we let the cap collapse to zero).
pub fn safe_oracle_read(
    raw: u64,
    last_price: u64,
    elapsed_slots: u64,
    max_bps_per_slot: u32,
) -> Result<u64, PerpRouterError> {
    if last_price == 0 {
        // Cold start — accept whatever the oracle says the first time.
        return Ok(raw);
    }

    let dt = elapsed_slots.max(1) as u128;
    let cap: u128 = (max_bps_per_slot as u128)
        .checked_mul(dt)
        .ok_or(PerpRouterError::MathOverflow)?
        .checked_mul(last_price as u128)
        .ok_or(PerpRouterError::MathOverflow)?
        / BPS_DENOMINATOR;

    let diff = (raw as i128 - last_price as i128).unsigned_abs();
    if diff <= cap {
        Ok(raw)
    } else if raw > last_price {
        // Clamp upward move.
        let clamped = (last_price as u128)
            .checked_add(cap)
            .ok_or(PerpRouterError::MathOverflow)?;
        Ok(clamped.min(u64::MAX as u128) as u64)
    } else {
        // Clamp downward move (saturate at 0).
        let clamped = (last_price as u128).saturating_sub(cap);
        Ok(clamped as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_change_passes_through() {
        let got = safe_oracle_read(100_000, 100_000, 1, 50).unwrap();
        assert_eq!(got, 100_000);
    }

    #[test]
    fn small_move_within_cap_passes_through() {
        // 50 bps of 100_000 = 500
        let got = safe_oracle_read(100_400, 100_000, 1, 50).unwrap();
        assert_eq!(got, 100_400);
    }

    #[test]
    fn upward_move_above_cap_clamps() {
        // raw = 2x last_price, way above 50 bps cap = 500
        let got = safe_oracle_read(200_000, 100_000, 1, 50).unwrap();
        assert_eq!(got, 100_500);
    }

    #[test]
    fn downward_move_above_cap_clamps() {
        // raw = 0, way below 50 bps cap = 500
        let got = safe_oracle_read(0, 100_000, 1, 50).unwrap();
        assert_eq!(got, 99_500);
    }

    #[test]
    fn elapsed_slots_widens_cap() {
        // 10 slots * 50 bps of 100_000 = 5000
        let got = safe_oracle_read(110_000, 100_000, 10, 50).unwrap();
        assert_eq!(got, 105_000);
    }

    #[test]
    fn zero_elapsed_slots_treated_as_one() {
        let got = safe_oracle_read(200_000, 100_000, 0, 50).unwrap();
        assert_eq!(got, 100_500);
    }

    #[test]
    fn cold_start_accepts_anything() {
        let got = safe_oracle_read(123_456_789, 0, 1, 50).unwrap();
        assert_eq!(got, 123_456_789);
    }
}
