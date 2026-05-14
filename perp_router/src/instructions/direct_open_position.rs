//! DirectOpenPosition — oracle-priced synthetic open.
//!
//! Runs on the ER when accounts are delegated; on base otherwise. No
//! Phoenix CPI — settlement is purely against `TraderAccount`. Used for
//! ER trading demos before a real Phoenix market exists on the ER.
//!
//! Account list:
//!   [0] trader            (signer)
//!   [1] trader_account    (writable; delegated)
//!   [2] global_state      (writable; delegated)
//!   [3] perp_market       (writable; delegated)

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
    constants::MAX_POSITIONS,
    error::{assert_with_msg, PerpRouterError},
    risk::{envelope::safe_oracle_read, recovery::opens_allowed, side_index::effective_size},
    state::{trader_account::Position, GlobalState, PerpMarket, TraderAccount},
    validation::loaders::{
        find_global_state_address, find_perp_market_address, find_trader_account_address,
    },
};

#[derive(BorshSerialize, BorshDeserialize)]
pub struct DirectOpenPositionParams {
    pub size: i64,
    pub mark_price: u64,
    pub margin: u64,
}

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let p = DirectOpenPositionParams::try_from_slice(data)?;
    assert_with_msg(
        p.size != 0 && p.margin > 0,
        ProgramError::InvalidInstructionData,
        "size and margin must be non-zero",
    )?;

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

    let g_recovery = {
        let buf = global_state_info.try_borrow_data()?;
        let g = bytemuck::from_bytes::<GlobalState>(&buf[..size_of::<GlobalState>()]);
        let (expected, _) = find_global_state_address(program_id);
        assert_with_msg(
            &expected == global_state_info.key,
            PerpRouterError::InvalidPda,
            "global_state PDA mismatch",
        )?;
        g.recovery_state
    };
    assert_with_msg(
        opens_allowed(g_recovery),
        PerpRouterError::RecoveryDrainOnly,
        "recovery state forbids opens",
    )?;

    let clamped_price = {
        let buf = perp_market_info.try_borrow_data()?;
        let m = bytemuck::from_bytes::<PerpMarket>(&buf[..size_of::<PerpMarket>()]);
        let (expected, _) = find_perp_market_address(&m.phoenix_market, program_id);
        assert_with_msg(
            &expected == perp_market_info.key,
            PerpRouterError::InvalidPda,
            "perp_market PDA mismatch",
        )?;
        let dt = current_slot.saturating_sub(m.last_oracle_slot);
        safe_oracle_read(p.mark_price, m.last_oracle_price, dt, m.max_bps_per_slot)?
    };

    {
        let mut t_buf = trader_account_info.try_borrow_mut_data()?;
        let t = bytemuck::from_bytes_mut::<TraderAccount>(
            &mut t_buf[..size_of::<TraderAccount>()],
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
        assert_with_msg(
            t.collateral >= p.margin,
            PerpRouterError::InsufficientCollateral,
            "insufficient collateral",
        )?;

        // leverage check
        let notional = (p.size.unsigned_abs() as u128)
            .checked_mul(clamped_price as u128)
            .ok_or(PerpRouterError::MathOverflow)?;
        let buf2 = perp_market_info.try_borrow_data()?;
        let m = bytemuck::from_bytes::<PerpMarket>(&buf2[..size_of::<PerpMarket>()]);
        let max_notional = (p.margin as u128)
            .checked_mul(m.max_leverage_bps as u128)
            .ok_or(PerpRouterError::MathOverflow)?
            / 10_000u128;
        assert_with_msg(
            notional <= max_notional,
            PerpRouterError::LeverageExceeded,
            "notional exceeds max leverage",
        )?;

        t.collateral = t.collateral.saturating_sub(p.margin);

        // slot
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

        let p_pos = &mut t.positions[slot.unwrap()];
        let old_abs = p_pos.size_stored.unsigned_abs() as u128;
        let add_abs = p.size.unsigned_abs() as u128;
        let blended_entry = if old_abs == 0 {
            clamped_price
        } else {
            let num = old_abs
                .checked_mul(p_pos.entry_price as u128)
                .ok_or(PerpRouterError::MathOverflow)?
                .checked_add(
                    add_abs
                        .checked_mul(clamped_price as u128)
                        .ok_or(PerpRouterError::MathOverflow)?,
                )
                .ok_or(PerpRouterError::MathOverflow)?;
            let den = old_abs
                .checked_add(add_abs)
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
            .checked_add(p.size)
            .ok_or(PerpRouterError::MathOverflow)?;

        let g_buf = global_state_info.try_borrow_data()?;
        let g = bytemuck::from_bytes::<GlobalState>(&g_buf[..size_of::<GlobalState>()]);
        let _eff = effective_size(p_pos.size_stored, g.get_a())?;
        p_pos.last_epoch_seen = g.epoch;
        p_pos.entry_funding_index = m.funding_index;
    }

    {
        let mut buf = perp_market_info.try_borrow_mut_data()?;
        let m = bytemuck::from_bytes_mut::<PerpMarket>(&mut buf[..size_of::<PerpMarket>()]);
        let oi = m
            .get_open_interest()
            .checked_add(p.size as i128)
            .ok_or(PerpRouterError::SideIndexOverflow)?;
        m.set_open_interest(oi);
        m.last_oracle_price = clamped_price;
        m.last_oracle_slot = current_slot;
    }
    Ok(())
}
