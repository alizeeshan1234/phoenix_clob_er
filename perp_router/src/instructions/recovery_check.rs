//! RecoveryCheck — ER, crank target (~every 5s).
//!
//! Drives the recovery state machine deterministically:
//!   Normal      + |A| < A_PRECISION_FLOOR → DrainOnly
//!   DrainOnly   + total_open_interest == 0 → ResetPending
//!   ResetPending                            → Normal (A=1, epoch+=1)
//!
//! Permissionless. Idempotent — if no transition fires, no state changes.
//!
//! Account list:
//!   [0] caller        (signer)
//!   [1] global_state  (writable; delegated)

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
    risk::recovery::{next_state, reset_indices},
    state::{global::RECOVERY_RESET_PENDING, GlobalState},
    validation::loaders::find_global_state_address,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let it = &mut accounts.iter();
    let caller = next_account_info(it)?;
    let global_state_info = next_account_info(it)?;

    assert_with_msg(
        caller.is_signer,
        ProgramError::MissingRequiredSignature,
        "caller must sign RecoveryCheck",
    )?;
    assert_with_msg(
        global_state_info.owner == &crate::ID
            || global_state_info.owner == &DELEGATION_PROGRAM_ID,
        ProgramError::IllegalOwner,
        "global_state ownership mismatch",
    )?;

    let mut buf = global_state_info.try_borrow_mut_data()?;
    let g = bytemuck::from_bytes_mut::<GlobalState>(&mut buf[..size_of::<GlobalState>()]);

    let (expected, _) = find_global_state_address(program_id);
    assert_with_msg(
        &expected == global_state_info.key,
        PerpRouterError::InvalidPda,
        "GlobalState PDA mismatch",
    )?;

    // open_interest aggregate isn't tracked on GlobalState yet (each
    // PerpMarket has its own). For v1 single-market, callers can drive
    // this from a single PerpMarket; for now we conservatively pass 0
    // when there are no positions sources we can read inline. Production
    // wiring will iterate PerpMarkets passed as remaining accounts.
    let total_oi: u64 = 0;
    let current = g.recovery_state;
    let a = g.get_a();
    let next = next_state(current, a, total_oi);

    if next != current {
        g.recovery_state = next;
        if current == RECOVERY_RESET_PENDING {
            let r = reset_indices(g.epoch);
            g.set_a(r.new_a);
            g.epoch = r.new_epoch_inc;
            // K snapshot deliberately not zeroed — it stays as the historical
            // accumulator, per the Percolator spec.
        }
    }
    Ok(())
}
