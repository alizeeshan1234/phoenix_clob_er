//! RequestDeposit — step 1 of 3, base layer, **single user signature**.
//!
//! Phoenix's MagicBlock Receipt-PDA deposit flow. The trader signs ONCE.
//! Subsequent steps fire automatically via Magic Actions:
//!
//!   1. SPL transfer wallet → vault (both base and quote in one ix).
//!   2. Create the `DepositReceipt` PDA owned by Phoenix.
//!   3. `delegate_account_with_actions(receipt, [ProcessDepositEr])` — the
//!      delegation program transfers receipt ownership and queues
//!      `ProcessDepositEr` to auto-fire on the ER, signed by the trader
//!      via the SDK's escrow-authority mechanism.
//!
//! Account list (matches `PhoenixInstruction::RequestDeposit`):
//!   [0]  trader               (signer, writable, payer)
//!   [1]  system_program
//!   [2]  market               (writable; expected delegated)
//!   [3]  base_account         (writable; SPL source)
//!   [4]  quote_account        (writable; SPL source)
//!   [5]  base_vault           (writable; SPL destination)
//!   [6]  quote_vault          (writable; SPL destination)
//!   [7]  token_program
//!   [8]  receipt              (writable, empty — created here)
//!   [9]  owner_program        (= Phoenix)
//!   [10] delegation_buffer    (writable)
//!   [11] delegation_record    (writable)
//!   [12] delegation_metadata  (writable)
//!   [13] delegation_program
//!   [14] magic_program        (forwarded into the post-delegation action)
//!   [15] magic_context        (writable; same)

use borsh::{BorshDeserialize, BorshSerialize};
use dlp_api::compact::ClearText;
use ephemeral_rollups_sdk::{
    consts::{DELEGATION_PROGRAM_ID, MAGIC_CONTEXT_ID, MAGIC_PROGRAM_ID},
    cpi::{delegate_account_with_actions, DelegateAccounts, DelegateConfig},
};
use sokoban::ZeroCopy;
use solana_instruction::{AccountMeta as SolAccountMeta, Instruction as SolInstruction};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    program::invoke,
    program_error::ProgramError,
    pubkey::Pubkey,
    rent::Rent,
    sysvar::Sysvar,
};
use std::mem::size_of;

use crate::{
    program::{
        accounts::{DepositReceipt, MarketHeader},
        error::assert_with_msg,
        system_utils::create_account,
        validation::loaders::{get_deposit_receipt_address, get_vault_address},
        PhoenixInstruction,
    },
    quantities::WrapperU64,
};

#[derive(BorshDeserialize, BorshSerialize)]
pub struct RequestDepositParams {
    pub quote_lots: u64,
    pub base_lots: u64,
}

