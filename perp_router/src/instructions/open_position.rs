//! OpenPosition — ER hot path. **Phoenix-matched** (v1.1).
//!
//! Long-only via Phoenix `Swap` (Bid). Shorts via a different mechanism
//! ship in v1.2 (Swap is take-only; perp_router would need base inventory
//! up-front to sell, or a separate borrow primitive).
//!
//! Flow:
//!   1. Gate on `GlobalState.recovery_state == Normal`.
//!   2. Envelope-clamp `mark_price` (used only for OI bookkeeping + entry
//!      price record — actual fill price comes from Phoenix).
//!   3. Verify margin × leverage ≥ quote-to-spend.
//!   4. Snapshot `collateral_vault` and `perp_base_vault` balances pre-swap.
//!   5. CPI `cpi_swap_take(Bid, ...)` — perp_authority signs.
//!   6. Re-read both vaults — compute `quote_spent` and `base_received`.
//!   7. Slippage check: `base_received >= min_base_lots_to_receive`.
//!   8. Debit `margin` from `TraderAccount.collateral`; record Position.
//!   9. Bump `PerpMarket.open_interest` and `last_oracle_price`.
//!
//! Account list (13):
//!   [0]  trader              (signer)
//!   [1]  trader_account      (writable, delegated)
//!   [2]  global_state        (writable, delegated)
//!   [3]  perp_market         (writable, delegated)
//!   [4]  perp_authority      (readonly; PDA — signs CPI)
//!   [5]  collateral_vault    (writable; perp_authority quote ATA)
//!   [6]  perp_base_vault     (writable; perp_authority base ATA)
//!   [7]  phoenix_program
//!   [8]  phoenix_log_authority
//!   [9]  phoenix_market      (writable)
//!   [10] phoenix_base_vault  (writable)
//!   [11] phoenix_quote_vault (writable)
//!   [12] token_program

use borsh::{BorshDeserialize, BorshSerialize};
use ephemeral_rollups_sdk::consts::DELEGATION_PROGRAM_ID;
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    clock::Clock,
    entrypoint::ProgramResult,
    program_error::ProgramError,
    program_pack::Pack,
    pubkey::Pubkey,
    sysvar::Sysvar,
};
use spl_token::state::Account as TokenAccount;
use std::mem::size_of;

use crate::{
    constants::MAX_POSITIONS,
    cpi::phoenix::{cpi_swap_take, Side},
    error::{assert_with_msg, PerpRouterError},
    risk::{envelope::safe_oracle_read, recovery::opens_allowed, side_index::effective_size},
    state::{
        trader_account::Position, vault::find_collateral_vault_address, GlobalState, PerpMarket,
        TraderAccount,
    },
    validation::loaders::{
        find_global_state_address, find_perp_authority_address, find_perp_market_address,
        find_trader_account_address,
    },
};

#[derive(BorshSerialize, BorshDeserialize)]
pub struct OpenPositionParams {
    /// Collateral to lock for this position.
    pub margin: u64,
    /// Maximum quote lots the swap may spend.
    pub num_quote_lots_to_spend: u64,
    /// Slippage floor: refuse swap if it fills less base than this.
    pub min_base_lots_to_receive: u64,
    /// Client-supplied mark price (envelope-clamped, used for OI bookkeeping).
    pub mark_price: u64,
    /// Opaque client-supplied id, forwarded to Phoenix for trade tracking.
    pub client_order_id: u128,
}

