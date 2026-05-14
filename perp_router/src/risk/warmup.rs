//! Percolator PnL warmup — reserve → matured.
//!
//! Fresh PnL pushed by `close_position` lands in `TraderAccount.pnl_reserve`
//! with `mature_slot = current_slot + WARMUP_SLOTS`. The `mature_pnl` crank
//! sweeps entries whose `mature_slot` has passed into `pnl_matured`.
//!
//! Goal: even if the envelope lets a small per-slot oracle lie through, the
//! attacker cannot withdraw the proceeds until WARMUP_SLOTS have elapsed —
//! by which time honest arbitrage / multi-source oracle divergence will have
//! collapsed the fake price.

use crate::error::PerpRouterError;
use crate::state::trader_account::{ReserveEntry, MAX_RESERVE_ENTRIES};

/// Append a fresh PnL entry. If the buffer is full, the oldest entry is
/// dropped — callers should ideally crank `mature_pnl` more often than the
/// buffer fills. Returns the new length.
pub fn push_reserve(
    reserve: &mut [ReserveEntry; MAX_RESERVE_ENTRIES],
    len: &mut u8,
    amount: u64,
    mature_slot: u64,
) -> Result<(), PerpRouterError> {
    if amount == 0 {
        return Ok(());
    }
    if (*len as usize) >= MAX_RESERVE_ENTRIES {
        // Shift left, drop oldest.
        for i in 0..MAX_RESERVE_ENTRIES - 1 {
            reserve[i] = reserve[i + 1];
        }
        reserve[MAX_RESERVE_ENTRIES - 1] = ReserveEntry { amount, mature_slot };
    } else {
        reserve[*len as usize] = ReserveEntry { amount, mature_slot };
        *len += 1;
    }
    Ok(())
}

/// Sweep all entries with `mature_slot <= current_slot` into a returned
/// `matured_total`. Survivors are compacted to the front of the buffer.
pub fn sweep_matured(
    reserve: &mut [ReserveEntry; MAX_RESERVE_ENTRIES],
    len: &mut u8,
    current_slot: u64,
) -> u64 {
    let mut matured: u64 = 0;
    let mut write: usize = 0;
    let read_end = *len as usize;
    for read in 0..read_end {
        let e = reserve[read];
        if current_slot >= e.mature_slot {
            matured = matured.saturating_add(e.amount);
        } else {
            if write != read {
                reserve[write] = e;
            }
            write += 1;
        }
    }
    // Zero the freed tail so stale data isn't readable.
    for i in write..read_end {
        reserve[i] = ReserveEntry::default();
    }
    *len = write as u8;
    matured
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty() -> ([ReserveEntry; MAX_RESERVE_ENTRIES], u8) {
        ([ReserveEntry::default(); MAX_RESERVE_ENTRIES], 0)
    }

    #[test]
    fn push_grows_until_full() {
        let (mut r, mut l) = empty();
        for i in 0..MAX_RESERVE_ENTRIES {
            push_reserve(&mut r, &mut l, 10, i as u64 + 100).unwrap();
        }
        assert_eq!(l as usize, MAX_RESERVE_ENTRIES);
        // One more push drops the oldest, len stays capped.
        push_reserve(&mut r, &mut l, 99, 999).unwrap();
        assert_eq!(l as usize, MAX_RESERVE_ENTRIES);
        assert_eq!(r[MAX_RESERVE_ENTRIES - 1].amount, 99);
        assert_eq!(r[MAX_RESERVE_ENTRIES - 1].mature_slot, 999);
    }

    #[test]
    fn zero_amount_is_no_op() {
        let (mut r, mut l) = empty();
        push_reserve(&mut r, &mut l, 0, 100).unwrap();
        assert_eq!(l, 0);
    }

    #[test]
    fn sweep_returns_only_matured() {
        let (mut r, mut l) = empty();
        push_reserve(&mut r, &mut l, 100, 50).unwrap(); // mature at 50
        push_reserve(&mut r, &mut l, 200, 150).unwrap(); // not yet
        push_reserve(&mut r, &mut l, 300, 50).unwrap(); // mature at 50
        let m = sweep_matured(&mut r, &mut l, 100);
        assert_eq!(m, 400);
        assert_eq!(l, 1);
        assert_eq!(r[0].amount, 200);
        assert_eq!(r[0].mature_slot, 150);
    }

    #[test]
    fn sweep_nothing_returns_zero() {
        let (mut r, mut l) = empty();
        push_reserve(&mut r, &mut l, 100, 1000).unwrap();
        assert_eq!(sweep_matured(&mut r, &mut l, 100), 0);
        assert_eq!(l, 1);
    }

    #[test]
    fn sweep_clears_tail_to_zero() {
        let (mut r, mut l) = empty();
        push_reserve(&mut r, &mut l, 100, 50).unwrap();
        push_reserve(&mut r, &mut l, 200, 50).unwrap();
        sweep_matured(&mut r, &mut l, 100);
        assert_eq!(l, 0);
        for e in r.iter() {
            assert_eq!(e.amount, 0);
            assert_eq!(e.mature_slot, 0);
        }
    }
}
