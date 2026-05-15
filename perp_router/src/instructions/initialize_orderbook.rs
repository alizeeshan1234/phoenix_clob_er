//! InitializeOrderbook — base layer, admin-only.
//!
//! Allocates the per-market orderbook PDA at the small `(32, 32, 32)`
//! `FIFOMarket` shape (~9 KB), then runs the in-tree Phoenix matching
//! engine's `initialize_with_params` + `set_fee` on it. The size fits
//! under Solana's single-CPI alloc cap (10,240 bytes), so allocation is
//! one `invoke_signed` here — no client-side pre-allocation, no chunked
//! realloc. That shape is the largest one that's both PDA-allocatable
//! *and* delegatable to MagicBlock ER, which is where matching has to run
//! once trading goes live (see the `sweep_shapes_vs_cpi_cap` test in
//! `state/orderbook.rs`).
//!
//! Account list:
//!   [0] admin           (signer, writable, payer)
//!   [1] system_program
//!   [2] perp_market     (writable; orderbook_bump stored back into it)
//!   [3] orderbook       (writable; system-owned uninit PDA — created here)

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
    rent::Rent,
    system_program,
    sysvar::Sysvar,
};
use std::mem::size_of;

use crate::{
    constants::ORDERBOOK_SEED,
    error::{assert_with_msg, PerpRouterError},
    state::{PerpMarket, PerpOrderbook, PERP_ORDERBOOK_SIZE},
    system_utils::create_account,
    validation::loaders::{find_orderbook_address, find_perp_market_address},
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
    let system_program_info = next_account_info(it)?;
    let perp_market_info = next_account_info(it)?;
    let orderbook_info = next_account_info(it)?;

    assert_with_msg(
        admin.is_signer,
        ProgramError::MissingRequiredSignature,
        "admin must sign InitializeOrderbook",
    )?;
    assert_with_msg(
        system_program_info.key == &system_program::ID,
        ProgramError::IncorrectProgramId,
        "system_program mismatch",
    )?;

    // perp_market PDA + ownership + authority check.
    {
        let buf = perp_market_info.try_borrow_data()?;
        let m = bytemuck::from_bytes::<PerpMarket>(&buf[..size_of::<PerpMarket>()]);
        let (expected, _) = find_perp_market_address(&m.phoenix_market, program_id);
        assert_with_msg(
            &expected == perp_market_info.key,
            PerpRouterError::InvalidPda,
            "perp_market PDA mismatch",
        )?;
        assert_with_msg(
            &m.authority == admin.key,
            PerpRouterError::InvalidAuthority,
            "Caller is not the recorded PerpMarket.authority",
        )?;
        assert_with_msg(
            m.orderbook_bump == 0,
            PerpRouterError::AlreadyInitialized,
            "Orderbook already initialised for this perp_market",
        )?;
    }
    assert_with_msg(
        perp_market_info.owner == &crate::ID,
        ProgramError::IllegalOwner,
        "perp_market must be perp_router-owned",
    )?;

    // Orderbook PDA derivation + uninit check.
    let (expected_orderbook, orderbook_bump) =
        find_orderbook_address(perp_market_info.key, program_id);
    assert_with_msg(
        &expected_orderbook == orderbook_info.key,
        PerpRouterError::InvalidPda,
        "orderbook PDA mismatch",
    )?;
    assert_with_msg(
        orderbook_info.owner == &system_program::ID && orderbook_info.data_is_empty(),
        PerpRouterError::AlreadyInitialized,
        "orderbook already initialised",
    )?;

    // Single-CPI allocate + assign, signed with the orderbook PDA seeds.
    let perp_market_key = *perp_market_info.key;
    let rent = Rent::get()?;
    let seeds: Vec<Vec<u8>> = vec![
        ORDERBOOK_SEED.to_vec(),
        perp_market_key.as_ref().to_vec(),
        vec![orderbook_bump],
    ];
    create_account(
        admin,
        orderbook_info,
        system_program_info,
        program_id,
        &rent,
        PERP_ORDERBOOK_SIZE as u64,
        seeds,
    )?;

    // Initialise the FIFOMarket layout in-place on the freshly-allocated
    // (zeroed) bytes.
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

    // Persist the orderbook bump on perp_market — DelegateOrderbook reads
    // it back to sign the delegation CPI with the correct seeds.
    {
        let mut buf = perp_market_info.try_borrow_mut_data()?;
        let m = bytemuck::from_bytes_mut::<PerpMarket>(&mut buf[..size_of::<PerpMarket>()]);
        m.orderbook_bump = orderbook_bump;
    }

    Ok(())
}
