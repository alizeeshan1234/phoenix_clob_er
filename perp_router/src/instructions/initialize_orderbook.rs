//! InitializeOrderbook — base layer, admin-only.
//!
//! Zero-inits a Phoenix `FIFOMarket<Pubkey, 512, 512, 128>` layout in the
//! provided `orderbook` account. The matching engine runs **in-process**
//! (perp_router pulls phoenix-v1 in as a `no-entrypoint` lib) — there is
//! no CPI to a separately deployed Phoenix program.
//!
//! The orderbook is ~82 KB, which is larger than Solana's 10 KB CPI
//! allocation cap (MAX_PERMITTED_DATA_INCREASE). The client therefore
//! pre-allocates the account at full size via a top-level
//! `system_instruction::create_account` (no CPI) using a fresh keypair
//! and assigns it to `perp_router` before this ix runs. This ix only
//! does the data-layout setup.
//!
//! Account list:
//!   [0] admin           (signer; gates initialisation)
//!   [1] perp_market     (readonly; context check — must be perp_router-owned)
//!   [2] orderbook       (writable; pre-allocated, perp_router-owned, zero-data)

use borsh::{BorshDeserialize, BorshSerialize};
use phoenix::{
    quantities::{BaseLotsPerBaseUnit, QuoteLotsPerBaseUnitPerTick, WrapperU64},
    state::markets::market_traits::WritableMarket,
};
use sokoban::ZeroCopy;
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    program_error::ProgramError,
    pubkey::Pubkey,
};

use crate::{
    error::{assert_with_msg, PerpRouterError},
    state::{PerpOrderbook, PERP_ORDERBOOK_SIZE},
    validation::loaders::find_perp_market_address,
};

#[derive(BorshSerialize, BorshDeserialize)]
pub struct InitializeOrderbookParams {
    /// Tick size, measured in quote lots per base unit per tick.
    pub tick_size_in_quote_lots_per_base_unit: u64,
    /// Number of base lots in one base unit (lot size denominator).
    pub base_lots_per_base_unit: u64,
    /// Taker fee in basis points (0..=10_000).
    pub taker_fee_bps: u16,
}

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let p = InitializeOrderbookParams::try_from_slice(data)?;
    assert_with_msg(
        p.base_lots_per_base_unit > 0 && p.tick_size_in_quote_lots_per_base_unit > 0,
        ProgramError::InvalidInstructionData,
        "lot/tick params must be non-zero",
    )?;
    assert_with_msg(
        p.taker_fee_bps <= 10_000,
        ProgramError::InvalidInstructionData,
        "taker_fee_bps must be <= 10_000",
    )?;

    let it = &mut accounts.iter();
    let admin = next_account_info(it)?;
    let perp_market_info = next_account_info(it)?;
    let orderbook_info = next_account_info(it)?;

    assert_with_msg(
        admin.is_signer,
        ProgramError::MissingRequiredSignature,
        "admin must sign InitializeOrderbook",
    )?;

    // perp_market PDA + ownership check.
    {
        use crate::state::PerpMarket;
        use std::mem::size_of;
        let buf = perp_market_info.try_borrow_data()?;
        let m = bytemuck::from_bytes::<PerpMarket>(&buf[..size_of::<PerpMarket>()]);
        let (expected, _) = find_perp_market_address(&m.phoenix_market, program_id);
        assert_with_msg(
            &expected == perp_market_info.key,
            PerpRouterError::InvalidPda,
            "perp_market PDA mismatch",
        )?;
    }
    assert_with_msg(
        perp_market_info.owner == &crate::ID,
        ProgramError::IllegalOwner,
        "perp_market must be perp_router-owned",
    )?;

    // Orderbook must already exist at full size, owned by perp_router,
    // with zero-initialised data (FIFOMarket initialise asserts the
    // base_lots/sequence_number fields are zero).
    assert_with_msg(
        orderbook_info.owner == &crate::ID,
        ProgramError::IllegalOwner,
        "orderbook must be perp_router-owned (pre-allocated by client)",
    )?;
    assert_with_msg(
        orderbook_info.data_len() == PERP_ORDERBOOK_SIZE,
        ProgramError::InvalidAccountData,
        "orderbook data length must equal PERP_ORDERBOOK_SIZE",
    )?;

    // Cast the fresh bytes to a `FIFOMarket` and run phoenix's matching
    // engine initialisation in-process.
    {
        let mut data = orderbook_info.try_borrow_mut_data()?;
        let market = PerpOrderbook::load_mut_bytes(&mut data)
            .ok_or(ProgramError::InvalidAccountData)?;
        market.initialize_with_params(
            QuoteLotsPerBaseUnitPerTick::new(p.tick_size_in_quote_lots_per_base_unit),
            BaseLotsPerBaseUnit::new(p.base_lots_per_base_unit),
        );
        market.set_fee(p.taker_fee_bps as u64);
    }

    Ok(())
}
