//! ExecuteWithdrawalBaseChain — base layer, **auto-fired post-undelegate
//! action**. Receipt-PDA withdrawal flow, step 3 of 3.
//!
//! NO user signer needed: the MagicBlock validator signs the outer tx as
//! part of the post-undelegate bundle scheduled by ProcessWithdrawalEr.
//! `receipt.trader` is verified against the trader account so vault
//! funds can only go back to the recorded trader.
//!
//! Account list (matches the CallHandler scheduled in ProcessWithdrawalEr):
//!   [0] trader        (writable; rent destination — NOT a signer)
//!   [1] market        (read-only; provides lot sizes + vault bumps)
//!   [2] base_account  (writable; SPL destination)
//!   [3] quote_account (writable; SPL destination)
//!   [4] base_vault    (writable; SPL source — vault PDA signs the transfer)
//!   [5] quote_vault   (writable; SPL source)
//!   [6] token_program
//!   [7] receipt       (writable; processed WithdrawalReceipt PDA — closed)

use sokoban::ZeroCopy;
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    program::invoke_signed,
    program_error::ProgramError,
    pubkey::Pubkey,
};
use std::mem::size_of;

use crate::{
    program::{
        accounts::{MarketHeader, WithdrawalReceipt},
        error::assert_with_msg,
        validation::loaders::get_withdrawal_receipt_address,
    },
    quantities::WrapperU64,
};

pub(crate) fn process_execute_withdrawal_base_chain(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    _data: &[u8],
) -> ProgramResult {
    let account_iter = &mut accounts.iter();
    let trader_info = next_account_info(account_iter)?;
    let market_info = next_account_info(account_iter)?;
    let base_account = next_account_info(account_iter)?;
    let quote_account = next_account_info(account_iter)?;
    let base_vault = next_account_info(account_iter)?;
    let quote_vault = next_account_info(account_iter)?;
    let token_program = next_account_info(account_iter)?;
    let receipt_info = next_account_info(account_iter)?;

    assert_with_msg(
        token_program.key == &spl_token::ID,
        ProgramError::IncorrectProgramId,
        "token_program must be SPL Token",
    )?;
    assert_with_msg(
        receipt_info.owner == &crate::ID,
        ProgramError::IllegalOwner,
        "Receipt must be Phoenix-owned (back on base layer after undelegate)",
    )?;

    let (base_lots, quote_lots) = {
        let data = receipt_info.try_borrow_data()?;
        let receipt = WithdrawalReceipt::load_bytes(&data[..size_of::<WithdrawalReceipt>()])
            .ok_or(ProgramError::InvalidAccountData)?;
        assert_with_msg(
            receipt.processed == 1,
            ProgramError::InvalidAccountData,
            "WithdrawalReceipt not yet processed; ProcessWithdrawalEr first",
        )?;
        assert_with_msg(
            &receipt.trader == trader_info.key,
            ProgramError::InvalidArgument,
            "trader account does not match receipt.trader",
        )?;
        assert_with_msg(
            &receipt.market == market_info.key,
            ProgramError::InvalidArgument,
            "market account does not match receipt.market",
        )?;
        let (expected_pda, _) =
            get_withdrawal_receipt_address(&receipt.market, &receipt.trader);
        assert_with_msg(
            &expected_pda == receipt_info.key,
            ProgramError::InvalidSeeds,
            "Receipt PDA mismatch",
        )?;
        (receipt.base_lots, receipt.quote_lots)
    };

    let (
        base_lot_size,
        quote_lot_size,
        base_mint,
        quote_mint,
        base_vault_key,
        quote_vault_key,
        base_vault_bump,
        quote_vault_bump,
    ) = {
        let data = market_info.try_borrow_data()?;
        let header = MarketHeader::load_bytes(&data[..size_of::<MarketHeader>()])
            .ok_or(ProgramError::InvalidAccountData)?;
        (
            header.get_base_lot_size().as_u64(),
            header.get_quote_lot_size().as_u64(),
            header.base_params.mint_key,
            header.quote_params.mint_key,
            header.base_params.vault_key,
            header.quote_params.vault_key,
            header.base_params.vault_bump as u8,
            header.quote_params.vault_bump as u8,
        )
    };
    assert_with_msg(
        base_vault.key == &base_vault_key,
        ProgramError::InvalidArgument,
        "base_vault key mismatch",
    )?;
    assert_with_msg(
        quote_vault.key == &quote_vault_key,
        ProgramError::InvalidArgument,
        "quote_vault key mismatch",
    )?;

    let base_atoms = base_lots
        .checked_mul(base_lot_size)
        .ok_or(ProgramError::InvalidInstructionData)?;
    let quote_atoms = quote_lots
        .checked_mul(quote_lot_size)
        .ok_or(ProgramError::InvalidInstructionData)?;

    // SPL transfers vault → user, signed by the vault PDA.
    if base_atoms > 0 {
        invoke_signed(
            &spl_token::instruction::transfer(
                token_program.key,
                base_vault.key,
                base_account.key,
                base_vault.key,
                &[],
                base_atoms,
            )?,
            &[
                token_program.clone(),
                base_vault.clone(),
                base_account.clone(),
            ],
            &[&[
                b"vault",
                market_info.key.as_ref(),
                base_mint.as_ref(),
                &[base_vault_bump],
            ]],
        )?;
    }
    if quote_atoms > 0 {
        invoke_signed(
            &spl_token::instruction::transfer(
                token_program.key,
                quote_vault.key,
                quote_account.key,
                quote_vault.key,
                &[],
                quote_atoms,
            )?,
            &[
                token_program.clone(),
                quote_vault.clone(),
                quote_account.clone(),
            ],
            &[&[
                b"vault",
                market_info.key.as_ref(),
                quote_mint.as_ref(),
                &[quote_vault_bump],
            ]],
        )?;
    }

    // Close the receipt.
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
