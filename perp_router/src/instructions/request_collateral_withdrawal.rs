//! RequestCollateralWithdrawal — base layer, **single user signature**.
//!
//! Stage 1 of 3. Mirrors `phoenix-v1/src/program/processor/request_withdrawal.rs`.
//!
//! No SPL transfer here. Stage 2 (ER) does the accounting + haircut. Stage
//! 3 (base, post-undelegate) does the payout. All accounts needed by stage 3
//! are forwarded through the post-delegation + post-undelegate actions.
//!
//! Account list:
//!   [0]  trader              (signer, writable, payer)
//!   [1]  system_program
//!   [2]  receipt             (writable, empty — created here)
//!   [3]  quote_mint          (readonly)
//!   [4]  perp_market         (readonly; context check)
//!   [5]  trader_token_account (writable; forwarded → stage 3)
//!   [6]  collateral_vault    (writable; forwarded → stage 3)
//!   [7]  token_program       (forwarded → stage 3)
//!   [8]  owner_program       (= perp_router)
//!   [9]  delegation_buffer   (writable)
//!   [10] delegation_record   (writable)
//!   [11] delegation_metadata (writable)
//!   [12] delegation_program
//!   [13] magic_program       (forwarded → stages 2+3)
//!   [14] magic_context       (writable; forwarded → stage 2)
//!   [15] trader_account      (writable, delegated; forwarded → stage 2)
//!   [16] global_state        (writable, delegated; forwarded → stage 2)

use borsh::{BorshDeserialize, BorshSerialize};
use dlp_api::compact::ClearText;
use ephemeral_rollups_sdk::{
    consts::{DELEGATION_PROGRAM_ID, MAGIC_CONTEXT_ID, MAGIC_PROGRAM_ID},
    cpi::{delegate_account_with_actions, DelegateAccounts, DelegateConfig},
};
use solana_instruction::{AccountMeta as SolAccountMeta, Instruction as SolInstruction};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    program_error::ProgramError,
    pubkey::Pubkey,
    rent::Rent,
    system_program,
    sysvar::Sysvar,
};
use std::mem::size_of;

use crate::{
    constants::WITHDRAWAL_RECEIPT_SEED,
    error::{assert_with_msg, PerpRouterError},
    state::{vault::find_collateral_vault_address, WithdrawalReceipt},
    system_utils::create_account,
    validation::loaders::find_withdrawal_receipt_address,
    PerpRouterInstruction,
};

#[derive(BorshSerialize, BorshDeserialize)]
pub struct RequestCollateralWithdrawalParams {
    /// Gross amount requested. Stage 2 (ER) applies the haircut.
    pub amount: u64,
}

