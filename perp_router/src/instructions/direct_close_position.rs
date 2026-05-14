//! DirectClosePosition — oracle-priced synthetic close (no Phoenix CPI).
//!
//! Reads entry_price + margin_locked from the on-chain Position (set on
//! open). PnL = effective_size × (clamped − entry); margin refunded;
//! gain → reserve (warmup); loss charged against margin then collateral.
//!
//! Account list:
//!   [0] trader            (signer)
//!   [1] trader_account    (writable)
//!   [2] global_state      (readable)
//!   [3] perp_market       (writable)

use borsh::{BorshDeserialize, BorshSerialize};
use ephemeral_rollups_sdk::consts::DELEGATION_PROGRAM_ID;
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    clock::Clock,
    entrypoint::ProgramResult,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvar::Sysvar,
};
use std::mem::size_of;

use crate::{
    constants::WARMUP_SLOTS,
    error::{assert_with_msg, PerpRouterError},
    risk::{
        envelope::safe_oracle_read,
        side_index::{effective_size, position_funding_owed},
        warmup::push_reserve,
    },
    state::{trader_account::Position, GlobalState, PerpMarket, TraderAccount},
    validation::loaders::{
        find_global_state_address, find_perp_market_address, find_trader_account_address,
    },
};

#[derive(BorshSerialize, BorshDeserialize)]
pub struct DirectClosePositionParams {
    pub mark_price: u64,
}

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let DirectClosePositionParams { mark_price } = DirectClosePositionParams::try_from_slice(data)?;

    let it = &mut accounts.iter();
    let trader = next_account_info(it)?;
    let trader_account_info = next_account_info(it)?;
    let global_state_info = next_account_info(it)?;
    let perp_market_info = next_account_info(it)?;

    assert_with_msg(
        trader.is_signer,
        ProgramError::MissingRequiredSignature,
        "trader must sign",
    )?;
    for info in [trader_account_info, global_state_info, perp_market_info] {
        assert_with_msg(
            info.owner == &crate::ID || info.owner == &DELEGATION_PROGRAM_ID,
            ProgramError::IllegalOwner,
            "ownership mismatch",
        )?;
    }

    let current_slot = Clock::get()?.slot;

    let (clamped_price, current_funding) = {
        let buf = perp_market_info.try_borrow_data()?;
        let m = bytemuck::from_bytes::<PerpMarket>(&buf[..size_of::<PerpMarket>()]);
        let (expected, _) = find_perp_market_address(&m.phoenix_market, program_id);
        assert_with_msg(
            &expected == perp_market_info.key,
            PerpRouterError::InvalidPda,
            "perp_market PDA mismatch",
        )?;
        let dt = current_slot.saturating_sub(m.last_oracle_slot);
        let clamped = safe_oracle_read(mark_price, m.last_oracle_price, dt, m.max_bps_per_slot)?;
        (clamped, m.get_funding_index())
    };

    let a = {
        let buf = global_state_info.try_borrow_data()?;
        let g = bytemuck::from_bytes::<GlobalState>(&buf[..size_of::<GlobalState>()]);
        let (expected, _) = find_global_state_address(program_id);
        assert_with_msg(
            &expected == global_state_info.key,
            PerpRouterError::InvalidPda,
            "global_state PDA mismatch",
        )?;
        g.get_a()
    };

    let stored_size_at_close = {
        let mut buf = trader_account_info.try_borrow_mut_data()?;
        let t = bytemuck::from_bytes_mut::<TraderAccount>(
            &mut buf[..size_of::<TraderAccount>()],
        );
        let (expected, _) = find_trader_account_address(&t.owner, program_id);
        assert_with_msg(
            &expected == trader_account_info.key,
            PerpRouterError::InvalidPda,
            "trader_account PDA mismatch",
        )?;
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
        let stored = t.positions[i].size_stored;
        let entry_price = t.positions[i].entry_price;
        let margin_locked = t.positions[i].margin_locked;
        let entry_f = i128::from_le_bytes(t.positions[i].entry_funding_index);
        let eff = effective_size(stored, a)?;
        let funding = position_funding_owed(eff, current_funding, entry_f)?;
        let price_diff = (clamped_price as i128) - (entry_price as i128);
        let gross = (eff as i128)
            .checked_mul(price_diff)
            .ok_or(PerpRouterError::MathOverflow)?;

        t.collateral = t
            .collateral
            .checked_add(margin_locked)
            .ok_or(PerpRouterError::MathOverflow)?;

        let pnl_after_funding = gross - funding;
        if pnl_after_funding > 0 {
            let mature_slot = current_slot.saturating_add(WARMUP_SLOTS);
            push_reserve(
                &mut t.pnl_reserve,
                &mut t.pnl_reserve_len,
                pnl_after_funding as u64,
                mature_slot,
            )?;
        } else if pnl_after_funding < 0 {
            let loss = pnl_after_funding.unsigned_abs() as u64;
            t.collateral = t.collateral.saturating_sub(loss);
        }

        t.positions[i] = Position::default();
        let last = (t.positions_len as usize) - 1;
        if i != last {
            t.positions[i] = t.positions[last];
            t.positions[last] = Position::default();
        }
        t.positions_len = t.positions_len.saturating_sub(1);

        stored
    };

    {
        let mut buf = perp_market_info.try_borrow_mut_data()?;
        let m = bytemuck::from_bytes_mut::<PerpMarket>(&mut buf[..size_of::<PerpMarket>()]);
        let oi = m
            .get_open_interest()
            .checked_sub(stored_size_at_close as i128)
            .ok_or(PerpRouterError::SideIndexOverflow)?;
        m.set_open_interest(oi);
        m.last_oracle_price = clamped_price;
        m.last_oracle_slot = current_slot;
    }
    Ok(())
}
