//! InitializeGlobalState — base layer, admin, one-shot.
//!
//! Allocates the singleton `GlobalState` PDA, initialises A = FIXED_POINT_ONE
//! (1.0 in fixed-point), K = F = B = 0, recovery_state = Normal, epoch = 0.
//!
//! Account list:
//!   [0] admin            (signer, writable, payer)
//!   [1] system_program
//!   [2] global_state     (writable, system-owned uninit; will become perp_router-owned)

use bytemuck::bytes_of_mut;
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    program_error::ProgramError,
    pubkey::Pubkey,
    rent::Rent,
    system_program,
    sysvar::Sysvar,
};
use std::mem::size_of;

use crate::{
    constants::{FIXED_POINT_ONE, GLOBAL_STATE_SEED},
    error::{assert_with_msg, PerpRouterError},
    state::{global::RECOVERY_NORMAL, GlobalState},
    system_utils::create_account,
    validation::loaders::find_global_state_address,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let it = &mut accounts.iter();
    let admin = next_account_info(it)?;
    let system_program_info = next_account_info(it)?;
    let global_state_info = next_account_info(it)?;

    assert_with_msg(
        admin.is_signer,
        ProgramError::MissingRequiredSignature,
        "admin must sign InitializeGlobalState",
    )?;
    assert_with_msg(
        system_program_info.key == &system_program::ID,
        ProgramError::IncorrectProgramId,
        "system_program account mismatch",
    )?;

    let (expected, bump) = find_global_state_address(program_id);
    assert_with_msg(
        global_state_info.key == &expected,
        PerpRouterError::InvalidPda,
        "global_state PDA mismatch",
    )?;
    assert_with_msg(
        global_state_info.owner == &system_program::ID && global_state_info.data_is_empty(),
        PerpRouterError::AlreadyInitialized,
        "GlobalState already initialised",
    )?;

    let rent = Rent::get()?;
    let seeds: Vec<Vec<u8>> = vec![GLOBAL_STATE_SEED.to_vec(), vec![bump]];
    create_account(
        admin,
        global_state_info,
        system_program_info,
        program_id,
        &rent,
        size_of::<GlobalState>() as u64,
        seeds,
    )?;

    let mut data = global_state_info.try_borrow_mut_data()?;
    let g = bytemuck::from_bytes_mut::<GlobalState>(&mut data[..size_of::<GlobalState>()]);
    *g = GlobalState::default();
    g.set_a(FIXED_POINT_ONE);
    g.recovery_state = RECOVERY_NORMAL;
    g.admin = *admin.key;
    g.bump = bump;
    // Touch bytes_of_mut to keep the lint happy if compiler decides bytemuck
    // import is otherwise unused after inlining.
    let _ = bytes_of_mut(g);
    Ok(())
}