pub fn process(_program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let RequestCollateralWithdrawalParams { amount } =
        RequestCollateralWithdrawalParams::try_from_slice(data)?;
    assert_with_msg(
        amount > 0,
        ProgramError::InvalidInstructionData,
        "RequestCollateralWithdrawal amount must be > 0",
    )?;

    let it = &mut accounts.iter();
    let trader = next_account_info(it)?;
    let system_program_info = next_account_info(it)?;
    let receipt_info = next_account_info(it)?;
    let quote_mint = next_account_info(it)?;
    let perp_market_info = next_account_info(it)?;
    let trader_token = next_account_info(it)?;
    let collateral_vault = next_account_info(it)?;
    let token_program = next_account_info(it)?;
    let owner_program = next_account_info(it)?;
    let delegation_buffer = next_account_info(it)?;
    let delegation_record = next_account_info(it)?;
    let delegation_metadata = next_account_info(it)?;
    let delegation_program = next_account_info(it)?;
    let magic_program = next_account_info(it)?;
    let magic_context = next_account_info(it)?;
    let trader_account_info = next_account_info(it)?;
    let global_state_info = next_account_info(it)?;

    assert_with_msg(
        trader.is_signer,
        ProgramError::MissingRequiredSignature,
        "trader must sign RequestCollateralWithdrawal",
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
        "delegation_program mismatch",
    )?;
    assert_with_msg(
        magic_program.key == &MAGIC_PROGRAM_ID,
        ProgramError::IncorrectProgramId,
        "magic_program mismatch",
    )?;
    assert_with_msg(
        magic_context.key == &MAGIC_CONTEXT_ID,
        ProgramError::InvalidArgument,
        "magic_context mismatch",
    )?;

    let expected_vault = find_collateral_vault_address(quote_mint.key, _program_id);
    assert_with_msg(
        collateral_vault.key == &expected_vault,
        PerpRouterError::InvalidPda,
        "collateral_vault ATA mismatch",
    )?;

    let (expected_receipt, receipt_bump) =
        find_withdrawal_receipt_address(trader.key, _program_id);
    assert_with_msg(
        receipt_info.key == &expected_receipt,
        PerpRouterError::InvalidPda,
        "withdrawal_receipt PDA mismatch",
    )?;
    assert_with_msg(
        receipt_info.owner == &system_program::ID && receipt_info.data_is_empty(),
        PerpRouterError::AlreadyInitialized,
        "WithdrawalReceipt must be uninitialised (close prior receipt first)",
    )?;

    // --- Create receipt ---
    let rent = Rent::get()?;
    let seeds: Vec<Vec<u8>> = vec![
        WITHDRAWAL_RECEIPT_SEED.to_vec(),
        trader.key.as_ref().to_vec(),
        vec![receipt_bump],
    ];
    create_account(
        trader,
        receipt_info,
        system_program_info,
        &crate::ID,
        &rent,
        size_of::<WithdrawalReceipt>() as u64,
        seeds,
    )?;
    {
        let mut buf = receipt_info.try_borrow_mut_data()?;
        let r = bytemuck::from_bytes_mut::<WithdrawalReceipt>(
            &mut buf[..size_of::<WithdrawalReceipt>()],
        );
        *r = WithdrawalReceipt {
            trader: *trader.key,
            market: *perp_market_info.key,
            gross_amount: amount,
            net_amount: 0,
            h_numerator: 0,
            h_denominator: 0,
            processed: 0,
            bump: receipt_bump,
            _pad: [0; 6],
        };
    }

    // --- Delegate receipt + queue ProcessCollateralWithdrawalEr ---
    // Stage 2 receives: trader, receipt, trader_account, global_state,
    // magic_program, magic_context, trader_token, collateral_vault,
    // token_program, quote_mint. trader_account and global_state are
    // delegated separately and resolved by the ER at execution time
    // (they're not passed here — they're delegated singletons).
    let process_er_ix = SolInstruction {
        program_id: crate::ID.to_bytes().into(),
        accounts: vec![
            SolAccountMeta::new(trader.key.to_bytes().into(), true),
            SolAccountMeta::new(receipt_info.key.to_bytes().into(), false),
            SolAccountMeta::new_readonly(magic_program.key.to_bytes().into(), false),
            SolAccountMeta::new(magic_context.key.to_bytes().into(), false),
            SolAccountMeta::new(trader_token.key.to_bytes().into(), false),
            SolAccountMeta::new(collateral_vault.key.to_bytes().into(), false),
            SolAccountMeta::new_readonly(quote_mint.key.to_bytes().into(), false),
            SolAccountMeta::new_readonly(token_program.key.to_bytes().into(), false),
            SolAccountMeta::new(trader_account_info.key.to_bytes().into(), false),
            SolAccountMeta::new(global_state_info.key.to_bytes().into(), false),
        ],
        data: vec![PerpRouterInstruction::ProcessCollateralWithdrawalEr as u8],
    };
    let pda_seeds: &[&[u8]] = &[WITHDRAWAL_RECEIPT_SEED, trader.key.as_ref()];
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
