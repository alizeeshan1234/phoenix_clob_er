//! GlobalState — singleton across the deployment. Holds Percolator side
//! indices, recovery state, and balance-sheet totals. Delegated to ER during
//! normal operation.

use bytemuck::{Pod, Zeroable};
use solana_program::pubkey::Pubkey;

/// Recovery state machine values. Stored as `u8` to keep Pod-safe.
pub const RECOVERY_NORMAL: u8 = 0;
pub const RECOVERY_DRAIN_ONLY: u8 = 1;
pub const RECOVERY_RESET_PENDING: u8 = 2;

#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
pub struct GlobalState {
    // --- Percolator side indices (fixed-point i128 stored as little-endian
    // bytes so the struct remains Pod-safe under bytemuck) ---
    pub a: [u8; 16], // i128 scale factor; 1.0 == FIXED_POINT_ONE
    pub k: [u8; 16], // i128 accumulated mark effects
    pub f: [u8; 16], // i128 accumulated funding
    pub b: [u8; 16], // i128 bankruptcy residual

    pub epoch: u64,         // bumps each time A is reset
    pub recovery_state: u8, // RECOVERY_*
    pub _pad0: [u8; 7],

    // --- Balance-sheet totals (used by haircut H) ---
    pub v_total_pool_value: u64,
    pub c_total_collateral: u64,
    pub i_insurance_reserve: u64,
    pub total_matured_pnl: u64,

    pub admin: Pubkey,
    pub bump: u8,
    pub _pad1: [u8; 7],
}

// Sokoban ZeroCopy impl via direct bytemuck transmute helpers.
// (We do not derive ZeroCopy here to avoid pulling in the proc-macro at this
// stage; field-level access is via bytemuck::from_bytes once we wire loaders.)

impl GlobalState {
    pub const SEED: &'static [u8] = crate::constants::GLOBAL_STATE_SEED;

    pub fn get_a(&self) -> i128 { i128::from_le_bytes(self.a) }
    pub fn set_a(&mut self, v: i128) { self.a = v.to_le_bytes() }
    pub fn get_k(&self) -> i128 { i128::from_le_bytes(self.k) }
    pub fn set_k(&mut self, v: i128) { self.k = v.to_le_bytes() }
    pub fn get_f(&self) -> i128 { i128::from_le_bytes(self.f) }
    pub fn set_f(&mut self, v: i128) { self.f = v.to_le_bytes() }
    pub fn get_b(&self) -> i128 { i128::from_le_bytes(self.b) }
    pub fn set_b(&mut self, v: i128) { self.b = v.to_le_bytes() }
}
