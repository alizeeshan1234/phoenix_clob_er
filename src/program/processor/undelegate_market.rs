//! Base-layer finalize for undelegation. After the ER processes
//! `CommitAndUndelegateMarket`, the MagicBlock delegation program calls
//! back into Phoenix on the base layer with the 8-byte
//! `EXTERNAL_UNDELEGATE_DISCRIMINATOR` followed by a borsh-encoded
//! `Vec<Vec<u8>>` of PDA seeds. This handler:
//!   1. Verifies the buffer account is signed (callback authority).
//!   2. Re-creates the market PDA as Phoenix-owned.
//!   3. Copies the buffered state back into it.
//!
//! Account list (per delegation program callback contract):
//!   [0] delegated_account (writable; was owned by delegation program)
//!   [1] buffer            (signer; lamports source for re-creation)
//!   [2] payer             (writable)
//!   [3] system_program

use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    pubkey::Pubkey,
};

use ephemeral_rollups_sdk::cpi::undelegate_account;

pub(crate) fn process_undelegate_market_with_seeds(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    account_seeds: Vec<Vec<u8>>,
) -> ProgramResult {
    let account_iter = &mut accounts.iter();
    let delegated_account = next_account_info(account_iter)?;
    let buffer = next_account_info(account_iter)?;
    let payer = next_account_info(account_iter)?;
    let system_program = next_account_info(account_iter)?;

    undelegate_account(
        delegated_account,
        program_id,
        buffer,
        payer,
        system_program,
        account_seeds,
    )?;
    Ok(())
}
