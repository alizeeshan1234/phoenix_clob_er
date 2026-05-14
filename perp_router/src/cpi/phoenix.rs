//! Typed wrappers around Phoenix CPI instructions.
//!
//! perp_router presents one identity to Phoenix: the singleton
//! [`perp_authority`](crate::validation::loaders::find_perp_authority_address)
//! PDA. It signs Phoenix `Swap` via `invoke_signed` with seeds
//! `[PERP_AUTHORITY_SEED, [bump]]`. Phoenix sees perp_router as a regular
//! taker — no Phoenix seat required (Swap is take-only).
//!
//! Order packet layout: Phoenix's `OrderPacket` is a Borsh enum. The
//! `ImmediateOrCancel` variant is what take-only swaps use. We hand-encode
//! it here to avoid dragging in Phoenix's full state types as a dep.

use borsh::BorshSerialize;
use solana_program::{
    account_info::AccountInfo,
    entrypoint::ProgramResult,
    instruction::{AccountMeta, Instruction},
    program::invoke_signed,
    program_error::ProgramError,
    pubkey::Pubkey,
};

use crate::constants::PERP_AUTHORITY_SEED;

/// Phoenix's program id is dynamic (deploy-specific). Client passes it via
/// the `phoenix_program` AccountInfo; we read its key at CPI time.
const PHOENIX_INSTRUCTION_SWAP: u8 = 0;

/// Direction of a take-only swap.
#[derive(Copy, Clone, Debug)]
pub enum Side {
    /// Buy base by spending quote.
    Bid,
    /// Sell base for quote.
    Ask,
}

/// Phoenix `Side` discriminant (matches `phoenix::state::Side`).
fn side_byte(s: Side) -> u8 {
    match s {
        Side::Bid => 0,
        Side::Ask => 1,
    }
}

/// Minimal Borsh-serializable subset of Phoenix's `OrderPacket` enum that
/// expresses "take only, IOC, swap base/quote up to limits". Phoenix
/// deserializes the full enum; we encode the `ImmediateOrCancel` variant
/// (discriminant 1) with the fields it expects.
///
/// IMPORTANT: keep this in sync with `phoenix::state::OrderPacket` if
/// Phoenix v1's order_packet.rs changes.
#[derive(BorshSerialize)]
struct ImmediateOrCancelPacket {
    /// `OrderPacket` discriminant for `ImmediateOrCancel` = 1.
    variant_tag: u8,
    /// Side (Bid=0, Ask=1)
    side: u8,
    /// Optional price-in-ticks (Option<u64>). `None` → cross the book.
    price_in_ticks_opt: u8,
    /// num_base_lots — base lots the swap wants to fill (0 = unspecified)
    num_base_lots: u64,
    /// num_quote_lots — quote lots the swap is willing to spend (0 = unspecified)
    num_quote_lots: u64,
    /// min_base_lots_to_fill (slippage floor on base side)
    min_base_lots_to_fill: u64,
    /// min_quote_lots_to_fill (slippage floor on quote side)
    min_quote_lots_to_fill: u64,
    /// `Option<SelfTradeBehavior>` → 0 = None
    self_trade_behavior_opt: u8,
    /// `Option<u32>` match_limit → 0 = None
    match_limit_opt: u8,
    /// Client-supplied order id (for tracking)
    client_order_id: u128,
    /// `Option<bool>` use_only_deposited_funds → 0 = None (i.e. wallet-funded)
    use_only_deposited_funds_opt: u8,
    /// `Option<u64>` last_valid_slot → 0 = None
    last_valid_slot_opt: u8,
    /// `Option<u64>` last_valid_unix_timestamp_in_seconds → 0 = None
    last_valid_unix_timestamp_in_seconds_opt: u8,
}

/// CPI Phoenix `Swap` (taker only, no seat required).
///
/// perp_router signs as the `perp_authority` PDA. Phoenix's account layout
/// for `Swap`:
///   [0] phoenix_program       (readonly; CPI target)
///   [1] log_authority         (readonly; Phoenix's `[b"log"]` PDA)
///   [2] market                (writable)
///   [3] trader = perp_authority (signer)
///   [4] base_account = perp_authority's base ATA (writable)
///   [5] quote_account = perp_authority's quote ATA (writable)
///   [6] base_vault = phoenix's `[b"vault", market, base_mint]` (writable)
///   [7] quote_vault = phoenix's `[b"vault", market, quote_mint]` (writable)
///   [8] token_program
///
/// Caller is responsible for passing these in `accounts` in this exact order.
#[allow(clippy::too_many_arguments)]
pub fn cpi_swap_take<'a, 'info>(
    program_id: &Pubkey,
    phoenix_program: &'a AccountInfo<'info>,
    log_authority: &'a AccountInfo<'info>,
    market: &'a AccountInfo<'info>,
    perp_authority: &'a AccountInfo<'info>,
    base_account: &'a AccountInfo<'info>,
    quote_account: &'a AccountInfo<'info>,
    phoenix_base_vault: &'a AccountInfo<'info>,
    phoenix_quote_vault: &'a AccountInfo<'info>,
    token_program: &'a AccountInfo<'info>,
    side: Side,
    num_base_lots: u64,
    num_quote_lots: u64,
    min_base_lots_to_fill: u64,
    min_quote_lots_to_fill: u64,
    client_order_id: u128,
) -> ProgramResult {
    let packet = ImmediateOrCancelPacket {
        variant_tag: 1, // ImmediateOrCancel
        side: side_byte(side),
        price_in_ticks_opt: 0, // None — cross the book
        num_base_lots,
        num_quote_lots,
        min_base_lots_to_fill,
        min_quote_lots_to_fill,
        self_trade_behavior_opt: 0,
        match_limit_opt: 0,
        client_order_id,
        use_only_deposited_funds_opt: 0,
        last_valid_slot_opt: 0,
        last_valid_unix_timestamp_in_seconds_opt: 0,
    };

    let mut data = Vec::with_capacity(80);
    data.push(PHOENIX_INSTRUCTION_SWAP);
    packet
        .serialize(&mut data)
        .map_err(|_| ProgramError::InvalidInstructionData)?;

    let ix = Instruction {
        program_id: *phoenix_program.key,
        accounts: vec![
            AccountMeta::new_readonly(*phoenix_program.key, false),
            AccountMeta::new_readonly(*log_authority.key, false),
            AccountMeta::new(*market.key, false),
            AccountMeta::new_readonly(*perp_authority.key, true),
            AccountMeta::new(*base_account.key, false),
            AccountMeta::new(*quote_account.key, false),
            AccountMeta::new(*phoenix_base_vault.key, false),
            AccountMeta::new(*phoenix_quote_vault.key, false),
            AccountMeta::new_readonly(*token_program.key, false),
        ],
        data,
    };

    let (_authority_pda, bump) =
        crate::validation::loaders::find_perp_authority_address(program_id);
    invoke_signed(
        &ix,
        &[
            phoenix_program.clone(),
            log_authority.clone(),
            market.clone(),
            perp_authority.clone(),
            base_account.clone(),
            quote_account.clone(),
            phoenix_base_vault.clone(),
            phoenix_quote_vault.clone(),
            token_program.clone(),
        ],
        &[&[PERP_AUTHORITY_SEED, &[bump]]],
    )
}
