//! RequestCollateralDeposit — base layer, **single user signature**.
//!
//! Stage 1 of 3. Mirrors `phoenix-v1/src/program/processor/request_deposit.rs`.
//!
//! Flow:
//!   1. SPL transfer USDC: trader_token_account → collateral_vault
//!   2. Create DepositReceipt PDA owned by perp_router
//!   3. `delegate_account_with_actions(receipt, [ProcessCollateralDepositEr])`
//!      — the receipt is delegated to the ER and the follow-up ix is queued
//!      to auto-fire under escrow authority.
//!
//! Account list:
//!   [0]  trader              (signer, writable, payer)
//!   [1]  system_program
//!   [2]  token_program
//!   [3]  quote_mint          (readonly)
//!   [4]  perp_market         (readonly; context check)
//!   [5]  trader_token_account (writable; SPL source)
//!   [6]  collateral_vault    (writable; SPL destination, PDA)
//!   [7]  receipt             (writable, empty — created here)
//!   [8]  owner_program       (= perp_router)
//!   [9]  delegation_buffer   (writable)
//!   [10] delegation_record   (writable)
//!   [11] delegation_metadata (writable)
//!   [12] delegation_program
//!   [13] magic_program       (forwarded into post-delegation action)
//!   [14] magic_context       (writable; same)
//!   [15] trader_account      (writable, delegated; forwarded → stage 2)
//!   [16] global_state        (writable, delegated; forwarded → stage 2)

use borsh::{BorshDeserialize, BorshSerialize};
use bytemuck;
use dlp_api::compact::ClearText;
use ephemeral_rollups_sdk::{
    consts::{DELEGATION_PROGRAM_ID, MAGIC_CONTEXT_ID, MAGIC_PROGRAM_ID},
    cpi::{delegate_account_with_actions, DelegateAccounts, DelegateConfig},
};
use solana_instruction::{AccountMeta as SolAccountMeta, Instruction as SolInstruction};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    program::invoke,
    program_error::ProgramError,
    pubkey::Pubkey,
    rent::Rent,
    system_program,
    sysvar::Sysvar,
};
use std::mem::size_of;

use crate::{
    constants::DEPOSIT_RECEIPT_SEED,
    error::{assert_with_msg, PerpRouterError},
    state::{vault::find_collateral_vault_address, DepositReceipt, PerpMarket},
    system_utils::create_account,
    validation::loaders::{find_deposit_receipt_address, find_perp_market_address},
    PerpRouterInstruction,
};

#[derive(BorshSerialize, BorshDeserialize)]
pub struct RequestCollateralDepositParams {
    pub amount: u64,
}

