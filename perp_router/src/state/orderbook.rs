//! Per-market orderbook account.
//!
//! Wraps Phoenix's `FIFOMarket` — the same matching engine the upstream
//! spot CLOB uses — but instantiated under perp_router's program id so
//! the account is owned by *us*, not Phoenix. Phoenix's matching logic is
//! invoked in-process (the crate is pulled in as a `no-entrypoint` lib);
//! we never CPI to a deployed Phoenix program.
//!
//! Sizing (BIDS, ASKS, NUM_SEATS) = (512, 512, 128) — the smallest market
//! shape Phoenix's dispatcher supports. Roughly enough room for 512
//! resting orders on each side and 128 distinct makers per market.
//!
//! Trader-id type is `Pubkey` so the resting order's maker maps directly
//! back to their `TraderAccount` PDA when a taker fill lands.

use phoenix::state::markets::FIFOMarket;
use solana_program::pubkey::Pubkey;

pub const PERP_ORDERBOOK_BIDS: usize = 512;
pub const PERP_ORDERBOOK_ASKS: usize = 512;
pub const PERP_ORDERBOOK_SEATS: usize = 128;

/// Concrete FIFOMarket type used as the perp_router orderbook.
pub type PerpOrderbook = FIFOMarket<
    Pubkey,
    PERP_ORDERBOOK_BIDS,
    PERP_ORDERBOOK_ASKS,
    PERP_ORDERBOOK_SEATS,
>;

/// Account size required to hold a zero-initialised `PerpOrderbook`.
pub const PERP_ORDERBOOK_SIZE: usize = core::mem::size_of::<PerpOrderbook>();

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn report_size() {
        println!(
            "PerpOrderbook = {} bytes ({:.1} KB)",
            PERP_ORDERBOOK_SIZE,
            PERP_ORDERBOOK_SIZE as f64 / 1024.0,
        );
    }
}
