//! RevokeSessionToken — close the SessionToken PDA, refund rent to owner.
//! Base layer, owner-signed.
//!
//! Account list:
//!   [0] owner          (signer, writable; lamport destination)
//!   [1] session_token  (writable; closed)

use sokoban::ZeroCopy;
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    program_error::ProgramError,
    pubkey::Pubkey,
};
use std::mem::size_of;

use crate::program::{accounts::SessionToken, error::assert_with_msg};

pub(crate) fn process_revoke_session_token(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    _data: &[u8],
) -> ProgramResult {
    let account_iter = &mut accounts.iter();
    let owner_info = next_account_info(account_iter)?;
    let session_token_info = next_account_info(account_iter)?;

    assert_with_msg(
        owner_info.is_signer && owner_info.is_writable,
        ProgramError::MissingRequiredSignature,
        "owner must sign and be writable",
    )?;
    assert_with_msg(
        session_token_info.owner == &crate::ID,
        ProgramError::IllegalOwner,
        "session_token must be owned by Phoenix",
    )?;

    {
        let data = session_token_info.try_borrow_data()?;
        let token = SessionToken::load_bytes(&data[..size_of::<SessionToken>()])
            .ok_or(ProgramError::InvalidAccountData)?;
        assert_with_msg(
            &token.owner == owner_info.key,
            ProgramError::InvalidArgument,
            "Only the recorded owner can revoke",
        )?;
    }

    // Close: drain lamports → owner, zero data, assign to system program.
    let dest_starting = owner_info.lamports();
    **owner_info.lamports.borrow_mut() = dest_starting
        .checked_add(session_token_info.lamports())
        .ok_or(ProgramError::InvalidAccountData)?;
    **session_token_info.lamports.borrow_mut() = 0;
    session_token_info.assign(&solana_program::system_program::ID);
    #[allow(deprecated)]
    session_token_info.realloc(0, false)?;
    Ok(())
}
