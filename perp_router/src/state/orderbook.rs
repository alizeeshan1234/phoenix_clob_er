//! Per-market orderbook account.
//!
//! Wraps Phoenix's `FIFOMarket` — the same matching engine the upstream
//! spot CLOB uses — but instantiated under perp_router's program id so
//! the account is owned by *us*, not Phoenix. Phoenix's matching logic is
//! invoked in-process (the crate is pulled in as a `no-entrypoint` lib);
//! we never CPI to a deployed Phoenix program.
//!
//! Sizing (BIDS, ASKS, NUM_SEATS) = (32, 32, 32). The hard constraint is
//! Solana's single-CPI alloc cap (10,240 bytes): only PDA-allocated
//! accounts under that cap are delegatable to MagicBlock ER, and matching
//! must run on ER. The `(32, 32, 32)` shape weighs 9,104 bytes — see the
//! `sweep_shapes_vs_cpi_cap` test below. Upstream Phoenix uses the
//! identical "small market PDA-allocatable" pattern (see
//! `phoenix-v1/src/program/processor/initialize.rs:121-131`).
//!
//! Trader-id type is `Pubkey` so the resting order's maker maps directly
//! back to their `TraderAccount` PDA when a taker fill lands.

use phoenix::state::markets::FIFOMarket;
use solana_program::pubkey::Pubkey;

pub const PERP_ORDERBOOK_BIDS: usize = 32;
pub const PERP_ORDERBOOK_ASKS: usize = 32;
pub const PERP_ORDERBOOK_SEATS: usize = 32;

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

    #[test]
    fn sweep_shapes_vs_cpi_cap() {
        // MAX_PERMITTED_DATA_INCREASE — single-CPI alloc cap. Anything
        // above this must use chunked-realloc or keypair allocation, and
        // (per phoenix-v1 upstream) is not delegatable to ER.
        const CAP: usize = 10_240;

        macro_rules! row {
            ($b:literal, $a:literal, $s:literal) => {{
                let n = core::mem::size_of::<FIFOMarket<Pubkey, $b, $a, $s>>();
                let fits = if n <= CAP { "FITS" } else { "OVER" };
                println!(
                    "  ({:>3}, {:>3}, {:>3}) = {:>6} bytes ({:>5.1} KB)  {}  {:+} vs cap",
                    $b, $a, $s,
                    n,
                    n as f64 / 1024.0,
                    fits,
                    n as i64 - CAP as i64,
                );
            }};
        }

        println!("\nFIFOMarket<Pubkey, BIDS, ASKS, SEATS> size sweep (cap = {} bytes)\n", CAP);
        // Current shape — baseline.
        row!(512, 512, 128);
        // Halving sweep.
        row!(256, 256, 64);
        row!(128, 128, 64);
        row!(128, 128, 32);
        row!(64,  64,  64);
        row!(64,  64,  32);
        row!(64,  64,  16);
        row!(32,  32,  32);
        row!(32,  32,  16);
        row!(16,  16,  16);
        row!(16,  16,  8);
        // Asymmetric / depth-favoring options.
        row!(128, 128, 16);
        row!(96,  96,  32);
        println!();
    }
}
