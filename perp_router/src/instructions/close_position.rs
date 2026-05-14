//! ClosePosition — ER hot path. **Phoenix-matched** (v1.1).
//!
//! Closes a long position by selling its base into Phoenix for quote.
//! PnL = quote_received − quote_locked_at_entry. The original margin is
//! returned to `TraderAccount.collateral`; gains land in `pnl_reserve` to
//! age through warmup. Losses are charged against the margin first, then
//! against collateral.
//!
//! Account list (13, same shape as `OpenPosition`):
//!   [0]  trader
//!   [1]  trader_account
//!   [2]  global_state
//!   [3]  perp_market
//!   [4]  perp_authority
//!   [5]  collateral_vault     (perp_authority's quote ATA)
//!   [6]  perp_base_vault      (perp_authority's base ATA)
//!   [7]  phoenix_program
//!   [8]  phoenix_log_authority
//!   [9]  phoenix_market
//!   [10] phoenix_base_vault
//!   [11] phoenix_quote_vault
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
    constants::WARMUP_SLOTS,
    cpi::phoenix::{cpi_swap_take, Side},
    error::{assert_with_msg, PerpRouterError},
    risk::{
        envelope::safe_oracle_read, side_index::{effective_size, position_funding_owed},
        warmup::push_reserve,
    },
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
pub struct ClosePositionParams {
    /// Slippage floor: refuse swap if it returns less quote than this.
    pub min_quote_lots_to_receive: u64,
    /// Client-supplied mark for envelope clamp + last_oracle_price bump.
    pub mark_price: u64,
    /// Opaque client-supplied id.
    pub client_order_id: u128,
}

