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
//!   [2] perp_market    (readonly; for max_leverage_bps + funding_index)
//!   [3] orderbook      (writable, delegated)
//!   [4] global_state   (readonly; for A coefficient — read-only here
//!                       so it can stay un-delegated on base, mirroring
//!                       DirectOpen/Close's treatment)
//!   [5..] maker_accounts (writable, delegated) — one TraderAccount per
//!         maker the caller expects to fill against.

use borsh::{BorshDeserialize, BorshSerialize};
use ephemeral_rollups_sdk::consts::DELEGATION_PROGRAM_ID;
use phoenix::{
    quantities::WrapperU64,
    state::{
        markets::{
            market_events::MarketEvent,
            market_traits::{Market, WritableMarket},
        },
        order_packet::OrderPacket,
        SelfTradeBehavior, Side,
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
use std::mem::size_of;

use crate::{
    constants::{MAX_POSITIONS, WARMUP_SLOTS},
    error::{assert_with_msg, PerpRouterError},
    risk::{
        side_index::{effective_size, position_funding_owed},
        warmup::push_reserve,
    },
    state::{trader_account::Position, GlobalState, PerpMarket, PerpOrderbook, TraderAccount},
    validation::loaders::{
        find_global_state_address, find_orderbook_address, find_trader_account_address,
    },
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

/// Convert (base_lots, price_in_ticks) to quote_lots using the matching
/// engine's accounting formula:
///   quote_lots = base_lots × price_in_ticks × tick_size_in_quote_lots_per_base_unit
///                / base_lots_per_base_unit
/// All arithmetic in u128 so high price × size doesn't overflow before
/// the divide. The earlier fast-path (`base_lots × price`) only worked
/// when tick_size = base_lots_per_base_unit = 1.
fn quote_lots_from_base_at_price(
    base_lots: u64,
    price_in_ticks: u64,
    tick_size_qlpbupt: u64,
    base_lots_per_base_unit: u64,
) -> Result<u64, ProgramError> {
    assert_with_msg(
        base_lots_per_base_unit > 0,
        ProgramError::InvalidAccountData,
        "base_lots_per_base_unit must be > 0",
    )?;
    let n: u128 = (base_lots as u128)
        .checked_mul(price_in_ticks as u128)
        .ok_or(PerpRouterError::MathOverflow)?
        .checked_mul(tick_size_qlpbupt as u128)
        .ok_or(PerpRouterError::MathOverflow)?;
    let q = n / (base_lots_per_base_unit as u128);
    if q > u64::MAX as u128 {
        return Err(PerpRouterError::MathOverflow.into());
    }
    Ok(q as u64)
}

/// Quote lots → required margin (in atoms). For v1 we assume
/// quote_lot_size = 1 (i.e. 1 quote lot = 1 collateral atom). Markets
/// with a different quote-lot scaling will need a per-market
/// `quote_lot_size` field on PerpMarket — currently a follow-up.
fn quote_lots_to_margin(quote_lots: u64, max_leverage_bps: u32) -> Result<u64, ProgramError> {
    assert_with_msg(
        max_leverage_bps > 0,
        ProgramError::InvalidAccountData,
        "PerpMarket.max_leverage_bps must be > 0",
    )?;
    quote_lots
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
#[allow(clippy::too_many_arguments)]
fn apply_fill_to_position(
    t: &mut TraderAccount,
    market: &Pubkey,
    signed_size: i64,
    fill_price: u64,
    fill_margin: u64,
    source: MarginSource,
    current_slot: u64,
    a_coefficient: i128,
    current_funding_index: i128,
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

        // Scale-up: settle accrued funding on the existing (effective)
        // size before re-anchoring. Without this the new index would
        // make the engine retroactively charge funding on the added
        // size from before it existed. Fresh-open (old_abs == 0) has
        // nothing to settle — just set the anchor below.
        if old_abs > 0 {
            let entry_f = i128::from_le_bytes(pos.entry_funding_index);
            let eff_old = effective_size(old, a_coefficient)?;
            let funding =
                position_funding_owed(eff_old, current_funding_index, entry_f)?;
            if funding > 0 {
                let loss = funding.unsigned_abs() as u64;
                t.collateral = t.collateral.saturating_sub(loss);
            } else if funding < 0 {
                push_reserve(
                    &mut t.pnl_reserve,
                    &mut t.pnl_reserve_len,
                    funding.unsigned_abs() as u64,
                    current_slot.saturating_add(WARMUP_SLOTS),
                )?;
            }
        }

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
        // Re-anchor the funding index for the combined position. Pre-existing
        // funding was settled above; from here, funding accrues against the
        // new (combined) size from `current_funding_index` onward.
        pos.entry_funding_index = current_funding_index.to_le_bytes();
        false
    } else {
        // ── CLOSE (or CLOSE + FLIP) ───────────────────────────────────
        let old_abs = old.unsigned_abs();
        let fill_abs = signed_size.unsigned_abs();
        let closed = old_abs.min(fill_abs);

        // Apply Percolator's lazy A-coefficient + accrued funding on the
        // closed portion, mirroring `direct_close_position`:
        //
        //   eff_closed = effective_size(closed_signed_stored, A)
        //   gross      = eff_closed × (fill_price − entry_price)
        //   funding    = position_funding_owed(eff_closed, F_now, F_entry)
        //   pnl_net    = gross − funding
        //
        // `eff_closed` is signed (negative for shorts) so `gross` comes
        // out with the right sign automatically.
        let closed_signed_stored: i64 = if old > 0 {
            closed as i64
        } else {
            -(closed as i64)
        };
        let eff_closed = effective_size(closed_signed_stored, a_coefficient)?;
        let price_diff = (fill_price as i128)
            .checked_sub(pos.entry_price as i128)
            .ok_or(PerpRouterError::MathOverflow)?;
        let gross: i128 = (eff_closed as i128)
            .checked_mul(price_diff)
            .ok_or(PerpRouterError::MathOverflow)?;
        let entry_f = i128::from_le_bytes(pos.entry_funding_index);
        let funding = position_funding_owed(eff_closed, current_funding_index, entry_f)?;
        let pnl_net: i128 = gross.checked_sub(funding).ok_or(PerpRouterError::MathOverflow)?;

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

        // pnl_net > 0 → reserve with warmup. pnl_net < 0 → debit
        // collateral (saturating at zero; bankruptcy is silent in v1).
        if pnl_net > 0 {
            push_reserve(
                &mut t.pnl_reserve,
                &mut t.pnl_reserve_len,
                pnl_net as u64,
                current_slot.saturating_add(WARMUP_SLOTS),
            )?;
        } else if pnl_net < 0 {
            let loss = pnl_net.unsigned_abs() as u64;
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
            // Flipped position is freshly opened; anchor funding here.
            pos.entry_funding_index = current_funding_index.to_le_bytes();
        }
        // Else: partial close — entry_price + entry_funding_index
        // preserved for the remaining same-sign exposure (still anchored
        // at its original entry).
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
    let global_state_info = next_account_info(it)?;
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

    let (max_leverage_bps, current_funding_index) = {
        let buf = perp_market_info.try_borrow_data()?;
        let m = bytemuck::from_bytes::<PerpMarket>(&buf[..size_of::<PerpMarket>()]);
        (m.max_leverage_bps, m.get_funding_index())
    };

    // A coefficient from GlobalState. Read-only here, so it can stay
    // un-delegated on base (ER serves a replicated clone).
    let a_coefficient = {
        let buf = global_state_info.try_borrow_data()?;
        let g = bytemuck::from_bytes::<GlobalState>(&buf[..size_of::<GlobalState>()]);
        let (expected, _) = find_global_state_address(program_id);
        assert_with_msg(
            &expected == global_state_info.key,
            PerpRouterError::InvalidPda,
            "GlobalState PDA mismatch",
        )?;
        g.get_a()
    };

    // Lot params live on the FIFOMarket. Pull them once so margin math
    // doesn't have to keep re-borrowing the 9 KB orderbook account.
    let (tick_size_qlpbupt, base_lots_per_base_unit) = {
        let buf = orderbook_info.try_borrow_data()?;
        let market =
            PerpOrderbook::load_bytes(&buf).ok_or(ProgramError::InvalidAccountData)?;
        (
            u64::from(market.get_tick_size()),
            u64::from(market.get_base_lots_per_base_unit()),
        )
    };

    // Worst-case margin pre-check (full size posts uncrossed). Closing
    // fills release margin rather than consume it, so this strictly
    // over-estimates — safe.
    let worst_quote_lots = quote_lots_from_base_at_price(
        p.num_base_lots,
        p.price_in_ticks,
        tick_size_qlpbupt,
        base_lots_per_base_unit,
    )?;
    let worst_margin = quote_lots_to_margin(worst_quote_lots, max_leverage_bps)?;
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
        let fill_quote_lots = quote_lots_from_base_at_price(
            fill.base_lots_filled,
            fill.price_in_ticks,
            tick_size_qlpbupt,
            base_lots_per_base_unit,
        )?;
        let fill_margin = quote_lots_to_margin(fill_quote_lots, max_leverage_bps)?;

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
                a_coefficient,
                current_funding_index,
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
                a_coefficient,
                current_funding_index,
            )?;
        }
    }

    // ── Posted (uncrossed) portion: reserve margin into locked_margin
    //    (Stage 3a). Closing fills don't post; only fresh outstanding
    //    quote sits on the book. Bid side: engine reports
    //    `num_quote_lots_posted` directly. Ask side: engine reports base
    //    lots, so we convert via the lot-aware formula.
    let posted_quote_lots: u64 = match side {
        Side::Bid => u64::from(response.num_quote_lots_posted),
        Side::Ask => quote_lots_from_base_at_price(
            u64::from(response.num_base_lots_posted),
            p.price_in_ticks,
            tick_size_qlpbupt,
            base_lots_per_base_unit,
        )?,
    };
    let posted_margin = quote_lots_to_margin(posted_quote_lots, max_leverage_bps)?;
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
