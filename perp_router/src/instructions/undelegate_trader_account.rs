//! UndelegateTraderAccount — ER.
//!
//! Commits final TraderAccount state and queues undelegation back to base.
//! No post-undelegate action is needed; the delegation program will
//! transfer ownership back to perp_router on the base layer when the
//! callback fires.
//!
//! Account list:
//!   [0] payer            (signer, writable; commonly the session key or owner)
//!   [1] trader_account   (writable; expected owner = delegation program on ER)
//!   [2] magic_program    (Magic111...)
//!   [3] magic_context    (MagicContext1...; writable)

use ephemeral_rollups_sdk::{
    consts::{DELEGATION_PROGRAM_ID, MAGIC_CONTEXT_ID, MAGIC_PROGRAM_ID},
    ephem::commit_and_undelegate_accounts,
};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    program_error::ProgramError,
    pubkey::Pubkey,
};

use crate::error::assert_with_msg;

pub fn process(_program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let it = &mut accounts.iter();
    let payer = next_account_info(it)?;
    let trader_info = next_account_info(it)?;
    let magic_program = next_account_info(it)?;
    let magic_context = next_account_info(it)?;

    assert_with_msg(
        payer.is_signer,
        ProgramError::MissingRequiredSignature,
        "payer must sign UndelegateTraderAccount",
    )?;
    assert_with_msg(
        magic_program.key == &MAGIC_PROGRAM_ID,
        ProgramError::IncorrectProgramId,
        "Invalid MagicBlock program id",
    )?;
    assert_with_msg(
        magic_context.key == &MAGIC_CONTEXT_ID,
        ProgramError::InvalidArgument,
        "Invalid MagicBlock context id",
    )?;
    assert_with_msg(
        trader_info.owner == &crate::ID || trader_info.owner == &DELEGATION_PROGRAM_ID,
        ProgramError::IllegalOwner,
        "TraderAccount must be perp_router (ER) or delegation-owned (base)",
    )?;

    commit_and_undelegate_accounts(
        payer,
        vec![trader_info],
        magic_context,
        magic_program,
        None,
    )?;
    Ok(())
}
