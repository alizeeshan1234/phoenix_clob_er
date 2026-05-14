//! Percolator recovery state machine.
//!
//! Transitions:
//! - `Normal`        + |A| < A_PRECISION_FLOOR        → `DrainOnly`
//! - `DrainOnly`     + open_interest == 0             → `ResetPending`
//! - `ResetPending`                                   → `Normal`
//!     (caller sets A = FIXED_POINT_ONE, snapshots K, bumps epoch)
//!
//! `open_position` rejects unless state == `Normal`. `close_position`
//! always allowed. No governance vote required for any transition.

use crate::constants::{A_PRECISION_FLOOR, FIXED_POINT_ONE};
use crate::state::global::{
    RECOVERY_DRAIN_ONLY, RECOVERY_NORMAL, RECOVERY_RESET_PENDING,
};

/// Pure transition function. Inputs:
///   - current recovery state (raw u8 from GlobalState)
///   - current A (fixed-point i128)
///   - total open interest (absolute, not net)
///
/// Returns the next recovery state. Caller is responsible for the side
/// effects of `ResetPending → Normal` (A=1, snapshot K, bump epoch).
pub fn next_state(current: u8, a: i128, open_interest: u64) -> u8 {
    let abs_a = a.unsigned_abs() as i128;
    match current {
        RECOVERY_NORMAL => {
            if abs_a < A_PRECISION_FLOOR {
                RECOVERY_DRAIN_ONLY
            } else {
                RECOVERY_NORMAL
            }
        }
        RECOVERY_DRAIN_ONLY => {
            if open_interest == 0 {
                RECOVERY_RESET_PENDING
            } else {
                RECOVERY_DRAIN_ONLY
            }
        }
        RECOVERY_RESET_PENDING => RECOVERY_NORMAL,
        // Defensive: unknown state → Normal (callers should never reach this).
        _ => RECOVERY_NORMAL,
    }
}

/// Whether `open_position` is currently allowed.
pub fn opens_allowed(recovery_state: u8) -> bool {
    recovery_state == RECOVERY_NORMAL
}

/// The deterministic reset performed when transitioning ResetPending → Normal.
/// Caller mutates GlobalState in-place.
pub struct ResetOutcome {
    pub new_a: i128,
    pub new_epoch_inc: u64,
}

pub fn reset_indices(prior_epoch: u64) -> ResetOutcome {
    ResetOutcome {
        new_a: FIXED_POINT_ONE,
        new_epoch_inc: prior_epoch.saturating_add(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::FIXED_POINT_ONE;

    #[test]
    fn normal_with_healthy_a_stays_normal() {
        assert_eq!(
            next_state(RECOVERY_NORMAL, FIXED_POINT_ONE, 100),
            RECOVERY_NORMAL
        );
    }

    #[test]
    fn normal_with_a_below_floor_goes_drain_only() {
        let bad_a = A_PRECISION_FLOOR - 1;
        assert_eq!(
            next_state(RECOVERY_NORMAL, bad_a, 100),
            RECOVERY_DRAIN_ONLY
        );
    }

    #[test]
    fn drain_only_with_oi_stays_drain_only() {
        assert_eq!(
            next_state(RECOVERY_DRAIN_ONLY, 0, 1),
            RECOVERY_DRAIN_ONLY
        );
    }

    #[test]
    fn drain_only_with_zero_oi_goes_reset_pending() {
        assert_eq!(
            next_state(RECOVERY_DRAIN_ONLY, 0, 0),
            RECOVERY_RESET_PENDING
        );
    }

    #[test]
    fn reset_pending_goes_normal() {
        assert_eq!(
            next_state(RECOVERY_RESET_PENDING, 0, 0),
            RECOVERY_NORMAL
        );
    }

    #[test]
    fn opens_only_in_normal() {
        assert!(opens_allowed(RECOVERY_NORMAL));
        assert!(!opens_allowed(RECOVERY_DRAIN_ONLY));
        assert!(!opens_allowed(RECOVERY_RESET_PENDING));
    }

    #[test]
    fn reset_bumps_a_and_epoch() {
        let r = reset_indices(7);
        assert_eq!(r.new_a, FIXED_POINT_ONE);
        assert_eq!(r.new_epoch_inc, 8);
    }
}
