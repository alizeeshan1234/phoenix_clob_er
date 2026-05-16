//! Margin-math helpers shared by the matched-flow instructions
//! (`place_order_perp`, `cancel_order_perp`). Pure functions; no Solana
//! account I/O.

use solana_program::program_error::ProgramError;

use crate::error::{assert_with_msg, PerpRouterError};

/// Convert (base_lots, price_in_ticks) to quote_lots using the matching
/// engine's accounting formula:
///   quote_lots = base_lots × price_in_ticks × tick_size_in_quote_lots_per_base_unit
///                / base_lots_per_base_unit
/// All arithmetic in u128 so high price × size doesn't overflow before
/// the divide. Returns an error if the final value exceeds u64::MAX.
pub fn quote_lots_from_base_at_price(
    base_lots: u64,
    price_in_ticks: u64,
    tick_size_qlpbupt: u64,
    base_lots_per_base_unit: u64,
) -> Result<u64, ProgramError> {
    assert_with_msg(
        base_lots_per_base_unit > 0,
        ProgramError::InvalidAccountData,
        "base_lots_per_base_unit must be > 0",
    )?;
    let n: u128 = (base_lots as u128)
        .checked_mul(price_in_ticks as u128)
        .ok_or(PerpRouterError::MathOverflow)?
        .checked_mul(tick_size_qlpbupt as u128)
        .ok_or(PerpRouterError::MathOverflow)?;
    let q = n / (base_lots_per_base_unit as u128);
    if q > u64::MAX as u128 {
        return Err(PerpRouterError::MathOverflow.into());
    }
    Ok(q as u64)
}

/// Quote lots → required margin (in collateral atoms). v1 assumes
/// `quote_lot_size = 1` (matching engine's "quote lot" equals 1
/// `TraderAccount.collateral` atom). Non-1 quote_lot_size requires a
/// per-market scaling field — flagged as a follow-up in the backlog.
pub fn quote_lots_to_margin(quote_lots: u64, max_leverage_bps: u32) -> Result<u64, ProgramError> {
    assert_with_msg(
        max_leverage_bps > 0,
        ProgramError::InvalidAccountData,
        "PerpMarket.max_leverage_bps must be > 0",
    )?;
    quote_lots
        .checked_mul(10_000)
        .ok_or_else(|| PerpRouterError::MathOverflow.into())
        .map(|n| n / max_leverage_bps as u64)
}
