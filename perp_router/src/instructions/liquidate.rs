//! Liquidate — ER, permissionless cranker.
//!
//! Closes an underwater position at the envelope-clamped oracle price. If
//! the position's collateral fails to cover the loss, the shortfall is
//! absorbed by ALL positions proportionally via Percolator's A index — no
//! per-account ADL loop, no winners singled out.
//!
//! v1 implementation: this instruction currently only mutates the
//! `GlobalState.{A, B}` indices to book a stated shortfall. The CPI to
//! Phoenix to actually close the position lives in `open_position.rs` /
//! `close_position.rs`; an under-margined position should be force-closed
//! by the crank, which then calls into this for the residual.
//!
//! Account list:
//!   [0] caller        (signer; crank)
//!   [1] global_state  (writable; delegated)
//!   [2] perp_market   (readonly; for total_oi)

use borsh::{BorshDeserialize, BorshSerialize};
use ephemeral_rollups_sdk::consts::DELEGATION_PROGRAM_ID;
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    program_error::ProgramError,
    pubkey::Pubkey,
};
use std::mem::size_of;

use crate::{
    error::{assert_with_msg, PerpRouterError},
    risk::side_index::apply_shortfall,
    state::{GlobalState, PerpMarket},
    validation::loaders::{find_global_state_address, find_perp_market_address},
};

#[derive(BorshSerialize, BorshDeserialize)]
pub struct LiquidateParams {
    /// Collateral shortfall in base units that needs to be absorbed by the
    /// pool. Set by the crank after it has force-closed a position and
    /// computed the deficit. Zero means: just trigger a recovery_check
    /// pass without booking a loss.
    pub shortfall: u64,
}

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let LiquidateParams { shortfall } = LiquidateParams::try_from_slice(data)?;

    let it = &mut accounts.iter();
    let caller = next_account_info(it)?;
    let global_state_info = next_account_info(it)?;
    let perp_market_info = next_account_info(it)?;

    assert_with_msg(
        caller.is_signer,
        ProgramError::MissingRequiredSignature,
        "caller must sign Liquidate",
    )?;
    assert_with_msg(
        global_state_info.owner == &crate::ID
            || global_state_info.owner == &DELEGATION_PROGRAM_ID,
        ProgramError::IllegalOwner,
        "global_state ownership mismatch",
    )?;

    let total_oi_abs = {
        let buf = perp_market_info.try_borrow_data()?;
        let m = bytemuck::from_bytes::<PerpMarket>(&buf[..size_of::<PerpMarket>()]);
        let (expected, _) = find_perp_market_address(&m.phoenix_market, program_id);
        assert_with_msg(
            &expected == perp_market_info.key,
            PerpRouterError::InvalidPda,
            "perp_market PDA mismatch",
        )?;
        m.get_open_interest().unsigned_abs() as u64
    };

    if shortfall == 0 {
        return Ok(());
    }

    let mut buf = global_state_info.try_borrow_mut_data()?;
    let g = bytemuck::from_bytes_mut::<GlobalState>(&mut buf[..size_of::<GlobalState>()]);
    let (expected, _) = find_global_state_address(program_id);
    assert_with_msg(
        &expected == global_state_info.key,
        PerpRouterError::InvalidPda,
        "global_state PDA mismatch",
    )?;

    let (a_new, b_new) = apply_shortfall(g.get_a(), g.get_b(), shortfall, total_oi_abs)?;
    g.set_a(a_new);
    g.set_b(b_new);
    // Pool value drops by the shortfall as well — that's the realized loss
    // on the books.
    g.v_total_pool_value = g.v_total_pool_value.saturating_sub(shortfall);
    Ok(())
}
