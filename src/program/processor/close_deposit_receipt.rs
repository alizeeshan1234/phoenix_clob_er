//! CloseDepositReceipt — base layer, **auto-fired post-undelegate action**.
//!
//! Phoenix Receipt-PDA deposit flow, step 3 of 3. NO user signer needed:
//! the MagicBlock validator signs the outer tx as part of the
//! post-undelegate action bundle scheduled by ProcessDepositEr. The
//! `receipt.trader` field is verified against the trader account so
//! lamports can only go back to the recorded trader.
//!
//! Account list:
//!   [0] trader   (writable; rent destination — NOT a signer)
//!   [1] receipt  (writable; processed DepositReceipt PDA — will be closed)

use sokoban::ZeroCopy;
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    program_error::ProgramError,
    pubkey::Pubkey,
};
use std::mem::size_of;

use crate::program::{
    accounts::DepositReceipt, error::assert_with_msg,
    validation::loaders::get_deposit_receipt_address,
};

pub(crate) fn process_close_deposit_receipt(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    _data: &[u8],
) -> ProgramResult {
    let account_iter = &mut accounts.iter();
    let trader_info = next_account_info(account_iter)?;
    let receipt_info = next_account_info(account_iter)?;

    assert_with_msg(
        receipt_info.owner == &crate::ID,
        ProgramError::IllegalOwner,
        "Receipt must be Phoenix-owned (back on base layer after undelegate)",
    )?;

    // Validate receipt: must be processed, trader pubkey must match, must
    // be at the canonical PDA.
    {
        let data = receipt_info.try_borrow_data()?;
        let receipt = DepositReceipt::load_bytes(&data[..size_of::<DepositReceipt>()])
            .ok_or(ProgramError::InvalidAccountData)?;
        assert_with_msg(
            receipt.processed == 1,
            ProgramError::InvalidAccountData,
            "DepositReceipt is not yet processed; ProcessDepositEr first",
        )?;
        assert_with_msg(
            &receipt.trader == trader_info.key,
            ProgramError::InvalidArgument,
            "trader account does not match receipt.trader",
        )?;
        let (expected_pda, _) =
            get_deposit_receipt_address(&receipt.market, &receipt.trader);
        assert_with_msg(
            &expected_pda == receipt_info.key,
            ProgramError::InvalidSeeds,
            "Receipt PDA mismatch",
        )?;
    }

    // Close: drain lamports to trader, zero data, assign to system program.
    let dest_starting = trader_info.lamports();
    **trader_info.lamports.borrow_mut() = dest_starting
        .checked_add(receipt_info.lamports())
        .ok_or(ProgramError::InvalidAccountData)?;
    **receipt_info.lamports.borrow_mut() = 0;
    receipt_info.assign(&solana_program::system_program::ID);
    #[allow(deprecated)]
    receipt_info.realloc(0, false)?;
    Ok(())
}
