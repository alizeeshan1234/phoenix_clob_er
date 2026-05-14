//! InitializeTrader — base layer, user.
//!
//! Allocates a TraderAccount PDA seeded `[TRADER_ACCOUNT_SEED, owner]`.
//! Idempotent in spirit but fails loudly if already initialised — clients
//! should branch on this.
//!
//! Account list:
//!   [0] owner            (signer, writable, payer)
//!   [1] system_program
//!   [2] trader_account   (writable, system-owned uninit)

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
    constants::TRADER_ACCOUNT_SEED,
    error::{assert_with_msg, PerpRouterError},
    state::TraderAccount,
    system_utils::create_account,
    validation::loaders::find_trader_account_address,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let it = &mut accounts.iter();
    let owner = next_account_info(it)?;
    let system_program_info = next_account_info(it)?;
    let trader_info = next_account_info(it)?;

    assert_with_msg(
        owner.is_signer,
        ProgramError::MissingRequiredSignature,
        "owner must sign InitializeTrader",
    )?;
    assert_with_msg(
        system_program_info.key == &system_program::ID,
        ProgramError::IncorrectProgramId,
        "system_program account mismatch",
    )?;

    let (expected, bump) = find_trader_account_address(owner.key, program_id);
    assert_with_msg(
        trader_info.key == &expected,
        PerpRouterError::InvalidPda,
        "trader_account PDA mismatch",
    )?;
    assert_with_msg(
        trader_info.owner == &system_program::ID && trader_info.data_is_empty(),
        PerpRouterError::AlreadyInitialized,
        "TraderAccount already initialised",
    )?;

    let rent = Rent::get()?;
    let seeds: Vec<Vec<u8>> = vec![
        TRADER_ACCOUNT_SEED.to_vec(),
        owner.key.as_ref().to_vec(),
        vec![bump],
    ];
    create_account(
        owner,
        trader_info,
        system_program_info,
        program_id,
        &rent,
        size_of::<TraderAccount>() as u64,
        seeds,
    )?;

    let mut buf = trader_info.try_borrow_mut_data()?;
    let t = bytemuck::from_bytes_mut::<TraderAccount>(&mut buf[..size_of::<TraderAccount>()]);
    *t = TraderAccount::default();
    t.owner = *owner.key;
    t.bump = bump;
    Ok(())
}