fn token_balance(acc: &AccountInfo) -> Result<u64, ProgramError> {
    let data = acc.try_borrow_data()?;
    let a = TokenAccount::unpack(&data)?;
    Ok(a.amount)
}

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let p = ClosePositionParams::try_from_slice(data)?;

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
        "trader must sign ClosePosition",
    )?;
    for info in [trader_account_info, global_state_info, perp_market_info] {
        assert_with_msg(
            info.owner == &crate::ID || info.owner == &DELEGATION_PROGRAM_ID,
            ProgramError::IllegalOwner,
            "delegated account ownership mismatch",
        )?;
    }
    let (expected_auth, _) = find_perp_authority_address(program_id);
    assert_with_msg(
        perp_authority.key == &expected_auth,
        PerpRouterError::InvalidPda,
        "perp_authority PDA mismatch",
    )?;

    let current_slot = Clock::get()?.slot;

    // --- Envelope-clamp mark, read market metadata ---
    let (clamped_price, base_mint, quote_mint, ph_b_key, ph_q_key, current_funding) = {
        let buf = perp_market_info.try_borrow_data()?;
        let m = bytemuck::from_bytes::<PerpMarket>(&buf[..size_of::<PerpMarket>()]);
        let (expected, _) = find_perp_market_address(&m.phoenix_market, program_id);
        assert_with_msg(
            &expected == perp_market_info.key,
            PerpRouterError::InvalidPda,
            "PerpMarket PDA mismatch",
        )?;
        let dt = current_slot.saturating_sub(m.last_oracle_slot);
        let clamped = safe_oracle_read(p.mark_price, m.last_oracle_price, dt, m.max_bps_per_slot)?;
        (
            clamped,
            m.base_mint,
            m.quote_mint,
            m.phoenix_base_vault,
            m.phoenix_quote_vault,
            m.get_funding_index(),
        )
    };
    assert_with_msg(
        phoenix_base_vault.key == &ph_b_key && phoenix_quote_vault.key == &ph_q_key,
        PerpRouterError::InvalidPda,
        "phoenix vault keys don't match PerpMarket cache",
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

    // --- Locate position, read its size + margin + entry ---
    let (size_to_sell, margin_locked, _entry_price, entry_funding) = {
        let buf = trader_account_info.try_borrow_data()?;
        let t = bytemuck::from_bytes::<TraderAccount>(&buf[..size_of::<TraderAccount>()]);
        assert_with_msg(
            &t.owner == trader.key,
            PerpRouterError::InvalidAuthority,
            "trader_account.owner != signer",
        )?;
        let mut idx: Option<usize> = None;
        for i in 0..(t.positions_len as usize) {
            if t.positions[i].market == *perp_market_info.key {
                idx = Some(i);
                break;
            }
        }
        let i = idx.ok_or(PerpRouterError::NotInitialized)?;
        let pos = t.positions[i];
        assert_with_msg(
            pos.size_stored > 0,
            ProgramError::InvalidArgument,
            "v1.1 close: only long positions supported (size > 0)",
        )?;
        let entry_f = i128::from_le_bytes(pos.entry_funding_index);
        (
            pos.size_stored as u64,
            pos.margin_locked,
            pos.entry_price,
            entry_f,
        )
    };

    // --- Snapshot pre-swap balances ---
    let q0 = token_balance(collateral_vault)?;
    let b0 = token_balance(perp_base_vault)?;
    assert_with_msg(
        b0 >= size_to_sell,
        PerpRouterError::InsufficientCollateral,
        "perp_base_vault has insufficient base to cover close",
    )?;

    // --- CPI Phoenix Swap (Ask = sell base for quote) ---
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
        Side::Ask,
        size_to_sell,
        0, // num_quote_lots — unconstrained
        0, // min_base_lots_to_fill (Ask uses min_quote_lots floor below)
        p.min_quote_lots_to_receive,
        p.client_order_id,
    )?;

    // --- Post-swap balances ---
    let q1 = token_balance(collateral_vault)?;
    let b1 = token_balance(perp_base_vault)?;
    let quote_received = q1
        .checked_sub(q0)
        .ok_or(PerpRouterError::MathOverflow)?;
    let base_sold = b0
        .checked_sub(b1)
        .ok_or(PerpRouterError::MathOverflow)?;
    assert_with_msg(
        quote_received >= p.min_quote_lots_to_receive,
        PerpRouterError::LeverageExceeded, // TODO: dedicated Slippage err
        "Phoenix close-fill below min_quote_lots_to_receive",
    )?;
    let _ = base_sold; // (should equal size_to_sell)

    // --- Apply lazy A/K/F/B + funding to compute settled cashflow ---
    let a = {
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
    let eff = effective_size(size_to_sell as i64, a)?;
    let funding = position_funding_owed(eff, current_funding, entry_funding)?;

    // realized cashflow = quote_received − margin_locked (− funding if owed).
    // If positive, that's PnL → reserve. Margin always refunds.
    let cash_in: i128 = (quote_received as i128) - (margin_locked as i128) - funding;

    // --- Mutate TraderAccount: refund margin, push PnL or charge loss ---
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
        t.collateral = t
            .collateral
            .checked_add(margin_locked)
            .ok_or(PerpRouterError::MathOverflow)?;

        if cash_in > 0 {
            let mature_slot = current_slot.saturating_add(WARMUP_SLOTS);
            push_reserve(
                &mut t.pnl_reserve,
                &mut t.pnl_reserve_len,
                cash_in as u64,
                mature_slot,
            )?;
        } else if cash_in < 0 {
            let loss = cash_in.unsigned_abs() as u64;
            t.collateral = t.collateral.saturating_sub(loss);
        }

        // Zero + compact position table.
        let mut idx: Option<usize> = None;
        for i in 0..(t.positions_len as usize) {
            if t.positions[i].market == *perp_market_info.key {
                idx = Some(i);
                break;
            }
        }
        let i = idx.ok_or(PerpRouterError::NotInitialized)?;
        t.positions[i] = Position::default();
        let last = (t.positions_len as usize) - 1;
        if i != last {
            t.positions[i] = t.positions[last];
            t.positions[last] = Position::default();
        }
        t.positions_len = t.positions_len.saturating_sub(1);
    }

    // --- Decrement OI + bump last mark ---
    {
        let mut buf = perp_market_info.try_borrow_mut_data()?;
        let m = bytemuck::from_bytes_mut::<PerpMarket>(&mut buf[..size_of::<PerpMarket>()]);
        let oi = m
            .get_open_interest()
            .checked_sub(size_to_sell as i128)
            .ok_or(PerpRouterError::SideIndexOverflow)?;
        m.set_open_interest(oi);
        m.last_oracle_price = clamped_price;
        m.last_oracle_slot = current_slot;
    }
    Ok(())
}
