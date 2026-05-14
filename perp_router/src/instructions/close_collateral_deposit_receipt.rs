//! CloseCollateralDepositReceipt — base layer, auto-fired post-undelegate.
//! Stage 3 of 3. Validator-signed (no user signer in this tx).
//!
//! Mirrors `phoenix-v1/src/program/processor/close_deposit_receipt.rs`.
//!
//! Account list:
//!   [0] trader   (writable; rent destination — NOT a signer)
//!   [1] receipt  (writable; processed DepositReceipt PDA — closed here)

use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    program_error::ProgramError,
    pubkey::Pubkey,
    system_program,
};
use std::mem::size_of;

use crate::{
    error::{assert_with_msg, PerpRouterError},
    state::DepositReceipt,
    validation::loaders::find_deposit_receipt_address,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let it = &mut accounts.iter();
    let trader = next_account_info(it)?;
    let receipt_info = next_account_info(it)?;

    assert_with_msg(
        receipt_info.owner == &crate::ID,
        ProgramError::IllegalOwner,
        "Receipt must be perp_router-owned (back on base after undelegate)",
    )?;

    {
        let buf = receipt_info.try_borrow_data()?;
        let r = bytemuck::from_bytes::<DepositReceipt>(&buf[..size_of::<DepositReceipt>()]);
        assert_with_msg(
            r.processed == 1,
            PerpRouterError::ReceiptNotProcessed,
            "DepositReceipt is not yet processed",
        )?;
        assert_with_msg(
            &r.trader == trader.key,
            ProgramError::InvalidArgument,
            "trader account does not match receipt.trader",
        )?;
        let (expected, _) = find_deposit_receipt_address(&r.trader, program_id);
        assert_with_msg(
            &expected == receipt_info.key,
            PerpRouterError::InvalidPda,
            "DepositReceipt PDA mismatch",
        )?;
    }

    // Drain lamports → trader, zero data, reassign to system program.
    let dest_starting = trader.lamports();
    **trader.lamports.borrow_mut() = dest_starting
        .checked_add(receipt_info.lamports())
        .ok_or(ProgramError::InvalidAccountData)?;
    **receipt_info.lamports.borrow_mut() = 0;
    receipt_info.assign(&system_program::ID);
    #[allow(deprecated)]
    receipt_info.realloc(0, false)?;
    Ok(())
}
