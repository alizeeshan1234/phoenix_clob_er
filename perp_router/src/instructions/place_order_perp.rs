//! PlaceOrderPerp — ER hot path. Pushes a limit order into the in-tree
//! Phoenix matching engine on `PerpOrderbook`. The matching engine is
//! called as a library (no CPI to a deployed Phoenix program).
//!
//! Stage 2 scope: prove matching fires end-to-end. The fill is logged
//! but `TraderAccount` synthetic-position bookkeeping is deferred to
//! Stage 3 — once the matching path is solid we replace
//! `DirectOpenPosition` with this + the position update on fills.
//!
//! Account list:
//!   [0] trader      (signer)
//!   [1] perp_market (readonly; orderbook PDA derives from this)
//!   [2] orderbook   (writable, delegated)
//!
//! Instruction data (borsh):
//!   side: u8                  (0=Bid, 1=Ask — matches `phoenix::state::Side`)
//!   price_in_ticks: u64
//!   num_base_lots: u64
//!   client_order_id: u128

use borsh::{BorshDeserialize, BorshSerialize};
use ephemeral_rollups_sdk::consts::DELEGATION_PROGRAM_ID;
use phoenix::{
    state::{
        markets::market_traits::WritableMarket, order_packet::OrderPacket, SelfTradeBehavior, Side,
    },
};
use sokoban::ZeroCopy;
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    clock::Clock,
    entrypoint::ProgramResult,
    msg,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvar::Sysvar,
};

use crate::{
    error::{assert_with_msg, PerpRouterError},
    state::PerpOrderbook,
    validation::loaders::find_orderbook_address,
};

#[derive(BorshSerialize, BorshDeserialize)]
pub struct PlaceOrderPerpParams {
    pub side: u8,
    pub price_in_ticks: u64,
    pub num_base_lots: u64,
    pub client_order_id: u128,
}

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let p = PlaceOrderPerpParams::try_from_slice(data)?;
    let side = match p.side {
        0 => Side::Bid,
        1 => Side::Ask,
        _ => return Err(ProgramError::InvalidInstructionData),
    };
    assert_with_msg(
        p.num_base_lots > 0 && p.price_in_ticks > 0,
        ProgramError::InvalidInstructionData,
        "price + size must be > 0",
    )?;

    let it = &mut accounts.iter();
    let trader = next_account_info(it)?;
    let perp_market_info = next_account_info(it)?;
    let orderbook_info = next_account_info(it)?;

    assert_with_msg(
        trader.is_signer,
        ProgramError::MissingRequiredSignature,
        "trader must sign PlaceOrderPerp",
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

    let order_packet = OrderPacket::new_limit_order(
        side,
        p.price_in_ticks,
        p.num_base_lots,
        SelfTradeBehavior::CancelProvide,
        None,
        p.client_order_id,
        // `use_only_deposited_funds = false` — there is no SPL deposit
        // flow for perps; the matching engine posts the order and the
        // returned MatchingEngineResponse tells us how many quote / base
        // lots the caller is "on the hook" for externally. Stage 2 just
        // logs that; Stage 3 will turn it into a TraderAccount margin
        // reservation against `collateral`.
        false,
    );

    let clock = Clock::get()?;
    let mut get_clock_fn = || (clock.slot, clock.unix_timestamp as u64);
    // Event recording is a no-op for Stage 2. Stage 3 will route fills
    // into TraderAccount.position bookkeeping.
    let mut record_event_fn = |_event| {};

    let mut buf = orderbook_info.try_borrow_mut_data()?;
    let market =
        PerpOrderbook::load_mut_bytes(&mut buf).ok_or(ProgramError::InvalidAccountData)?;

    let (resting_order_id, response) = market
        .place_order(trader.key, order_packet, &mut record_event_fn, &mut get_clock_fn)
        .ok_or(PerpRouterError::OrderRejected)?;

    msg!(
        "PlaceOrderPerp ok: side={:?} px={} sz={} resting_id={:?} qin={} bout={} bin={} qout={} posted_b={} posted_q={}",
        side,
        p.price_in_ticks,
        p.num_base_lots,
        resting_order_id,
        u64::from(response.num_quote_lots_in),
        u64::from(response.num_base_lots_out),
        u64::from(response.num_base_lots_in),
        u64::from(response.num_quote_lots_out),
        u64::from(response.num_base_lots_posted),
        u64::from(response.num_quote_lots_posted),
    );
    Ok(())
}
