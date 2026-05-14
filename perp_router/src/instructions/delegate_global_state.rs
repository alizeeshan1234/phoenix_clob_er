//! DelegateGlobalState — base layer, admin.
//! Hands the singleton GlobalState to the delegation program.

use borsh::{BorshDeserialize, BorshSerialize};
use ephemeral_rollups_sdk::{
    consts::DELEGATION_PROGRAM_ID,
    cpi::{delegate_account, DelegateAccounts, DelegateConfig},
};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    program_error::ProgramError,
    pubkey::Pubkey,
};
use std::mem::size_of;

use crate::{
    constants::GLOBAL_STATE_SEED,
    error::{assert_with_msg, PerpRouterError},
    state::GlobalState,
    validation::loaders::find_global_state_address,
};

#[derive(BorshDeserialize, BorshSerialize)]
pub struct DelegateGlobalStateParams {
    pub validator: Option<Pubkey>,
}

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let DelegateGlobalStateParams { validator } =
        DelegateGlobalStateParams::try_from_slice(data)?;

    let it = &mut accounts.iter();
    let admin = next_account_info(it)?;
    let system_program_info = next_account_info(it)?;
    let global_state_info = next_account_info(it)?;
    let owner_program = next_account_info(it)?;
    let delegation_buffer = next_account_info(it)?;
    let delegation_record = next_account_info(it)?;
    let delegation_metadata = next_account_info(it)?;
    let delegation_program = next_account_info(it)?;

    assert_with_msg(
        admin.is_signer,
        ProgramError::MissingRequiredSignature,
        "admin must sign DelegateGlobalState",
    )?;
    assert_with_msg(
        owner_program.key == &crate::ID,
        ProgramError::IncorrectProgramId,
        "owner_program must be perp_router",
    )?;
    assert_with_msg(
        delegation_program.key == &DELEGATION_PROGRAM_ID,
        ProgramError::IncorrectProgramId,
        "delegation_program account mismatch",
    )?;
    assert_with_msg(
        global_state_info.owner == &crate::ID,
        ProgramError::IllegalOwner,
        "GlobalState must currently be perp_router-owned",
    )?;

    let (recorded_admin, stored_bump) = {
        let buf = global_state_info.try_borrow_data()?;
        let g = bytemuck::from_bytes::<GlobalState>(&buf[..size_of::<GlobalState>()]);
        (g.admin, g.bump)
    };
    assert_with_msg(
        &recorded_admin == admin.key,
        PerpRouterError::InvalidAuthority,
        "Caller is not the recorded GlobalState.admin",
    )?;
    let (expected, derived_bump) = find_global_state_address(program_id);
    assert_with_msg(
        &expected == global_state_info.key && derived_bump == stored_bump,
        PerpRouterError::InvalidPda,
        "GlobalState PDA / bump mismatch",
    )?;

    let pda_seeds: &[&[u8]] = &[GLOBAL_STATE_SEED];
    delegate_account(
        DelegateAccounts {
            payer: admin,
            pda: global_state_info,
            owner_program,
            buffer: delegation_buffer,
            delegation_record,
            delegation_metadata,
            delegation_program,
            system_program: system_program_info,
        },
        pda_seeds,
        DelegateConfig {
            commit_frequency_ms: u32::MAX,
            validator,
        },
    )?;
    Ok(())
}
