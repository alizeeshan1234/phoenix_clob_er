//! PDA derivation helpers. Mirrors the style of
//! `phoenix-v1/src/program/validation/loaders.rs`.

use solana_program::pubkey::Pubkey;

use crate::constants::{
    DEPOSIT_RECEIPT_SEED, GLOBAL_STATE_SEED, PERP_AUTHORITY_SEED, PERP_MARKET_SEED,
    TRADER_ACCOUNT_SEED, WITHDRAWAL_RECEIPT_SEED,
};

/// perp_router's singleton SPL/CPI authority.
pub fn find_perp_authority_address(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[PERP_AUTHORITY_SEED], program_id)
}

pub fn find_global_state_address(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[GLOBAL_STATE_SEED], program_id)
}

pub fn find_perp_market_address(
    phoenix_market: &Pubkey,
    program_id: &Pubkey,
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[PERP_MARKET_SEED, phoenix_market.as_ref()],
        program_id,
    )
}

pub fn find_trader_account_address(
    owner: &Pubkey,
    program_id: &Pubkey,
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[TRADER_ACCOUNT_SEED, owner.as_ref()],
        program_id,
    )
}

/// Deposit receipt is per-trader (single in-flight deposit per user).
/// The receipt's `market` field is metadata for audit; PDA does not include it.
pub fn find_deposit_receipt_address(
    trader: &Pubkey,
    program_id: &Pubkey,
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[DEPOSIT_RECEIPT_SEED, trader.as_ref()],
        program_id,
    )
}

pub fn find_withdrawal_receipt_address(
    trader: &Pubkey,
    program_id: &Pubkey,
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[WITHDRAWAL_RECEIPT_SEED, trader.as_ref()],
        program_id,
    )
}
