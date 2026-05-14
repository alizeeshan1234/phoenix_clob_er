//! DirectDeposit — base layer, single transaction, no delegation needed.
//!
//! Simpler alternative to the 3-stage Magic Action chain when running
//! locally / on a devnet without MagicBlock allowlisting. Does in one tx:
//!   1. SPL transfer USDC trader → collateral_vault
//!   2. Credit `TraderAccount.collateral`
//!   3. Bump `GlobalState.{v_total_pool_value, c_total_collateral}`
//!
//! Account list:
//!   [0] trader              (signer)
//!   [1] token_program
//!   [2] quote_mint          (readonly)
//!   [3] trader_token_account (writable; SPL source)
//!   [4] collateral_vault    (writable; SPL destination = ATA(authority, mint))
//!   [5] trader_account      (writable)
//!   [6] global_state        (writable)

use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    program::invoke,
    program_error::ProgramError,
    pubkey::Pubkey,
};
use std::mem::size_of;

use crate::{
    error::{assert_with_msg, PerpRouterError},
    state::{vault::find_collateral_vault_address, GlobalState, TraderAccount},
    validation::loaders::{find_global_state_address, find_trader_account_address},
};

#[derive(BorshSerialize, BorshDeserialize)]
pub struct DirectDepositParams {
    pub amount: u64,
}

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let DirectDepositParams { amount } = DirectDepositParams::try_from_slice(data)?;
    assert_with_msg(
        amount > 0,
        ProgramError::InvalidInstructionData,
        "DirectDeposit amount must be > 0",
    )?;

    let it = &mut accounts.iter();
    let trader = next_account_info(it)?;
    let token_program = next_account_info(it)?;
    let quote_mint = next_account_info(it)?;
    let trader_token = next_account_info(it)?;
    let collateral_vault = next_account_info(it)?;
    let trader_account_info = next_account_info(it)?;
    let global_state_info = next_account_info(it)?;

    assert_with_msg(
        trader.is_signer,
        ProgramError::MissingRequiredSignature,
        "trader must sign DirectDeposit",
    )?;
    assert_with_msg(
        token_program.key == &spl_token::ID,
        ProgramError::IncorrectProgramId,
        "token_program must be SPL Token",
    )?;
    let expected_vault = find_collateral_vault_address(quote_mint.key, program_id);
    assert_with_msg(
        collateral_vault.key == &expected_vault,
        PerpRouterError::InvalidPda,
        "collateral_vault ATA mismatch",
    )?;
    assert_with_msg(
        trader_account_info.owner == &crate::ID,
        ProgramError::IllegalOwner,
        "trader_account must be perp_router-owned",
    )?;
    assert_with_msg(
        global_state_info.owner == &crate::ID,
        ProgramError::IllegalOwner,
        "global_state must be perp_router-owned",
    )?;

    // SPL transfer trader → vault.
    invoke(
        &spl_token::instruction::transfer(
            token_program.key,
            trader_token.key,
            collateral_vault.key,
            trader.key,
            &[],
            amount,
        )?,
        &[
            token_program.clone(),
            trader_token.clone(),
            collateral_vault.clone(),
            trader.clone(),
        ],
    )?;

    // Credit TraderAccount.
    {
        let mut buf = trader_account_info.try_borrow_mut_data()?;
        let t = bytemuck::from_bytes_mut::<TraderAccount>(
            &mut buf[..size_of::<TraderAccount>()],
        );
        let (expected, _) = find_trader_account_address(&t.owner, program_id);
        assert_with_msg(
            &expected == trader_account_info.key,
            PerpRouterError::InvalidPda,
            "trader_account PDA mismatch",
        )?;
        assert_with_msg(
            &t.owner == trader.key,
            PerpRouterError::InvalidAuthority,
            "trader_account.owner != signer",
        )?;
        t.collateral = t
            .collateral
            .checked_add(amount)
            .ok_or(PerpRouterError::MathOverflow)?;
    }

    // Bump GlobalState.
    {
        let mut buf = global_state_info.try_borrow_mut_data()?;
        let g = bytemuck::from_bytes_mut::<GlobalState>(
            &mut buf[..size_of::<GlobalState>()],
        );
        let (expected, _) = find_global_state_address(program_id);
        assert_with_msg(
            &expected == global_state_info.key,
            PerpRouterError::InvalidPda,
            "global_state PDA mismatch",
        )?;
        g.v_total_pool_value = g
            .v_total_pool_value
            .checked_add(amount)
            .ok_or(PerpRouterError::MathOverflow)?;
        g.c_total_collateral = g
            .c_total_collateral
            .checked_add(amount)
            .ok_or(PerpRouterError::MathOverflow)?;
    }
    Ok(())
}
