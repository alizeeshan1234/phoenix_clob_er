//! PlaceOrderPerp — ER hot path. Pushes a limit order into the in-tree
//! Phoenix matching engine on `PerpOrderbook`, captures any fills, and
//! settles both taker and maker `TraderAccount.positions[]` plus margin
//! bookkeeping.
//!
//! Stage 3b scope:
//!   - Posted portion (no cross): reserves margin from `collateral`
//!     into `locked_margin` (Stage 3a behavior, unchanged).
//!   - Filled portion (crossed against resting liquidity): for each
//!     Fill event the matching engine emits,
//!       * taker: debit `collateral` for the fill's margin, push the
//!         fill into `positions[market]` with VWAP-blended entry price.
//!       * maker: release `locked_margin` (was reserved when their
//!         order rested), push the opposite-side fill into their
//!         `positions[market]` with VWAP blend.
//!
//! Account list:
//!   [0] trader         (signer)
//!   [1] trader_account (writable, delegated)
//!   [2] perp_market    (readonly; for max_leverage_bps + orderbook PDA)
//!   [3] orderbook      (writable, delegated)
//!   [4..] maker_accounts (writable, delegated) — one TraderAccount per
//!         maker the caller expects to fill against. Looked up by
//!         pubkey-matching the derived TraderAccount PDA against
//!         `Fill.maker_id`. Caller off-chain scans the book to discover.
//!
//! Instruction data (borsh):
//!   side: u8                  (0=Bid, 1=Ask — matches `phoenix::state::Side`)
//!   price_in_ticks: u64
//!   num_base_lots: u64
//!   client_order_id: u128

