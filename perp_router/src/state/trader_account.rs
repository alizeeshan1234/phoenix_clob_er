//! TraderAccount — per-user state. Holds collateral and synthetic positions.
//! Delegated to ER during active trading sessions.

use bytemuck::{Pod, Zeroable};
use solana_program::pubkey::Pubkey;

use crate::constants::MAX_POSITIONS;

/// A single open synthetic position. The `size_stored` field is raw; the
/// effective size must be computed via `risk::side_index::apply_indices`
/// before use, because A/K/F/B side-index updates apply lazily.
///
/// `entry_price` is the envelope-clamped mark at which the position was
/// opened. `margin_locked` is the collateral set aside when opening. Both
/// are authoritative on-chain — `close_position` reads them and so can
/// validate PnL and margin refund without trusting a client param.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
pub struct Position {
    pub market: Pubkey,
    /// Signed base-lot size at the time the position was last touched.
    /// Effective size = size_stored * (current A / FIXED_POINT_ONE).
    pub size_stored: i64,
    /// Envelope-clamped entry mark price (in oracle units).
    pub entry_price: u64,
    /// Collateral originally locked when this position was opened.
    /// Refunded on close.
    pub margin_locked: u64,
    pub entry_funding_index: [u8; 16], // i128
    pub last_epoch_seen: u64,
}

/// A pending PnL entry that hasn't yet matured. Once `current_slot >=
/// mature_slot`, the entry is swept into `pnl_matured`.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
pub struct ReserveEntry {
    pub amount: u64,
    pub mature_slot: u64,
}

/// Fixed-size reserve ring buffer to keep TraderAccount Pod-safe.
pub const MAX_RESERVE_ENTRIES: usize = 16;

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct TraderAccount {
    pub owner: Pubkey,

    /// Free collateral (USDC base units) deposited via the receipt flow and
    /// not currently posted as margin.
    pub collateral: u64,

    /// Matured PnL — withdrawable, subject to haircut.
    pub pnl_matured: u64,

    /// Pending PnL entries awaiting warmup.
    pub pnl_reserve: [ReserveEntry; MAX_RESERVE_ENTRIES],
    pub pnl_reserve_len: u8,
    pub _pad0: [u8; 7],

    pub positions: [Position; MAX_POSITIONS],
    pub positions_len: u8,
    pub _pad1: [u8; 7],

    /// Epoch of GlobalState when this account last had its A/K/F/B applied.
    pub last_index_epoch: u64,

    pub bump: u8,
    pub _pad2: [u8; 7],
}

impl Default for TraderAccount {
    fn default() -> Self {
        // SAFETY: Pod guarantees zero is a valid bit pattern.
        unsafe { core::mem::zeroed() }
    }
}

impl TraderAccount {
    pub const SEED: &'static [u8] = crate::constants::TRADER_ACCOUNT_SEED;
}
