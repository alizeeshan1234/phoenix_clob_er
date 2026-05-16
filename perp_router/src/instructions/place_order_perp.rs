//! PlaceOrderPerp — ER hot path. Pushes a limit order into the in-tree
//! Phoenix matching engine on `PerpOrderbook`, captures fills, and
//! settles both taker and maker `TraderAccount` state — including PnL
//! realization on closing fills.
//!
//! Stage 3c scope (this revision):
//!   - Posted portion (no cross): reserves margin from `collateral`
//!     into `locked_margin` (Stage 3a).
//!   - Filled portion (crosses resting liquidity): for each fill,
//!     `apply_fill_to_position` dispatches by case:
//!       * OPEN / SCALE-UP (same sign or first entry): VWAP-blend the
//!         fill into positions[market] and consume margin from the
//!         appropriate source (taker → free collateral; maker →
//!         locked_margin reservation).
//!       * CLOSE (opposite sign, |fill| ≤ |existing|): realize PnL on
//!         the closed portion (profit → pnl_reserve with WARMUP_SLOTS,
//!         loss → debit collateral), release proportional
//!         position.margin_locked back to collateral. The fill itself
//!         consumes no new margin since it's an unwind. For makers, the
//!         resting order's locked_margin is always released regardless.
//!       * FLIP (opposite sign, |fill| > |existing|): close fully (as
//!         above), then open the remainder in the opposite direction
//!         at fill_price with proportional margin.
//!
//! Funding accrual and Percolator side-index (A-coefficient) effects on
//! PnL are NOT applied here — Stage 3d gap. `DirectClosePosition` still
//! has the full risk-engine flow.
//!
//! Unit assumption: in v1 the orderbook is configured with
//! `tick_size=1, base_lots_per_base_unit=1` so prices in ticks map 1:1
//! to oracle/quote units. Heterogeneous lot params will need a scaling
//! step in `notional_to_margin`.
//!
//! Account list:
//!   [0] trader         (signer)
//!   [1] trader_account (writable, delegated)
//!   [2] perp_market    (readonly; for max_leverage_bps + orderbook PDA)
//!   [3] orderbook      (writable, delegated)
//!   [4..] maker_accounts (writable, delegated) — one TraderAccount per
//!         maker the caller expects to fill against.

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
    constants::{MAX_POSITIONS, WARMUP_SLOTS},
    error::{assert_with_msg, PerpRouterError},
    risk::warmup::push_reserve,
    state::{trader_account::Position, PerpMarket, PerpOrderbook, TraderAccount},
    validation::loaders::{find_orderbook_address, find_trader_account_address},
};

/// Max fills the engine may emit per call. Bounded by Solana's per-tx
/// account cap — each fill needs the maker's TraderAccount in
/// remaining-accounts.
const MAX_FILLS_PER_IX: usize = 8;

#[derive(BorshSerialize, BorshDeserialize)]
pub struct PlaceOrderPerpParams {
    pub side: u8,
    pub price_in_ticks: u64,
    pub num_base_lots: u64,
    pub client_order_id: u128,
}

#[derive(Copy, Clone, Default)]
struct CapturedFill {
    maker_id: Pubkey,
    price_in_ticks: u64,
    base_lots_filled: u64,
}

/// Where the margin for an incoming fill comes from. Taker → debit free
/// `collateral`. Maker → consume the existing `locked_margin` reservation
/// that backed the resting order.
#[derive(Copy, Clone, Eq, PartialEq)]
enum MarginSource {
    FreeCollateral,
    LockedMargin,
}

/// `quote_notional × 10_000 / max_leverage_bps`. Assumes tick / lot
/// params of 1:1 (see file header).
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

/// Find a position slot for `market`; allocate a new slot if absent.
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

