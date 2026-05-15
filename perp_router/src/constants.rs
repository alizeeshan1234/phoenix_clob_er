//! Compile-time constants for perp_router. All risk parameters live here so
//! they're auditable in one place and trivially swap-able across deployments.

/// Fixed-point scale for index math. All A/K/F/B side-indices and fractional
/// computations use this as 1.0.
pub const FIXED_POINT_ONE: i128 = 1_000_000_000_000_000_000; // 1e18

/// Oracle envelope: maximum price move per Solana slot, in basis points.
/// 50 bps = 0.5% per slot.
pub const MAX_BPS_PER_SLOT: u32 = 50;

/// PnL warmup: number of slots fresh PnL must age before maturing to
/// withdrawable. 150 slots ≈ 60s on Solana (400ms slots).
pub const WARMUP_SLOTS: u64 = 150;

/// Recovery: if |A| drifts below this fraction of FIXED_POINT_ONE, the system
/// transitions to DrainOnly. 0.95 = 5% maximum cumulative shrinkage before
/// forcing a reset cycle.
pub const A_PRECISION_FLOOR: i128 = 950_000_000_000_000_000; // 0.95e18

/// Maximum leverage in basis points. 100_000 bps = 10x.
/// (max_notional = margin × MAX_LEVERAGE_BPS / 10_000)
pub const MAX_LEVERAGE_BPS: u32 = 100_000;

/// Maximum open positions per trader.
pub const MAX_POSITIONS: usize = 8;

/// PDA seed prefixes.
pub const GLOBAL_STATE_SEED: &[u8] = b"global_state";
pub const PERP_MARKET_SEED: &[u8] = b"perp_market";
pub const TRADER_ACCOUNT_SEED: &[u8] = b"trader_account";
pub const DEPOSIT_RECEIPT_SEED: &[u8] = b"perp_deposit_receipt";
pub const WITHDRAWAL_RECEIPT_SEED: &[u8] = b"perp_withdrawal_receipt";
pub const ORDERBOOK_SEED: &[u8] = b"orderbook";

/// Singleton PDA that acts as:
///   1. The SPL token owner for all collateral / base / quote vaults
///   2. The "trader" identity perp_router presents to Phoenix when CPI-ing
///      `Swap` instructions (i.e. perp_router is one Phoenix trader)
/// All SPL transfers and Phoenix CPIs sign as this PDA via `invoke_signed`.
pub const PERP_AUTHORITY_SEED: &[u8] = b"perp_authority";
