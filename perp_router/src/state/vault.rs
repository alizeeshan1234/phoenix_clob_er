//! Collateral / base vaults are SPL token accounts whose SPL "owner" is the
//! `perp_authority` PDA. perp_router signs SPL transfers (Phoenix Swap,
//! user withdrawals) as `perp_authority` via `invoke_signed`.
//!
//! Address derivation is the standard associated token account formula:
//! `ATA(perp_authority, mint)`. This means clients create the vaults via
//! `createAssociatedTokenAccountIdempotentInstruction` — no custom init
//! flow needed.

use solana_program::pubkey::Pubkey;
use spl_associated_token_account::get_associated_token_address;

use crate::validation::loaders::find_perp_authority_address;

/// Per-asset collateral vault (also serves as Phoenix Swap's quote_account
/// when the asset is the quote mint, or base_account when it's the base
/// mint of a market). One vault per mint.
pub fn find_collateral_vault_address(mint: &Pubkey, program_id: &Pubkey) -> Pubkey {
    let (authority, _) = find_perp_authority_address(program_id);
    get_associated_token_address(&authority, mint)
}

/// Alias used by Phoenix CPI sites — same ATA layout, different intent.
pub fn find_perp_token_account(mint: &Pubkey, program_id: &Pubkey) -> Pubkey {
    find_collateral_vault_address(mint, program_id)
}
