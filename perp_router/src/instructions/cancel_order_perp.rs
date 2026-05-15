//! CancelOrderPerp — ER hot path. Cancels *all* of the trader's resting
//! orders on the orderbook in one shot and releases their locked margin
//! back to free collateral.
//!
//! Stage 3a ships the bulk cancel — `cancel_all_orders` on the WritableMarket
//! trait drains every resting order owned by the trader and returns an
//! aggregate `MatchingEngineResponse`. Per-order cancel (by FIFOOrderId)
//! is a Stage 3.x add-on that requires plumbing the order id out of
//! PlaceOrderPerp via Solana return data.
//!
//! The released margin is the *full* current `locked_margin` for this
//! trader, on the simplifying assumption that all the trader's locked
//! collateral backs orders on this orderbook. Multi-market support will
//! need per-order or per-market accounting.
//!
//! Account list:
//!   [0] trader         (signer)
//!   [1] trader_account (writable, delegated)
//!   [2] perp_market    (readonly; for orderbook PDA + leverage params)
//!   [3] orderbook      (writable, delegated)

use ephemeral_rollups_sdk::consts::DELEGATION_PROGRAM_ID;
use phoenix::state::markets::market_traits::WritableMarket;
use sokoban::ZeroCopy;
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    msg,
    program_error::ProgramError,
    pubkey::Pubkey,
};
use std::mem::size_of;

use crate::{
    error::{assert_with_msg, PerpRouterError},
    state::{PerpOrderbook, TraderAccount},
    validation::loaders::{find_orderbook_address, find_trader_account_address},
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let it = &mut accounts.iter();
    let trader = next_account_info(it)?;
    let trader_account_info = next_account_info(it)?;
    let perp_market_info = next_account_info(it)?;
    let orderbook_info = next_account_info(it)?;

    assert_with_msg(
        trader.is_signer,
        ProgramError::MissingRequiredSignature,
        "trader must sign CancelOrderPerp",
    )?;
    for info in [trader_account_info, perp_market_info, orderbook_info] {
        assert_with_msg(
            info.owner == &crate::ID || info.owner == &DELEGATION_PROGRAM_ID,
            ProgramError::IllegalOwner,
            "account ownership mismatch",
        )?;
    }

    let (expected_orderbook, _) =
        find_orderbook_address(perp_market_info.key, program_id);
    assert_with_msg(
        &expected_orderbook == orderbook_info.key,
        PerpRouterError::InvalidPda,
        "orderbook PDA mismatch",
    )?;
    let (expected_trader_account, _) = find_trader_account_address(trader.key, program_id);
    assert_with_msg(
        &expected_trader_account == trader_account_info.key,
        PerpRouterError::InvalidPda,
        "trader_account PDA mismatch",
    )?;
    {
        let buf = trader_account_info.try_borrow_data()?;
        let t = bytemuck::from_bytes::<TraderAccount>(&buf[..size_of::<TraderAccount>()]);
        assert_with_msg(
            &t.owner == trader.key,
            PerpRouterError::InvalidAuthority,
            "trader_account.owner != signer",
        )?;
    }

    // Drain every resting order owned by this trader on this orderbook.
    let mut record_event_fn = |_event| {};
    let _response = {
        let mut buf = orderbook_info.try_borrow_mut_data()?;
        let market =
            PerpOrderbook::load_mut_bytes(&mut buf).ok_or(ProgramError::InvalidAccountData)?;
        market
            .cancel_all_orders(trader.key, false, &mut record_event_fn)
            .ok_or(PerpRouterError::OrderRejected)?
    };

    // Release the full locked_margin. Stage 3.x with per-order tracking
    // will refine this to the actual freed portion.
    let released = {
        let mut buf = trader_account_info.try_borrow_mut_data()?;
        let t = bytemuck::from_bytes_mut::<TraderAccount>(&mut buf[..size_of::<TraderAccount>()]);
        let r = t.locked_margin;
        t.locked_margin = 0;
        r
    };

    msg!("CancelOrderPerp ok: released_locked_margin={}", released);
    Ok(())
}
