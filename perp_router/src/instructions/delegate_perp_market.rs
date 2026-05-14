//! DelegatePerpMarket — base layer, admin.
//! Hands a PerpMarket PDA to the delegation program.

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
    constants::PERP_MARKET_SEED,
    error::{assert_with_msg, PerpRouterError},
    state::PerpMarket,
    validation::loaders::find_perp_market_address,
};

#[derive(BorshDeserialize, BorshSerialize)]
pub struct DelegatePerpMarketParams {
    pub validator: Option<Pubkey>,
}

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let DelegatePerpMarketParams { validator } =
        DelegatePerpMarketParams::try_from_slice(data)?;

    let it = &mut accounts.iter();
    let admin = next_account_info(it)?;
    let system_program_info = next_account_info(it)?;
    let perp_market_info = next_account_info(it)?;
    let owner_program = next_account_info(it)?;
    let delegation_buffer = next_account_info(it)?;
    let delegation_record = next_account_info(it)?;
    let delegation_metadata = next_account_info(it)?;
    let delegation_program = next_account_info(it)?;

    assert_with_msg(
        admin.is_signer,
        ProgramError::MissingRequiredSignature,
        "admin must sign DelegatePerpMarket",
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
        perp_market_info.owner == &crate::ID,
        ProgramError::IllegalOwner,
        "PerpMarket must currently be perp_router-owned",
    )?;

    let (recorded_phoenix_market, recorded_authority, stored_bump) = {
        let buf = perp_market_info.try_borrow_data()?;
        let m = bytemuck::from_bytes::<PerpMarket>(&buf[..size_of::<PerpMarket>()]);
        (m.phoenix_market, m.authority, m.bump)
    };
    assert_with_msg(
        &recorded_authority == admin.key,
        PerpRouterError::InvalidAuthority,
        "Caller is not the recorded PerpMarket.authority",
    )?;
    let (expected, derived_bump) =
        find_perp_market_address(&recorded_phoenix_market, program_id);
    assert_with_msg(
        &expected == perp_market_info.key && derived_bump == stored_bump,
        PerpRouterError::InvalidPda,
        "PerpMarket PDA / bump mismatch",
    )?;

    let pda_seeds: &[&[u8]] =
        &[PERP_MARKET_SEED, recorded_phoenix_market.as_ref()];
    delegate_account(
        DelegateAccounts {
            payer: admin,
            pda: perp_market_info,
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
