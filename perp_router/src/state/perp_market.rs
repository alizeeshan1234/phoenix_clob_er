//! PerpMarket — per-market state. Links a Phoenix CLOB market (the matching
//! "slab") to an oracle, and accrues funding. Delegated to ER during
//! operation.

use bytemuck::{Pod, Zeroable};
use solana_program::pubkey::Pubkey;

#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
pub struct PerpMarket {
    /// Sibling Phoenix market used for matching.
    pub phoenix_market: Pubkey,
    /// Oracle feed (Pyth Pull receiver account on Solana / replicated to ER).
    pub oracle: Pubkey,

    /// Base SPL mint (the asset being traded — e.g. wSOL).
    pub base_mint: Pubkey,
    /// Quote SPL mint (collateral asset — typically USDC).
    pub quote_mint: Pubkey,
    /// Phoenix's base vault PDA for this market (matches Phoenix's
    /// `[b"vault", market, base_mint]`). Cached so we don't re-derive
    /// every Swap.
    pub phoenix_base_vault: Pubkey,
    pub phoenix_quote_vault: Pubkey,

    /// Last envelope-clamped oracle price seen, in oracle units.
    pub last_oracle_price: u64,
    /// Slot at which `last_oracle_price` was recorded.
    pub last_oracle_slot: u64,

    /// Max basis-point move allowed per slot (overrides global default).
    pub max_bps_per_slot: u32,
    /// Max leverage in basis points (e.g. 1000 = 10x).
    pub max_leverage_bps: u32,

    /// Funding index, accrued lazily.
    pub funding_index: [u8; 16], // i128
    /// Net open interest in base lots. Signed.
    pub open_interest: [u8; 16], // i128

    pub authority: Pubkey,
    pub bump: u8,
    pub _pad: [u8; 7],
}

impl PerpMarket {
    pub const SEED: &'static [u8] = crate::constants::PERP_MARKET_SEED;

    pub fn get_funding_index(&self) -> i128 { i128::from_le_bytes(self.funding_index) }
    pub fn set_funding_index(&mut self, v: i128) { self.funding_index = v.to_le_bytes() }
    pub fn get_open_interest(&self) -> i128 { i128::from_le_bytes(self.open_interest) }
    pub fn set_open_interest(&mut self, v: i128) { self.open_interest = v.to_le_bytes() }
}
