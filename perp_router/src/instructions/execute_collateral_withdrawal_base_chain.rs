//! ExecuteCollateralWithdrawalBaseChain — base layer, auto-fired
//! post-undelegate. Stage 3 of 3. Validator-signed (no user signer).
//!
//! Pays out `receipt.net_amount` from the vault PDA to the trader's token
//! account. Closes the receipt and refunds rent.
//!
//! Mirrors `phoenix-v1/src/program/processor/execute_withdrawal_base_chain.rs`.
//!
//! Account list:
//!   [0] trader              (writable; rent destination — NOT a signer)
//!   [1] receipt             (writable; processed WithdrawalReceipt — closed here)
//!   [2] trader_token        (writable; SPL destination)
//!   [3] collateral_vault    (writable; SPL source — vault PDA signs)
//!   [4] quote_mint          (readonly; for vault PDA bump derivation)
//!   [5] token_program

use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    program::invoke_signed,
    program_error::ProgramError,
    pubkey::Pubkey,
    system_program,
};
use std::mem::size_of;

use crate::{
    constants::PERP_AUTHORITY_SEED,
    error::{assert_with_msg, PerpRouterError},
    state::{vault::find_collateral_vault_address, WithdrawalReceipt},
    validation::loaders::{find_perp_authority_address, find_withdrawal_receipt_address},
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let it = &mut accounts.iter();
    let trader = next_account_info(it)?;
    let receipt_info = next_account_info(it)?;
    let trader_token = next_account_info(it)?;
    let collateral_vault = next_account_info(it)?;
    let quote_mint = next_account_info(it)?;
    let token_program = next_account_info(it)?;
    let perp_authority = next_account_info(it)?;

    assert_with_msg(
        token_program.key == &spl_token::ID,
        ProgramError::IncorrectProgramId,
        "token_program must be SPL Token",
    )?;
    assert_with_msg(
        receipt_info.owner == &crate::ID,
        ProgramError::IllegalOwner,
        "Receipt must be perp_router-owned (back on base after undelegate)",
    )?;

    let (net_amount,) = {
        let buf = receipt_info.try_borrow_data()?;
        let r = bytemuck::from_bytes::<WithdrawalReceipt>(
            &buf[..size_of::<WithdrawalReceipt>()],
        );
        assert_with_msg(
            r.processed == 1,
            PerpRouterError::ReceiptNotProcessed,
            "WithdrawalReceipt not yet processed",
        )?;
        assert_with_msg(
            &r.trader == trader.key,
            ProgramError::InvalidArgument,
            "trader account does not match receipt.trader",
        )?;
        let (expected, _) = find_withdrawal_receipt_address(&r.trader, program_id);
        assert_with_msg(
            &expected == receipt_info.key,
            PerpRouterError::InvalidPda,
            "WithdrawalReceipt PDA mismatch",
        )?;
        (r.net_amount,)
    };

    // Derive expected vault (ATA of perp_authority + quote_mint) and
    // perp_authority's bump (used for signing the SPL transfer).
    let expected_vault = find_collateral_vault_address(quote_mint.key, program_id);
    assert_with_msg(
        collateral_vault.key == &expected_vault,
        PerpRouterError::InvalidPda,
        "collateral_vault ATA mismatch",
    )?;
    let (expected_authority, authority_bump) = find_perp_authority_address(program_id);
    assert_with_msg(
        perp_authority.key == &expected_authority,
        PerpRouterError::InvalidPda,
        "perp_authority PDA mismatch",
    )?;

    // perp_authority signs the SPL transfer.
    if net_amount > 0 {
        invoke_signed(
            &spl_token::instruction::transfer(
                token_program.key,
                collateral_vault.key,
                trader_token.key,
                perp_authority.key,
                &[],
                net_amount,
            )?,
            &[
                token_program.clone(),
                collateral_vault.clone(),
                trader_token.clone(),
                perp_authority.clone(),
            ],
            &[&[PERP_AUTHORITY_SEED, &[authority_bump]]],
        )?;
    }

    // Close receipt.
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
