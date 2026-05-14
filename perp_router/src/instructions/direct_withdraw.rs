//! DirectWithdraw — base layer, single tx, no delegation needed.
//!
//! Single-tx variant of the 3-stage Magic Action withdrawal chain.
//! Applies Percolator's H haircut on matured PnL; collateral is senior.
//!
//! Account list:
//!   [0] trader              (signer)
//!   [1] token_program
//!   [2] quote_mint          (readonly; for vault signing)
//!   [3] trader_token_account (writable; SPL destination)
//!   [4] collateral_vault    (writable; SPL source)
//!   [5] trader_account      (writable)
//!   [6] global_state        (writable)
//!   [7] perp_authority      (readonly; PDA — signs SPL out)

use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    program::invoke_signed,
    program_error::ProgramError,
    pubkey::Pubkey,
};
use std::mem::size_of;

use crate::{
    constants::PERP_AUTHORITY_SEED,
    error::{assert_with_msg, PerpRouterError},
    risk::haircut::{compute_h, split_withdrawal},
    state::{vault::find_collateral_vault_address, GlobalState, TraderAccount},
    validation::loaders::{
        find_global_state_address, find_perp_authority_address, find_trader_account_address,
    },
};

#[derive(BorshSerialize, BorshDeserialize)]
pub struct DirectWithdrawParams {
    /// Gross amount requested. The instruction applies the haircut.
    pub amount: u64,
}

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let DirectWithdrawParams { amount } = DirectWithdrawParams::try_from_slice(data)?;
    assert_with_msg(
        amount > 0,
        ProgramError::InvalidInstructionData,
        "DirectWithdraw amount must be > 0",
    )?;

    let it = &mut accounts.iter();
    let trader = next_account_info(it)?;
    let token_program = next_account_info(it)?;
    let quote_mint = next_account_info(it)?;
    let trader_token = next_account_info(it)?;
    let collateral_vault = next_account_info(it)?;
    let trader_account_info = next_account_info(it)?;
    let global_state_info = next_account_info(it)?;
    let perp_authority = next_account_info(it)?;

    assert_with_msg(
        trader.is_signer,
        ProgramError::MissingRequiredSignature,
        "trader must sign DirectWithdraw",
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
    let (expected_auth, authority_bump) = find_perp_authority_address(program_id);
    assert_with_msg(
        perp_authority.key == &expected_auth,
        PerpRouterError::InvalidPda,
        "perp_authority PDA mismatch",
    )?;

    // Snapshot pool state.
    let g_snap = {
        let buf = global_state_info.try_borrow_data()?;
        let g = bytemuck::from_bytes::<GlobalState>(&buf[..size_of::<GlobalState>()]);
        let (expected, _) = find_global_state_address(program_id);
        assert_with_msg(
            &expected == global_state_info.key,
            PerpRouterError::InvalidPda,
            "global_state PDA mismatch",
        )?;
        *g
    };

    // Apply haircut split.
    let (h_num, h_den) = compute_h(
        g_snap.v_total_pool_value,
        g_snap.c_total_collateral,
        g_snap.i_insurance_reserve,
        g_snap.total_matured_pnl,
    );

    let (collateral_request, pnl_request, net_amount) = {
        let buf = trader_account_info.try_borrow_data()?;
        let t = bytemuck::from_bytes::<TraderAccount>(&buf[..size_of::<TraderAccount>()]);
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
        let coll_req = amount.min(t.collateral);
        let pnl_req = amount.saturating_sub(coll_req).min(t.pnl_matured);
        let (_, _, total) = split_withdrawal(coll_req, pnl_req, h_num, h_den)?;
        (coll_req, pnl_req, total)
    };

    // Mutate TraderAccount + GlobalState.
    {
        let mut buf = trader_account_info.try_borrow_mut_data()?;
        let t = bytemuck::from_bytes_mut::<TraderAccount>(
            &mut buf[..size_of::<TraderAccount>()],
        );
        t.collateral = t
            .collateral
            .checked_sub(collateral_request)
            .ok_or(PerpRouterError::InsufficientCollateral)?;
        t.pnl_matured = t
            .pnl_matured
            .checked_sub(pnl_request)
            .ok_or(PerpRouterError::InsufficientCollateral)?;
    }
    {
        let mut buf = global_state_info.try_borrow_mut_data()?;
        let g = bytemuck::from_bytes_mut::<GlobalState>(
            &mut buf[..size_of::<GlobalState>()],
        );
        g.v_total_pool_value = g.v_total_pool_value.saturating_sub(net_amount);
        g.c_total_collateral = g.c_total_collateral.saturating_sub(collateral_request);
        g.total_matured_pnl = g.total_matured_pnl.saturating_sub(pnl_request);
    }

    // Vault PDA signs the payout.
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
    Ok(())
}