pub(crate) fn process_request_deposit(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let RequestDepositParams { quote_lots, base_lots } =
        RequestDepositParams::try_from_slice(data)?;
    assert_with_msg(
        quote_lots > 0 || base_lots > 0,
        ProgramError::InvalidInstructionData,
        "RequestDeposit must specify a non-zero amount on at least one side",
    )?;

    let account_iter = &mut accounts.iter();
    let trader_info = next_account_info(account_iter)?;
    let system_program_info = next_account_info(account_iter)?;
    let market_info = next_account_info(account_iter)?;
    let base_account = next_account_info(account_iter)?;
    let quote_account = next_account_info(account_iter)?;
    let base_vault = next_account_info(account_iter)?;
    let quote_vault = next_account_info(account_iter)?;
    let token_program_info = next_account_info(account_iter)?;
    let receipt_info = next_account_info(account_iter)?;
    let owner_program_info = next_account_info(account_iter)?;
    let delegation_buffer = next_account_info(account_iter)?;
    let delegation_record = next_account_info(account_iter)?;
    let delegation_metadata = next_account_info(account_iter)?;
    let delegation_program = next_account_info(account_iter)?;
    let magic_program_info = next_account_info(account_iter)?;
    let magic_context_info = next_account_info(account_iter)?;

    assert_with_msg(
        trader_info.is_signer,
        ProgramError::MissingRequiredSignature,
        "trader must sign RequestDeposit",
    )?;
    assert_with_msg(
        owner_program_info.key == &crate::ID,
        ProgramError::IncorrectProgramId,
        "owner_program must be Phoenix",
    )?;
    assert_with_msg(
        delegation_program.key == &DELEGATION_PROGRAM_ID,
        ProgramError::IncorrectProgramId,
        "delegation_program must be DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh",
    )?;
    assert_with_msg(
        magic_program_info.key == &MAGIC_PROGRAM_ID,
        ProgramError::IncorrectProgramId,
        "magic_program must be Magic111...",
    )?;
    assert_with_msg(
        magic_context_info.key == &MAGIC_CONTEXT_ID,
        ProgramError::InvalidArgument,
        "magic_context must be MagicContext1...",
    )?;
    assert_with_msg(
        token_program_info.key == &spl_token::ID,
        ProgramError::IncorrectProgramId,
        "token_program must be SPL Token",
    )?;
    assert_with_msg(
        market_info.owner == &DELEGATION_PROGRAM_ID,
        ProgramError::IllegalOwner,
        "RequestDeposit requires the market to be delegated; use DepositFunds otherwise",
    )?;

    // Pull lot sizes, mint keys, vault keys from header (preserved under delegation).
    let (
        base_lot_size,
        quote_lot_size,
        base_mint,
        quote_mint,
        expected_base_vault,
        expected_quote_vault,
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
        )
    };
    assert_with_msg(
        base_vault.key == &expected_base_vault,
        ProgramError::InvalidArgument,
        "base_vault key mismatch",
    )?;
    assert_with_msg(
        quote_vault.key == &expected_quote_vault,
        ProgramError::InvalidArgument,
        "quote_vault key mismatch",
    )?;
    let (expected_base_pda, _) = get_vault_address(market_info.key, &base_mint);
    let (expected_quote_pda, _) = get_vault_address(market_info.key, &quote_mint);
    assert_with_msg(
        base_vault.key == &expected_base_pda,
        ProgramError::InvalidSeeds,
        "base_vault PDA mismatch",
    )?;
    assert_with_msg(
        quote_vault.key == &expected_quote_pda,
        ProgramError::InvalidSeeds,
        "quote_vault PDA mismatch",
    )?;

    // Receipt PDA.
    let (expected_receipt, receipt_bump) =
        get_deposit_receipt_address(market_info.key, trader_info.key);
    assert_with_msg(
        receipt_info.key == &expected_receipt,
        ProgramError::InvalidSeeds,
        "DepositReceipt PDA mismatch",
    )?;
    assert_with_msg(
        receipt_info.owner == &solana_program::system_program::ID
            && receipt_info.data_is_empty(),
        ProgramError::InvalidAccountData,
        "DepositReceipt must be uninitialized (a prior receipt exists — close it first)",
    )?;

    // 1. SPL transfer trader -> vault.
    let quote_amount = quote_lots
        .checked_mul(quote_lot_size)
        .ok_or(ProgramError::InvalidInstructionData)?;
    let base_amount = base_lots
        .checked_mul(base_lot_size)
        .ok_or(ProgramError::InvalidInstructionData)?;

    if quote_amount > 0 {
        invoke(
            &spl_token::instruction::transfer(
                token_program_info.key,
                quote_account.key,
                quote_vault.key,
                trader_info.key,
                &[],
                quote_amount,
            )?,
            &[
                token_program_info.clone(),
                quote_account.clone(),
                quote_vault.clone(),
                trader_info.clone(),
            ],
        )?;
    }
    if base_amount > 0 {
        invoke(
            &spl_token::instruction::transfer(
                token_program_info.key,
                base_account.key,
                base_vault.key,
                trader_info.key,
                &[],
                base_amount,
            )?,
            &[
                token_program_info.clone(),
                base_account.clone(),
                base_vault.clone(),
                trader_info.clone(),
            ],
        )?;
    }

    // 2. Create the DepositReceipt PDA owned by Phoenix.
    let rent = Rent::get()?;
    let seeds: Vec<Vec<u8>> = vec![
        b"deposit_receipt".to_vec(),
        market_info.key.as_ref().to_vec(),
        trader_info.key.as_ref().to_vec(),
        vec![receipt_bump],
    ];
    create_account(
        trader_info,
        receipt_info,
        system_program_info,
        &crate::ID,
        &rent,
        size_of::<DepositReceipt>() as u64,
        seeds,
    )?;
    {
        let mut data = receipt_info.try_borrow_mut_data()?;
        let receipt = DepositReceipt::load_mut_bytes(&mut data[..size_of::<DepositReceipt>()])
            .ok_or(ProgramError::InvalidAccountData)?;
        *receipt = DepositReceipt::new_init(
            *trader_info.key,
            *market_info.key,
            base_lots,
            quote_lots,
            receipt_bump,
        )?;
    }

    // 3. Build the post-delegation action: ProcessDepositEr fires on the
    //    ER right after delegation lands. Account order must match
    //    process_deposit_er's expected layout.
    let process_er_ix = SolInstruction {
        program_id: crate::ID.to_bytes().into(),
        accounts: vec![
            SolAccountMeta::new(trader_info.key.to_bytes().into(), true),
            SolAccountMeta::new(market_info.key.to_bytes().into(), false),
            SolAccountMeta::new(receipt_info.key.to_bytes().into(), false),
            SolAccountMeta::new_readonly(magic_program_info.key.to_bytes().into(), false),
            SolAccountMeta::new(magic_context_info.key.to_bytes().into(), false),
        ],
        data: vec![PhoenixInstruction::ProcessDepositEr as u8],
    };

    let pda_seeds: &[&[u8]] = &[
        b"deposit_receipt",
        market_info.key.as_ref(),
        trader_info.key.as_ref(),
    ];

    let actions = vec![process_er_ix].cleartext();
    delegate_account_with_actions(
        DelegateAccounts {
            payer: trader_info,
            pda: receipt_info,
            owner_program: owner_program_info,
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
        &[trader_info],
    )?;

    Ok(())
}
