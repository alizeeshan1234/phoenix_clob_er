//! CancelOrderPerp — ER hot path. Cancels a single resting order by its
//! `FIFOOrderId` (price_in_ticks + order_sequence_number) and releases
//! exactly the margin that backed that order.
//!
//! Prior versions cancelled *all* of the trader's orders in one shot and
//! zeroed `locked_margin` wholesale. That worked for the v1 test where
//! every trader had at most one resting order, but it's wrong for any
//! real client with concurrent orders — cancelling one would have
//! incorrectly released margin backing the others. The id surface comes
//! from `PlaceOrderPerp` via Solana return data: the place ix sets
//! `[price_in_ticks_le; order_sequence_number_le]` (16 bytes); the
//! client reads it back from `getTransaction(...).meta.returnData` and
//! threads it into this ix's data.
//!
//! Side is derived from the high bit of the sequence number per
//! Phoenix's `Side::from_order_sequence_number` (no need to pass it
//! explicitly).
//!
//! Account list:
//!   [0] trader         (signer)
//!   [1] trader_account (writable, delegated)
//!   [2] perp_market    (readonly; for max_leverage_bps + orderbook PDA)
//!   [3] orderbook      (writable, delegated)
//!
//! Instruction data (borsh):
//!   price_in_ticks: u64
//!   order_sequence_number: u64

use borsh::{BorshDeserialize, BorshSerialize};
use ephemeral_rollups_sdk::consts::DELEGATION_PROGRAM_ID;
use phoenix::{
    quantities::WrapperU64,
    state::{
        markets::{
            fifo::FIFOOrderId,
            market_traits::{Market, WritableMarket},
        },
        Side,
    },
};
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
    risk::margin::{quote_lots_from_base_at_price, quote_lots_to_margin},
    state::{PerpMarket, PerpOrderbook, TraderAccount},
    validation::loaders::{find_orderbook_address, find_trader_account_address},
};

#[derive(BorshSerialize, BorshDeserialize)]
pub struct CancelOrderPerpParams {
    pub price_in_ticks: u64,
    pub order_sequence_number: u64,
}

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let p = CancelOrderPerpParams::try_from_slice(data)?;

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

    let max_leverage_bps = {
        let buf = perp_market_info.try_borrow_data()?;
        let m = bytemuck::from_bytes::<PerpMarket>(&buf[..size_of::<PerpMarket>()]);
        m.max_leverage_bps
    };

    let order_id = FIFOOrderId::new_from_untyped(p.price_in_ticks, p.order_sequence_number);
    let side = Side::from_order_sequence_number(p.order_sequence_number);

    // Read the order's remaining size from the book BEFORE the cancel.
    // We can't recover the freed amount from `MatchingEngineResponse`
    // when `claim_funds = false`: reduce_order_inner returns an
    // all-zero response in that branch and stashes the freed amount on
    // the trader's seat (`quote_lots_free` / `base_lots_free`), which
    // is irrelevant to us since we have no SPL deposit flow. We need
    // the on-book size to know how much margin to release.
    let (tick_size_qlpbupt, base_lots_per_base_unit, cancelled_base_lots) = {
        let mut record_event_fn = |_event| {};
        let mut buf = orderbook_info.try_borrow_mut_data()?;
        let market =
            PerpOrderbook::load_mut_bytes(&mut buf).ok_or(ProgramError::InvalidAccountData)?;
        let ts = u64::from(market.get_tick_size());
        let blbu = u64::from(market.get_base_lots_per_base_unit());
        let cancelled = market
            .get_book(side)
            .get(&order_id)
            .map(|o| u64::from(o.num_base_lots))
            .ok_or(PerpRouterError::OrderRejected)?;
        market
            .cancel_order(trader.key, &order_id, side, false, &mut record_event_fn)
            .ok_or(PerpRouterError::OrderRejected)?;
        (ts, blbu, cancelled)
    };

    // Margin freed = (cancelled_base_lots × price × tick_size /
    // base_lots_per_base_unit) × 10_000 / max_leverage_bps.
    let cancelled_quote_lots = quote_lots_from_base_at_price(
        cancelled_base_lots,
        p.price_in_ticks,
        tick_size_qlpbupt,
        base_lots_per_base_unit,
    )?;
    let freed_margin = quote_lots_to_margin(cancelled_quote_lots, max_leverage_bps)?;

    // Release the freed margin back to free collateral. We deliberately
    // subtract rather than zero out — other orders by this trader may
    // still hold reservations.
    {
        let mut buf = trader_account_info.try_borrow_mut_data()?;
        let t = bytemuck::from_bytes_mut::<TraderAccount>(&mut buf[..size_of::<TraderAccount>()]);
        t.locked_margin = t
            .locked_margin
            .checked_sub(freed_margin)
            .ok_or(PerpRouterError::MathOverflow)?;
    }

    msg!(
        "CancelOrderPerp ok: side={:?} px={} seq={} released_margin={}",
        side,
        p.price_in_ticks,
        p.order_sequence_number,
        freed_margin,
    );
    Ok(())
}