use borsh::{BorshDeserialize, BorshSerialize};
use ephemeral_rollups_sdk::consts::DELEGATION_PROGRAM_ID;
use phoenix::state::{
    markets::{market_events::MarketEvent, market_traits::WritableMarket},
    order_packet::OrderPacket,
    SelfTradeBehavior, Side,
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
use std::mem::size_of;

use crate::{
    constants::MAX_POSITIONS,
    error::{assert_with_msg, PerpRouterError},
    state::{trader_account::Position, PerpMarket, PerpOrderbook, TraderAccount},
    validation::loaders::{find_orderbook_address, find_trader_account_address},
};

/// Max fills the engine may emit for a single PlaceOrderPerp call.
/// Bounded by Solana's per-tx account cap (~64): each fill needs the
/// maker's TraderAccount in remaining-accounts.
const MAX_FILLS_PER_IX: usize = 8;

#[derive(BorshSerialize, BorshDeserialize)]
pub struct PlaceOrderPerpParams {
    pub side: u8,
    pub price_in_ticks: u64,
    pub num_base_lots: u64,
    pub client_order_id: u128,
}

/// A single Fill captured from the matching engine's event stream.
#[derive(Copy, Clone, Default)]
struct CapturedFill {
    maker_id: Pubkey,
    price_in_ticks: u64,
    base_lots_filled: u64,
}

/// Notional → required margin. Inputs are in raw quote lots (i.e. the
/// matching engine's accounting unit). Output is in the same unit, which
/// for the v1 lot params (`tick_size=1, base_lots_per_base_unit=1`) is
/// 1:1 with `TraderAccount.collateral` quote atoms.
fn notional_to_margin(quote_notional: u64, max_leverage_bps: u32) -> Result<u64, ProgramError> {
    assert_with_msg(
        max_leverage_bps > 0,
        ProgramError::InvalidAccountData,
        "PerpMarket.max_leverage_bps must be > 0",
    )?;
    quote_notional
        .checked_mul(10_000)
        .ok_or_else(|| PerpRouterError::MathOverflow.into())
        .map(|n| n / max_leverage_bps as u64)
}

/// Find a position slot for `market` in `t.positions`; allocate a new
/// slot if absent. Returns the slot index.
fn position_slot(t: &mut TraderAccount, market: &Pubkey) -> Result<usize, ProgramError> {
    for i in 0..(t.positions_len as usize) {
        if t.positions[i].market == *market {
            return Ok(i);
        }
    }
    let i = t.positions_len as usize;
    if i >= MAX_POSITIONS {
        return Err(PerpRouterError::PositionTableFull.into());
    }
    t.positions_len += 1;
    t.positions[i] = Position::default();
    t.positions[i].market = *market;
    Ok(i)
}

/// VWAP-blend a new fill into an existing position. `signed_size` is +ve
/// for longs (buys), -ve for shorts (sells). `margin_used` is added to
/// `positions[].margin_locked`.
fn blend_fill_into_position(
    t: &mut TraderAccount,
    market: &Pubkey,
    signed_size: i64,
    fill_price: u64,
    margin_used: u64,
) -> ProgramResult {
    let slot = position_slot(t, market)?;
    let pos = &mut t.positions[slot];

    let old_abs = pos.size_stored.unsigned_abs() as u128;
    let added_abs = signed_size.unsigned_abs() as u128;
    let blended_entry = if old_abs == 0 {
        fill_price
    } else {
        let num = old_abs
            .checked_mul(pos.entry_price as u128)
            .ok_or(PerpRouterError::MathOverflow)?
            .checked_add(
                added_abs
                    .checked_mul(fill_price as u128)
                    .ok_or(PerpRouterError::MathOverflow)?,
            )
            .ok_or(PerpRouterError::MathOverflow)?;
        let den = old_abs
            .checked_add(added_abs)
            .ok_or(PerpRouterError::MathOverflow)?
            .max(1);
        (num / den) as u64
    };
    pos.entry_price = blended_entry;
    pos.size_stored = pos
        .size_stored
        .checked_add(signed_size)
        .ok_or(PerpRouterError::MathOverflow)?;
    pos.margin_locked = pos
        .margin_locked
        .checked_add(margin_used)
        .ok_or(PerpRouterError::MathOverflow)?;
    Ok(())
}

/// Find the maker's `TraderAccount` in `remaining_accounts` by deriving
/// its PDA and matching pubkeys.
fn find_maker_account<'a, 'info>(
    remaining: &'a [AccountInfo<'info>],
    maker_id: &Pubkey,
    program_id: &Pubkey,
) -> Result<&'a AccountInfo<'info>, ProgramError> {
    let (expected_pda, _) = find_trader_account_address(maker_id, program_id);
    remaining
        .iter()
        .find(|a| a.key == &expected_pda)
        .ok_or_else(|| PerpRouterError::MakerAccountMissing.into())
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
    let trader_account_info = next_account_info(it)?;
    let perp_market_info = next_account_info(it)?;
    let orderbook_info = next_account_info(it)?;
    // Anything left is the variable-length maker-account tail.
    let remaining_accounts: &[AccountInfo] = it.as_slice();

    assert_with_msg(
        trader.is_signer,
        ProgramError::MissingRequiredSignature,
        "trader must sign PlaceOrderPerp",
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

    let max_leverage_bps = {
        let buf = perp_market_info.try_borrow_data()?;
        let m = bytemuck::from_bytes::<PerpMarket>(&buf[..size_of::<PerpMarket>()]);
        m.max_leverage_bps
    };

    // Worst-case margin pre-check (full size posts uncrossed). If the
    // trader has the headroom for the entire order resting, they
    // definitely have enough for the fill+rest mix.
    let worst_quote_notional = p
        .price_in_ticks
        .checked_mul(p.num_base_lots)
        .ok_or(PerpRouterError::MathOverflow)?;
    let worst_margin = notional_to_margin(worst_quote_notional, max_leverage_bps)?;
    {
        let buf = trader_account_info.try_borrow_data()?;
        let t = bytemuck::from_bytes::<TraderAccount>(&buf[..size_of::<TraderAccount>()]);
        assert_with_msg(
            &t.owner == trader.key,
            PerpRouterError::InvalidAuthority,
            "trader_account.owner != signer",
        )?;
        let free = t.collateral.saturating_sub(t.locked_margin);
        assert_with_msg(
            free >= worst_margin,
            PerpRouterError::InsufficientCollateral,
            "free collateral cannot cover worst-case margin for this order",
        )?;
    }

    let order_packet = OrderPacket::new_limit_order(
        side,
        p.price_in_ticks,
        p.num_base_lots,
        SelfTradeBehavior::CancelProvide,
        None,
        p.client_order_id,
        false,
    );

    // Capture fill events on the stack so we can settle both sides of
    // every match after the matching engine returns and the orderbook
    // borrow is dropped.
    let mut fills: [CapturedFill; MAX_FILLS_PER_IX] = [CapturedFill::default(); MAX_FILLS_PER_IX];
    let mut fill_count: usize = 0;
    let mut overflow = false;

    let clock = Clock::get()?;
    let mut get_clock_fn = || (clock.slot, clock.unix_timestamp as u64);

    let (resting_order_id, response) = {
        let mut record_event_fn = |event: MarketEvent<Pubkey>| {
            if let MarketEvent::Fill {
                maker_id,
                price_in_ticks,
                base_lots_filled,
                ..
            } = event
            {
                if fill_count < MAX_FILLS_PER_IX {
                    fills[fill_count] = CapturedFill {
                        maker_id,
                        price_in_ticks: u64::from(price_in_ticks),
                        base_lots_filled: u64::from(base_lots_filled),
                    };
                    fill_count += 1;
                } else {
                    overflow = true;
                }
            }
        };

        let mut buf = orderbook_info.try_borrow_mut_data()?;
        let market =
            PerpOrderbook::load_mut_bytes(&mut buf).ok_or(ProgramError::InvalidAccountData)?;
        market
            .place_order(trader.key, order_packet, &mut record_event_fn, &mut get_clock_fn)
            .ok_or(PerpRouterError::OrderRejected)?
    };

    assert_with_msg(
        !overflow,
        PerpRouterError::TooManyFills,
        "matching engine emitted more fills than MAX_FILLS_PER_IX",
    )?;

    // ── Apply each fill: taker debits collateral, maker releases
    //    locked_margin; both sides get a position update.
    let market_key = *perp_market_info.key;
    for i in 0..fill_count {
        let fill = fills[i];
        let fill_notional = fill
            .price_in_ticks
            .checked_mul(fill.base_lots_filled)
            .ok_or(PerpRouterError::MathOverflow)?;
        let fill_margin = notional_to_margin(fill_notional, max_leverage_bps)?;

        // Taker side: Bid → long (+size). Ask → short (-size).
        let taker_signed: i64 = match side {
            Side::Bid => fill.base_lots_filled as i64,
            Side::Ask => -(fill.base_lots_filled as i64),
        };
        {
            let mut buf = trader_account_info.try_borrow_mut_data()?;
            let t = bytemuck::from_bytes_mut::<TraderAccount>(
                &mut buf[..size_of::<TraderAccount>()],
            );
            t.collateral = t
                .collateral
                .checked_sub(fill_margin)
                .ok_or(PerpRouterError::InsufficientCollateral)?;
            blend_fill_into_position(t, &market_key, taker_signed, fill.price_in_ticks, fill_margin)?;
        }

        // Maker side: opposite sign from taker. Their resting order
        // backed the locked_margin reservation; release it here, transfer
        // into the new position's margin_locked.
        let maker_account = find_maker_account(remaining_accounts, &fill.maker_id, program_id)?;
        assert_with_msg(
            maker_account.owner == &crate::ID || maker_account.owner == &DELEGATION_PROGRAM_ID,
            ProgramError::IllegalOwner,
            "maker TraderAccount ownership mismatch",
        )?;
        {
            let mut buf = maker_account.try_borrow_mut_data()?;
            let m = bytemuck::from_bytes_mut::<TraderAccount>(
                &mut buf[..size_of::<TraderAccount>()],
            );
            m.locked_margin = m
                .locked_margin
                .checked_sub(fill_margin)
                .ok_or(PerpRouterError::InsufficientCollateral)?;
            blend_fill_into_position(m, &market_key, -taker_signed, fill.price_in_ticks, fill_margin)?;
        }
    }

    // ── Posted (uncrossed) portion: reserve margin from free collateral
    //    into locked_margin. Same logic as Stage 3a.
    let posted_quote_notional: u64 = match side {
        Side::Bid => u64::from(response.num_quote_lots_posted),
        Side::Ask => p
            .price_in_ticks
            .checked_mul(u64::from(response.num_base_lots_posted))
            .ok_or(PerpRouterError::MathOverflow)?,
    };
    let posted_margin = notional_to_margin(posted_quote_notional, max_leverage_bps)?;
    if posted_margin > 0 {
        let mut buf = trader_account_info.try_borrow_mut_data()?;
        let t = bytemuck::from_bytes_mut::<TraderAccount>(&mut buf[..size_of::<TraderAccount>()]);
        t.locked_margin = t
            .locked_margin
            .checked_add(posted_margin)
            .ok_or(PerpRouterError::MathOverflow)?;
    }

    msg!(
        "PlaceOrderPerp ok: side={:?} fills={} resting_id={:?} posted_b={} posted_q={} posted_margin+={}",
        side,
        fill_count,
        resting_order_id,
        u64::from(response.num_base_lots_posted),
        u64::from(response.num_quote_lots_posted),
        posted_margin,
    );
    Ok(())
}