pub fn process(_program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let RequestCollateralDepositParams { amount } =
        RequestCollateralDepositParams::try_from_slice(data)?;
    assert_with_msg(
        amount > 0,
        ProgramError::InvalidInstructionData,
        "RequestCollateralDeposit amount must be > 0",
    )?;

    let it = &mut accounts.iter();
    let trader = next_account_info(it)?;
    let system_program_info = next_account_info(it)?;
    let token_program = next_account_info(it)?;
    let quote_mint = next_account_info(it)?;
    let perp_market_info = next_account_info(it)?;
    let trader_token = next_account_info(it)?;
    let collateral_vault = next_account_info(it)?;
    let receipt_info = next_account_info(it)?;
    let owner_program = next_account_info(it)?;
    let delegation_buffer = next_account_info(it)?;
    let delegation_record = next_account_info(it)?;
    let delegation_metadata = next_account_info(it)?;
    let delegation_program = next_account_info(it)?;
    let magic_program = next_account_info(it)?;
    let magic_context = next_account_info(it)?;
    // Delegated targets — read on base, forwarded to stage 2 on ER:
    let trader_account_info = next_account_info(it)?;
    let global_state_info = next_account_info(it)?;

    // --- signer + program identity checks ---
    assert_with_msg(
        trader.is_signer,
        ProgramError::MissingRequiredSignature,
        "trader must sign RequestCollateralDeposit",
    )?;
    assert_with_msg(
        system_program_info.key == &system_program::ID,
        ProgramError::IncorrectProgramId,
        "system_program mismatch",
    )?;
    assert_with_msg(
        token_program.key == &spl_token::ID,
        ProgramError::IncorrectProgramId,
        "token_program must be SPL Token",
    )?;
    assert_with_msg(
        owner_program.key == &crate::ID,
        ProgramError::IncorrectProgramId,
        "owner_program must be perp_router",
    )?;
    assert_with_msg(
        delegation_program.key == &DELEGATION_PROGRAM_ID,
        ProgramError::IncorrectProgramId,
        "delegation_program account mismatch",
    )?;
    assert_with_msg(
        magic_program.key == &MAGIC_PROGRAM_ID,
        ProgramError::IncorrectProgramId,
        "magic_program account mismatch",
    )?;
    assert_with_msg(
        magic_context.key == &MAGIC_CONTEXT_ID,
        ProgramError::InvalidArgument,
        "magic_context account mismatch",
    )?;

    // --- PDA shape checks ---
    let (expected_market_pda, _) =
        find_perp_market_address(perp_market_info.key, _program_id);
    let _ = expected_market_pda; // PerpMarket key itself isn't recomputed here;
    // we trust it via header below.
    {
        let buf = perp_market_info.try_borrow_data()?;
        let m = bytemuck::from_bytes::<PerpMarket>(&buf[..size_of::<PerpMarket>()]);
        let (expected, _) = find_perp_market_address(&m.phoenix_market, _program_id);
        assert_with_msg(
            &expected == perp_market_info.key,
            PerpRouterError::InvalidPda,
            "perp_market PDA mismatch",
        )?;
    }

    let expected_vault = find_collateral_vault_address(quote_mint.key, _program_id);
    assert_with_msg(
        collateral_vault.key == &expected_vault,
        PerpRouterError::InvalidPda,
        "collateral_vault ATA mismatch",
    )?;

    let (expected_receipt, receipt_bump) =
        find_deposit_receipt_address(trader.key, _program_id);
    assert_with_msg(
        receipt_info.key == &expected_receipt,
        PerpRouterError::InvalidPda,
        "deposit_receipt PDA mismatch",
    )?;
    assert_with_msg(
        receipt_info.owner == &system_program::ID && receipt_info.data_is_empty(),
        PerpRouterError::AlreadyInitialized,
        "DepositReceipt must be uninitialised (close any prior receipt first)",
    )?;

    // --- 1. SPL transfer trader → vault ---
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

    // --- 2. Create the DepositReceipt PDA owned by perp_router ---
    let rent = Rent::get()?;
    let seeds: Vec<Vec<u8>> = vec![
        DEPOSIT_RECEIPT_SEED.to_vec(),
        trader.key.as_ref().to_vec(),
        vec![receipt_bump],
    ];
    create_account(
        trader,
        receipt_info,
        system_program_info,
        &crate::ID,
        &rent,
        size_of::<DepositReceipt>() as u64,
        seeds,
    )?;
    {
        let mut buf = receipt_info.try_borrow_mut_data()?;
        let r = bytemuck::from_bytes_mut::<DepositReceipt>(&mut buf[..size_of::<DepositReceipt>()]);
        *r = DepositReceipt {
            trader: *trader.key,
            market: *perp_market_info.key,
            amount,
            processed: 0,
            bump: receipt_bump,
            _pad: [0; 6],
        };
    }

    // --- 3. Delegate receipt + queue ProcessCollateralDepositEr ---
    // Stage 2's account order must match process_collateral_deposit_er.rs:
    //   [0] trader, [1] receipt, [2] trader_account, [3] global_state,
    //   [4] magic_program, [5] magic_context
    let process_er_ix = SolInstruction {
        program_id: crate::ID.to_bytes().into(),
        accounts: vec![
            SolAccountMeta::new(trader.key.to_bytes().into(), true),
            SolAccountMeta::new(receipt_info.key.to_bytes().into(), false),
            SolAccountMeta::new(trader_account_info.key.to_bytes().into(), false),
            SolAccountMeta::new(global_state_info.key.to_bytes().into(), false),
            SolAccountMeta::new_readonly(magic_program.key.to_bytes().into(), false),
            SolAccountMeta::new(magic_context.key.to_bytes().into(), false),
        ],
        data: vec![PerpRouterInstruction::ProcessCollateralDepositEr as u8],
    };
    let pda_seeds: &[&[u8]] = &[DEPOSIT_RECEIPT_SEED, trader.key.as_ref()];
    let actions = vec![process_er_ix].cleartext();
    delegate_account_with_actions(
        DelegateAccounts {
            payer: trader,
            pda: receipt_info,
            owner_program,
            buffer: delegation_buffer,
            delegation_record,
            delegation_metadata,
            delegation_program,
            system_program: system_program_info,
        },
        pda_seeds,
        DelegateConfig {
            commit_frequency_ms: u32::MAX,
            validator: None,
        },
        actions,
        &[trader],
    )?;

    Ok(())
}
