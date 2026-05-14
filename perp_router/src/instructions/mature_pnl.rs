//! MaturePnl — ER, crank target (~every slot).
//!
//! Sweeps a single TraderAccount's `pnl_reserve` entries whose `mature_slot`
//! has passed into `pnl_matured`. Bumps `GlobalState.total_matured_pnl`.
//!
//! Permissionless — anyone (including the crank) can call this.
//!
//! Account list:
//!   [0] caller          (signer; cranker or anyone)
//!   [1] trader_account  (writable; delegated TraderAccount)
//!   [2] global_state    (writable; delegated GlobalState)
//!   [3] clock           (Sysvar)

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
    error::{assert_with_msg, PerpRouterError},
    risk::warmup::sweep_matured,
    state::{GlobalState, TraderAccount},
    validation::loaders::{find_global_state_address, find_trader_account_address},
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let it = &mut accounts.iter();
    let caller = next_account_info(it)?;
    let trader_account_info = next_account_info(it)?;
    let global_state_info = next_account_info(it)?;

    assert_with_msg(
        caller.is_signer,
        ProgramError::MissingRequiredSignature,
        "caller must sign MaturePnl",
    )?;
    for info in [trader_account_info, global_state_info] {
        assert_with_msg(
            info.owner == &crate::ID || info.owner == &DELEGATION_PROGRAM_ID,
            ProgramError::IllegalOwner,
            "delegated account ownership mismatch",
        )?;
    }

    let current_slot = Clock::get()?.slot;

    // Sweep + accumulate.
    let matured = {
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
        let m = sweep_matured(&mut t.pnl_reserve, &mut t.pnl_reserve_len, current_slot);
        if m > 0 {
            t.pnl_matured = t
                .pnl_matured
                .checked_add(m)
                .ok_or(PerpRouterError::MathOverflow)?;
        }
        m
    };

    // Bump GlobalState.total_matured_pnl.
    if matured > 0 {
        let mut buf = global_state_info.try_borrow_mut_data()?;
        let g = bytemuck::from_bytes_mut::<GlobalState>(
            &mut buf[..size_of::<GlobalState>()],
        );
        let (expected, _) = find_global_state_address(program_id);
        assert_with_msg(
            &expected == global_state_info.key,
            PerpRouterError::InvalidPda,
            "GlobalState PDA mismatch",
        )?;
        g.total_matured_pnl = g
            .total_matured_pnl
            .checked_add(matured)
            .ok_or(PerpRouterError::MathOverflow)?;
    }
    Ok(())
}