fn token_balance(acc: &AccountInfo) -> Result<u64, ProgramError> {
    let data = acc.try_borrow_data()?;
    let a = TokenAccount::unpack(&data)?;
    Ok(a.amount)
}

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let p = OpenPositionParams::try_from_slice(data)?;
    assert_with_msg(
        p.margin > 0 && p.num_quote_lots_to_spend > 0,
        ProgramError::InvalidInstructionData,
        "OpenPosition: margin and num_quote_lots must be > 0",
    )?;

    let it = &mut accounts.iter();
    let trader = next_account_info(it)?;
    let trader_account_info = next_account_info(it)?;
    let global_state_info = next_account_info(it)?;
    let perp_market_info = next_account_info(it)?;
    let perp_authority = next_account_info(it)?;
    let collateral_vault = next_account_info(it)?;
    let perp_base_vault = next_account_info(it)?;
    let phoenix_program = next_account_info(it)?;
    let phoenix_log_authority = next_account_info(it)?;
    let phoenix_market = next_account_info(it)?;
    let phoenix_base_vault = next_account_info(it)?;
    let phoenix_quote_vault = next_account_info(it)?;
    let token_program = next_account_info(it)?;

    assert_with_msg(
        trader.is_signer,
        ProgramError::MissingRequiredSignature,
        "trader must sign OpenPosition",
    )?;
    for info in [trader_account_info, global_state_info, perp_market_info] {
        assert_with_msg(
            info.owner == &crate::ID || info.owner == &DELEGATION_PROGRAM_ID,
            ProgramError::IllegalOwner,
            "delegated account ownership mismatch",
        )?;
    }

    // --- PDA validation ---
    let (expected_auth, _) = find_perp_authority_address(program_id);
    assert_with_msg(
        perp_authority.key == &expected_auth,
        PerpRouterError::InvalidPda,
        "perp_authority PDA mismatch",
    )?;

    let current_slot = Clock::get()?.slot;

    // --- 1. Recovery gate ---
    let g_recovery = {
        let buf = global_state_info.try_borrow_data()?;
        let g = bytemuck::from_bytes::<GlobalState>(&buf[..size_of::<GlobalState>()]);
        let (expected, _) = find_global_state_address(program_id);
        assert_with_msg(
            &expected == global_state_info.key,
            PerpRouterError::InvalidPda,
            "GlobalState PDA mismatch",
        )?;
        g.recovery_state
    };
    assert_with_msg(
        opens_allowed(g_recovery),
        PerpRouterError::RecoveryDrainOnly,
        "Recovery state forbids opening new positions",
    )?;

    // --- 2. Envelope-clamp mark + cross-check market wiring ---
    let (clamped_price, base_mint, quote_mint, ph_base_vault_key, ph_quote_vault_key) = {
        let buf = perp_market_info.try_borrow_data()?;
        let m = bytemuck::from_bytes::<PerpMarket>(&buf[..size_of::<PerpMarket>()]);
        let (expected, _) = find_perp_market_address(&m.phoenix_market, program_id);
        assert_with_msg(
            &expected == perp_market_info.key,
            PerpRouterError::InvalidPda,
            "PerpMarket PDA mismatch",
        )?;
        assert_with_msg(
            &m.phoenix_market == phoenix_market.key,
            PerpRouterError::InvalidPda,
            "phoenix_market doesn't match PerpMarket.phoenix_market",
        )?;
        let dt = current_slot.saturating_sub(m.last_oracle_slot);
        let clamped = safe_oracle_read(p.mark_price, m.last_oracle_price, dt, m.max_bps_per_slot)?;
        (
            clamped,
            m.base_mint,
            m.quote_mint,
            m.phoenix_base_vault,
            m.phoenix_quote_vault,
        )
    };
    assert_with_msg(
        phoenix_base_vault.key == &ph_base_vault_key,
        PerpRouterError::InvalidPda,
        "phoenix_base_vault mismatch",
    )?;
    assert_with_msg(
        phoenix_quote_vault.key == &ph_quote_vault_key,
        PerpRouterError::InvalidPda,
        "phoenix_quote_vault mismatch",
    )?;
    assert_with_msg(
        collateral_vault.key == &find_collateral_vault_address(&quote_mint, program_id),
        PerpRouterError::InvalidPda,
        "collateral_vault ATA mismatch",
    )?;
    assert_with_msg(
        perp_base_vault.key == &find_collateral_vault_address(&base_mint, program_id),
        PerpRouterError::InvalidPda,
        "perp_base_vault ATA mismatch",
    )?;

    // --- 3. Leverage check ---
    {
        let buf = trader_account_info.try_borrow_data()?;
        let t = bytemuck::from_bytes::<TraderAccount>(&buf[..size_of::<TraderAccount>()]);
        assert_with_msg(
            &t.owner == trader.key,
            PerpRouterError::InvalidAuthority,
            "trader_account.owner != signer",
        )?;
        assert_with_msg(
            t.collateral >= p.margin,
            PerpRouterError::InsufficientCollateral,
            "insufficient collateral for margin",
        )?;
        // leverage in bps: notional ≤ margin × max_leverage_bps / 10_000
        // notional ≈ num_quote_lots_to_spend (in quote lots; client must
        // pre-scale to leverage). We compare quote_lots × 10_000 ≤ margin × bps.
        // For simplicity here we use the on-PerpMarket cap.
        let buf2 = perp_market_info.try_borrow_data()?;
        let m = bytemuck::from_bytes::<PerpMarket>(&buf2[..size_of::<PerpMarket>()]);
        let max_notional = (p.margin as u128)
            .checked_mul(m.max_leverage_bps as u128)
            .ok_or(PerpRouterError::MathOverflow)?
            / 10_000u128;
        assert_with_msg(
            (p.num_quote_lots_to_spend as u128) <= max_notional,
            PerpRouterError::LeverageExceeded,
            "quote-to-spend exceeds margin × max_leverage",
        )?;
    }

    // --- 4. Snapshot vault balances pre-swap ---
    let q0 = token_balance(collateral_vault)?;
    let b0 = token_balance(perp_base_vault)?;

    // --- 5. CPI Phoenix Swap (Bid = buy base with quote) ---
    cpi_swap_take(
        program_id,
        phoenix_program,
        phoenix_log_authority,
        phoenix_market,
        perp_authority,
        perp_base_vault,
        collateral_vault,
        phoenix_base_vault,
        phoenix_quote_vault,
        token_program,
        Side::Bid,
        0, // num_base_lots — unconstrained; capped by quote spend
        p.num_quote_lots_to_spend,
        p.min_base_lots_to_receive,
        0, // min_quote_lots_to_fill (unused for Bid)
        p.client_order_id,
    )?;

    // --- 6. Read post-swap balances ---
    let q1 = token_balance(collateral_vault)?;
    let b1 = token_balance(perp_base_vault)?;
    let quote_spent = q0
        .checked_sub(q1)
        .ok_or(PerpRouterError::MathOverflow)?;
    let base_received = b1
        .checked_sub(b0)
        .ok_or(PerpRouterError::MathOverflow)?;

    // --- 7. Slippage check ---
    assert_with_msg(
        base_received >= p.min_base_lots_to_receive,
        PerpRouterError::LeverageExceeded, // reuse — TODO: dedicated Slippage err
        "Phoenix fill below min_base_lots_to_receive",
    )?;

    // --- 8. Mutate TraderAccount: debit margin, record Position ---
    {
        let mut buf = trader_account_info.try_borrow_mut_data()?;
        let t = bytemuck::from_bytes_mut::<TraderAccount>(
            &mut buf[..size_of::<TraderAccount>()],
        );
        let (expected, _) = find_trader_account_address(&t.owner, program_id);
        assert_with_msg(
            &expected == trader_account_info.key,
            PerpRouterError::InvalidPda,
            "TraderAccount PDA mismatch",
        )?;
        t.collateral = t.collateral.saturating_sub(p.margin);

        // Find / allocate slot.
        let mut slot: Option<usize> = None;
        for i in 0..(t.positions_len as usize) {
            if t.positions[i].market == *perp_market_info.key {
                slot = Some(i);
                break;
            }
        }
        if slot.is_none() {
            if (t.positions_len as usize) >= MAX_POSITIONS {
                return Err(PerpRouterError::PositionTableFull.into());
            }
            slot = Some(t.positions_len as usize);
            t.positions_len += 1;
            t.positions[slot.unwrap()] = Position::default();
            t.positions[slot.unwrap()].market = *perp_market_info.key;
        }

        let added_size = base_received as i64;
        let added_quote = quote_spent;
        let p_pos = &mut t.positions[slot.unwrap()];
        let old_abs = p_pos.size_stored.unsigned_abs() as u128;
        let added_abs = added_size.unsigned_abs() as u128;
        // VWAP entry: weighted by base-lot abs size.
        let fill_avg_price = if base_received == 0 {
            clamped_price // shouldn't happen — slippage check above
        } else {
            (added_quote as u128 / base_received.max(1) as u128) as u64
        };
        let blended_entry = if old_abs == 0 {
            fill_avg_price
        } else {
            let num = old_abs
                .checked_mul(p_pos.entry_price as u128)
                .ok_or(PerpRouterError::MathOverflow)?
                .checked_add(
                    added_abs
                        .checked_mul(fill_avg_price as u128)
                        .ok_or(PerpRouterError::MathOverflow)?,
                )
                .ok_or(PerpRouterError::MathOverflow)?;
            let den = old_abs
                .checked_add(added_abs)
                .ok_or(PerpRouterError::MathOverflow)?;
            (num / den.max(1)) as u64
        };
        p_pos.entry_price = blended_entry;
        p_pos.margin_locked = p_pos
            .margin_locked
            .checked_add(p.margin)
            .ok_or(PerpRouterError::MathOverflow)?;
        p_pos.size_stored = p_pos
            .size_stored
            .checked_add(added_size)
            .ok_or(PerpRouterError::MathOverflow)?;

        let g_buf = global_state_info.try_borrow_data()?;
        let g = bytemuck::from_bytes::<GlobalState>(&g_buf[..size_of::<GlobalState>()]);
        let _eff = effective_size(p_pos.size_stored, g.get_a())?;
        p_pos.last_epoch_seen = g.epoch;
        let m_buf = perp_market_info.try_borrow_data()?;
        let m = bytemuck::from_bytes::<PerpMarket>(&m_buf[..size_of::<PerpMarket>()]);
        p_pos.entry_funding_index = m.funding_index;
    }

    // --- 9. Update PerpMarket OI + last mark ---
    {
        let mut buf = perp_market_info.try_borrow_mut_data()?;
        let m = bytemuck::from_bytes_mut::<PerpMarket>(&mut buf[..size_of::<PerpMarket>()]);
        let oi = m
            .get_open_interest()
            .checked_add(base_received as i128)
            .ok_or(PerpRouterError::SideIndexOverflow)?;
        m.set_open_interest(oi);
        m.last_oracle_price = clamped_price;
        m.last_oracle_slot = current_slot;
    }
    Ok(())
}
