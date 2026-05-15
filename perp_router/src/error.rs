//! Program-wide error type. Maps to `ProgramError::Custom(n)`.

use solana_program::{program_error::ProgramError, msg};
use thiserror::Error;

#[derive(Error, Debug, Copy, Clone)]
pub enum PerpRouterError {
    #[error("Invalid signer or missing authority")]
    InvalidAuthority,
    #[error("PDA seed mismatch")]
    InvalidPda,
    #[error("Account already initialized")]
    AlreadyInitialized,
    #[error("Account not initialized")]
    NotInitialized,
    #[error("Oracle price moved beyond per-slot envelope")]
    OracleEnvelopeExceeded,
    #[error("Recovery state forbids opening new positions")]
    RecoveryDrainOnly,
    #[error("Insufficient collateral for requested operation")]
    InsufficientCollateral,
    #[error("Position size exceeds max leverage")]
    LeverageExceeded,
    #[error("PnL is still in warmup; not withdrawable")]
    PnlInWarmup,
    #[error("Haircut math overflow")]
    HaircutOverflow,
    #[error("Side-index math overflow")]
    SideIndexOverflow,
    #[error("Phoenix CPI failed")]
    PhoenixCpiFailed,
    #[error("Receipt already processed")]
    ReceiptAlreadyProcessed,
    #[error("Receipt not yet processed")]
    ReceiptNotProcessed,
    #[error("Math overflow")]
    MathOverflow,
    #[error("Too many open positions")]
    PositionTableFull,
    #[error("Orderbook seat table is full")]
    SeatTableFull,
    #[error("Matching engine rejected order")]
    OrderRejected,
}

impl From<PerpRouterError> for ProgramError {
    fn from(e: PerpRouterError) -> Self {
        ProgramError::Custom(e as u32)
    }
}

#[inline]
pub fn assert_with_msg(cond: bool, err: impl Into<ProgramError>, message: &str) -> Result<(), ProgramError> {
    if !cond {
        msg!("Assertion failed: {}", message);
        Err(err.into())
    } else {
        Ok(())
    }
}
