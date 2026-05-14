//! DelegateTraderAccount — base layer.
//!
//! Hands the TraderAccount PDA over to the MagicBlock delegation program so
//! the user can transact on the ER. Mirrors
//! `phoenix-v1/src/program/processor/delegate_market.rs` exactly, with seeds
//! `[TRADER_ACCOUNT_SEED, owner]`.
//!
//! Account list:
//!   [0] owner               (signer, writable, payer)
//!   [1] system_programyes
//!   [2] trader_account      (writable; currently perp_router-owned)
//!   [3] owner_program       (= perp_router; readonly)
//!   [4] delegation_buffer   (writable)
//!   [5] delegation_record   (writable)
//!   [6] delegation_metadata (writable)
//!   [7] delegation_program

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
    constants::TRADER_ACCOUNT_SEED,
    error::{assert_with_msg, PerpRouterError},
    state::TraderAccount,
    validation::loaders::find_trader_account_address,
};

#[derive(BorshDeserialize, BorshSerialize)]
pub struct DelegateTraderAccountParams {
    /// Pin to a specific MagicBlock validator (`None` = any).
    pub validator: Option<Pubkey>,
}

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let DelegateTraderAccountParams { validator } =
        DelegateTraderAccountParams::try_from_slice(data)?;

    let it = &mut accounts.iter();
    let owner = next_account_info(it)?;
    let system_program_info = next_account_info(it)?;
    let trader_info = next_account_info(it)?;
    let owner_program = next_account_info(it)?;
    let delegation_buffer = next_account_info(it)?;
    let delegation_record = next_account_info(it)?;
    let delegation_metadata = next_account_info(it)?;
    let delegation_program = next_account_info(it)?;

    assert_with_msg(
        owner.is_signer,
        ProgramError::MissingRequiredSignature,
        "owner must sign DelegateTraderAccount",
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
        trader_info.owner == &crate::ID,
        ProgramError::IllegalOwner,
        "TraderAccount must currently be perp_router-owned (not already delegated)",
    )?;

    // Re-derive the PDA seeds from the account itself to prove the seeds we
    // sign with are canonical.
    let (recorded_owner, stored_bump) = {
        let buf = trader_info.try_borrow_data()?;
        let t = bytemuck::from_bytes::<TraderAccount>(&buf[..size_of::<TraderAccount>()]);
        (t.owner, t.bump)
    };
    assert_with_msg(
        &recorded_owner == owner.key,
        PerpRouterError::InvalidAuthority,
        "Caller is not the recorded TraderAccount.owner",
    )?;
    let (expected, derived_bump) = find_trader_account_address(owner.key, program_id);
    assert_with_msg(
        &expected == trader_info.key && derived_bump == stored_bump,
        PerpRouterError::InvalidPda,
        "TraderAccount PDA / bump mismatch",
    )?;

    let pda_seeds: &[&[u8]] = &[TRADER_ACCOUNT_SEED, owner.key.as_ref()];

    delegate_account(
        DelegateAccounts {
            payer: owner,
            pda: trader_info,
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
