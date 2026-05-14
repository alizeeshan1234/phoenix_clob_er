//! CommitAndUndelegateMarket — ER-side instruction that snapshots final
//! state AND queues undelegation. After this lands on the ER, the
//! delegation program will call back into Phoenix on the base layer with
//! the `EXTERNAL_UNDELEGATE_DISCRIMINATOR` to finalize ownership transfer.
//!
//! Account list:
//!   [0] payer         (signer, writable)
//!   [1] market        (writable; expected owner = delegation program)
//!   [2] magic_program (Magic111...)
//!   [3] magic_context (MagicContext1...; writable)

use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    program_error::ProgramError,
    pubkey::Pubkey,
};

use ephemeral_rollups_sdk::{
    consts::{DELEGATION_PROGRAM_ID, MAGIC_CONTEXT_ID, MAGIC_PROGRAM_ID},
    ephem::commit_and_undelegate_accounts,
};

use crate::program::error::assert_with_msg;

pub(crate) fn process_commit_and_undelegate_market(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    _data: &[u8],
) -> ProgramResult {
    let account_iter = &mut accounts.iter();
    let payer_info = next_account_info(account_iter)?;
    let market_info = next_account_info(account_iter)?;
    let magic_program = next_account_info(account_iter)?;
    let magic_context = next_account_info(account_iter)?;

    assert_with_msg(
        payer_info.is_signer,
        ProgramError::MissingRequiredSignature,
        "payer must sign CommitAndUndelegateMarket",
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
        market_info.owner == &crate::ID || market_info.owner == &DELEGATION_PROGRAM_ID,
        ProgramError::IllegalOwner,
        "Market must be owned by Phoenix (ER) or the delegation program (base)",
    )?;

    commit_and_undelegate_accounts(
        payer_info,
        vec![market_info],
        magic_context,
        magic_program,
        None,
    )?;
    Ok(())
}