/// Settle one fill into `t.positions[market]`, moving margin between
/// `collateral` / `locked_margin` / `position.margin_locked` as the case
/// requires and realizing PnL on any closed portion. See file header
/// for the case dispatch.
fn apply_fill_to_position(
    t: &mut TraderAccount,
    market: &Pubkey,
    signed_size: i64,
    fill_price: u64,
    fill_margin: u64,
    source: MarginSource,
    current_slot: u64,
) -> ProgramResult {
    // Maker side: always release the resting order's reservation first.
    if source == MarginSource::LockedMargin {
        t.locked_margin = t
            .locked_margin
            .checked_sub(fill_margin)
            .ok_or(PerpRouterError::MathOverflow)?;
    }

    let slot = position_slot(t, market)?;
    let became_zero = {
        let pos = &mut t.positions[slot];
        let old = pos.size_stored;
        let same_sign = old == 0 || ((old > 0) == (signed_size > 0));

        if same_sign {
        // ── OPEN or SCALE-UP ──────────────────────────────────────────
        // Taker pays from collateral; maker's reservation already
        // released — its `fill_margin` worth conceptually transfers to
        // `position.margin_locked`, no further collateral move needed.
        if source == MarginSource::FreeCollateral {
            t.collateral = t
                .collateral
                .checked_sub(fill_margin)
                .ok_or(PerpRouterError::InsufficientCollateral)?;
        }

        let old_abs = old.unsigned_abs() as u128;
        let added_abs = signed_size.unsigned_abs() as u128;
        pos.entry_price = if old_abs == 0 {
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
        pos.size_stored = pos
            .size_stored
            .checked_add(signed_size)
            .ok_or(PerpRouterError::MathOverflow)?;
        pos.margin_locked = pos
            .margin_locked
            .checked_add(fill_margin)
            .ok_or(PerpRouterError::MathOverflow)?;
        false
    } else {
        // ── CLOSE (or CLOSE + FLIP) ───────────────────────────────────
        let old_abs = old.unsigned_abs();
        let fill_abs = signed_size.unsigned_abs();
        let closed = old_abs.min(fill_abs);

        // PnL on closed portion. Long closed by selling at higher
        // price → profit. Short closed by buying at lower → profit.
        let pnl_per_unit: i128 = if old > 0 {
            (fill_price as i128) - (pos.entry_price as i128)
        } else {
            (pos.entry_price as i128) - (fill_price as i128)
        };
        let pnl_raw: i128 = pnl_per_unit
            .checked_mul(closed as i128)
            .ok_or(PerpRouterError::MathOverflow)?;

        // Proportional release of existing position margin → collateral.
        let released = ((pos.margin_locked as u128)
            .checked_mul(closed as u128)
            .ok_or(PerpRouterError::MathOverflow)?
            / (old_abs as u128).max(1)) as u64;
        pos.margin_locked = pos.margin_locked.saturating_sub(released);
        t.collateral = t
            .collateral
            .checked_add(released)
            .ok_or(PerpRouterError::MathOverflow)?;

        // PnL: profit → reserve with warmup. Loss → debit collateral
        // (capped at zero — bankruptcy is silent for v1; v2 should
        // mark the trader liquidatable).
        if pnl_raw > 0 {
            push_reserve(
                &mut t.pnl_reserve,
                &mut t.pnl_reserve_len,
                pnl_raw as u64,
                current_slot.saturating_add(WARMUP_SLOTS),
            )?;
        } else if pnl_raw < 0 {
            let loss = pnl_raw.unsigned_abs() as u64;
            t.collateral = t.collateral.saturating_sub(loss);
        }

        // Update size for the closed portion.
        let new_size = old
            .checked_add(signed_size)
            .ok_or(PerpRouterError::MathOverflow)?;
        pos.size_stored = new_size;

        if new_size == 0 {
            // Pure full close: zero metadata so the post-scope compaction
            // step can drop this slot via swap-with-last.
            pos.entry_price = 0;
            pos.margin_locked = 0;
        } else if fill_abs > old_abs {
            // Flip: leftover opens opposite direction at fill_price.
            let leftover_abs = fill_abs - old_abs;
            let leftover_margin = ((fill_margin as u128)
                .checked_mul(leftover_abs as u128)
                .ok_or(PerpRouterError::MathOverflow)?
                / (fill_abs as u128).max(1)) as u64;
            if source == MarginSource::FreeCollateral {
                t.collateral = t
                    .collateral
                    .checked_sub(leftover_margin)
                    .ok_or(PerpRouterError::InsufficientCollateral)?;
            }
            pos.entry_price = fill_price;
            pos.margin_locked = leftover_margin;
        }
        // Else: partial close — entry_price preserved for the remaining
        // same-sign exposure.
        new_size == 0
        }
    };

    // Slot compaction. Mirrors DirectClosePosition's swap-with-last so
    // a trader churning across markets doesn't stall at the MAX_POSITIONS
    // cap with zombie zero-size slots. Only the close branch can produce
    // `became_zero` — open/scale-up always sets size_stored ≠ 0, and the
    // flip path keeps the slot occupied with the opposite-direction
    // remainder.
    if became_zero {
        let last = (t.positions_len as usize).saturating_sub(1);
        if slot != last {
            t.positions[slot] = t.positions[last];
        }
        t.positions[last] = Position::default();
        t.positions_len = t.positions_len.saturating_sub(1);
    }

    Ok(())
}

/// Locate the maker's `TraderAccount` in `remaining` by deriving the
/// expected PDA from `maker_id`.
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

    // Worst-case margin pre-check (full size posts uncrossed). Closing
    // fills release margin rather than consume it, so this strictly
    // over-estimates — safe.
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

    let mut fills: [CapturedFill; MAX_FILLS_PER_IX] = [CapturedFill::default(); MAX_FILLS_PER_IX];
    let mut fill_count: usize = 0;
    let mut overflow = false;

    let clock = Clock::get()?;
    let current_slot = clock.slot;
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

    // ── Settle every captured fill on both taker and maker sides.
    let market_key = *perp_market_info.key;
    for i in 0..fill_count {
        let fill = fills[i];
        let fill_notional = fill
            .price_in_ticks
            .checked_mul(fill.base_lots_filled)
            .ok_or(PerpRouterError::MathOverflow)?;
        let fill_margin = notional_to_margin(fill_notional, max_leverage_bps)?;

        let taker_signed: i64 = match side {
            Side::Bid => fill.base_lots_filled as i64,
            Side::Ask => -(fill.base_lots_filled as i64),
        };

        {
            let mut buf = trader_account_info.try_borrow_mut_data()?;
            let t = bytemuck::from_bytes_mut::<TraderAccount>(
                &mut buf[..size_of::<TraderAccount>()],
            );
            apply_fill_to_position(
                t,
                &market_key,
                taker_signed,
                fill.price_in_ticks,
                fill_margin,
                MarginSource::FreeCollateral,
                current_slot,
            )?;
        }

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
            apply_fill_to_position(
                m,
                &market_key,
                -taker_signed,
                fill.price_in_ticks,
                fill_margin,
                MarginSource::LockedMargin,
                current_slot,
            )?;
        }
    }

    // ── Posted (uncrossed) portion: reserve margin into locked_margin
    //    (Stage 3a). Closing fills don't post; only fresh outstanding
    //    quote sits on the book.
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
