//! CreateSessionToken — base layer, owner-signed.
//!
//! The owner registers an ephemeral keypair (`session_signer`) and an
//! expiry timestamp. The on-chain PDA records this authorization;
//! subsequent `*ViaSession` ixs check it before processing the trade.
//!
//! Account list:
//!   [0] owner          (signer, writable, payer)
//!   [1] session_token  (writable, empty — will be created)
//!   [2] system_program

use borsh::{BorshDeserialize, BorshSerialize};
use sokoban::ZeroCopy;
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    clock::Clock,
    entrypoint::ProgramResult,
    program_error::ProgramError,
    pubkey::Pubkey,
    rent::Rent,
    sysvar::Sysvar,
};
use std::mem::size_of;

use crate::program::{
    accounts::SessionToken,
    error::assert_with_msg,
    system_utils::create_account,
    validation::loaders::get_session_token_address,
};

#[derive(BorshDeserialize, BorshSerialize)]
pub struct CreateSessionTokenParams {
    /// Ephemeral keypair pubkey to authorize.
    pub session_signer: Pubkey,
    /// Unix timestamp after which the token is invalid. 0 = never.
    pub expires_at: i64,
}

pub(crate) fn process_create_session_token(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let CreateSessionTokenParams { session_signer, expires_at } =
        CreateSessionTokenParams::try_from_slice(data)?;

    let account_iter = &mut accounts.iter();
    let owner_info = next_account_info(account_iter)?;
    let session_token_info = next_account_info(account_iter)?;
    let system_program_info = next_account_info(account_iter)?;

    assert_with_msg(
        owner_info.is_signer && owner_info.is_writable,
        ProgramError::MissingRequiredSignature,
        "owner must sign and be writable (pays rent)",
    )?;
    assert_with_msg(
        session_token_info.owner == &solana_program::system_program::ID
            && session_token_info.data_is_empty(),
        ProgramError::InvalidAccountData,
        "session_token must be uninitialized (revoke any existing token first)",
    )?;

    let (expected_pda, bump) = get_session_token_address(owner_info.key, &session_signer);
    assert_with_msg(
        session_token_info.key == &expected_pda,
        ProgramError::InvalidSeeds,
        "session_token PDA mismatch",
    )?;

    let rent = Rent::get()?;
    let seeds: Vec<Vec<u8>> = vec![
        b"session".to_vec(),
        owner_info.key.as_ref().to_vec(),
        session_signer.as_ref().to_vec(),
        vec![bump],
    ];
    create_account(
        owner_info,
        session_token_info,
        system_program_info,
        &crate::ID,
        &rent,
        size_of::<SessionToken>() as u64,
        seeds,
    )?;

    let now_slot = Clock::get()?.slot;
    {
        let mut data = session_token_info.try_borrow_mut_data()?;
        let token = SessionToken::load_mut_bytes(&mut data[..size_of::<SessionToken>()])
            .ok_or(ProgramError::InvalidAccountData)?;
        *token = SessionToken::new_init(
            *owner_info.key,
            session_signer,
            expires_at,
            now_slot,
            bump,
        )?;
    }

    Ok(())
}
