//! ClaimSeat — ER hot path. Registers the trader's pubkey into the
//! `PerpOrderbook.traders` inline RedBlackTree so they can later place
//! resting limit/post-only orders against the matching engine.
//!
//! Idempotent. The matching engine itself auto-registers on Limit / PostOnly
//! `place_order` (see `phoenix-v1/src/state/markets/fifo.rs:793`), so
//! ClaimSeat is mostly useful for take-only flows (IOC / FoK) which only
//! match against existing seats, and for explicitly reserving a slot
//! before the per-market 32-seat cap is full.
//!
//! Runs on ER because the orderbook is delegated; perp_router-owned base
//! state would simply be a stale clone of ER state during the trading
//! window.
//!
//! Account list:
//!   [0] trader      (signer)
//!   [1] perp_market (readonly; orderbook PDA derives from this)
//!   [2] orderbook   (writable, delegated)

use ephemeral_rollups_sdk::consts::DELEGATION_PROGRAM_ID;
use phoenix::state::markets::market_traits::WritableMarket;
use sokoban::ZeroCopy;
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    program_error::ProgramError,
    pubkey::Pubkey,
};

use crate::{
    error::{assert_with_msg, PerpRouterError},
    state::PerpOrderbook,
    validation::loaders::find_orderbook_address,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let it = &mut accounts.iter();
    let trader = next_account_info(it)?;
    let perp_market_info = next_account_info(it)?;
    let orderbook_info = next_account_info(it)?;

    assert_with_msg(
        trader.is_signer,
        ProgramError::MissingRequiredSignature,
        "trader must sign ClaimSeat",
    )?;
    assert_with_msg(
        perp_market_info.owner == &crate::ID
            || perp_market_info.owner == &DELEGATION_PROGRAM_ID,
        ProgramError::IllegalOwner,
        "perp_market ownership mismatch",
    )?;
    assert_with_msg(
        orderbook_info.owner == &crate::ID
            || orderbook_info.owner == &DELEGATION_PROGRAM_ID,
        ProgramError::IllegalOwner,
        "orderbook ownership mismatch",
    )?;

    let (expected_orderbook, _) =
        find_orderbook_address(perp_market_info.key, program_id);
    assert_with_msg(
        &expected_orderbook == orderbook_info.key,
        PerpRouterError::InvalidPda,
        "orderbook PDA mismatch",
    )?;

    let mut data = orderbook_info.try_borrow_mut_data()?;
    let market =
        PerpOrderbook::load_mut_bytes(&mut data).ok_or(ProgramError::InvalidAccountData)?;
    // Returns Some(index) on success, None if the seat-table is full.
    market
        .get_or_register_trader(trader.key)
        .ok_or(PerpRouterError::SeatTableFull)?;
    Ok(())
}
