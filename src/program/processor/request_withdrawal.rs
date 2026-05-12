//! RequestWithdrawal — step 1 of 3, base layer, **single user signature**.
//!
//! Records the requested withdrawal amounts in a `WithdrawalReceipt` PDA
//! and delegates it with a post-delegation action that auto-fires
//! `ProcessWithdrawalEr` on the ER. No SPL transfer here — funds are
//! validated and debited on the ER, and the actual transfer happens
//! atomically in the post-undelegate `ExecuteWithdrawalBaseChain` action.
//!
//! The vault and user-token accounts are forwarded all the way through
//! both auto-fired actions so step 3 has everything it needs without
//! requiring another user signature.
//!
//! Account list:
//!   [0]  trader               (signer, writable, payer)
//!   [1]  system_program
//!   [2]  receipt              (writable, empty — created here)
//!   [3]  market               (read-only)
//!   [4]  base_account         (writable; forwarded)
//!   [5]  quote_account        (writable; forwarded)
//!   [6]  base_vault           (writable; forwarded)
//!   [7]  quote_vault          (writable; forwarded)
//!   [8]  token_program        (forwarded)
//!   [9]  owner_program        (= Phoenix)
//!   [10] delegation_buffer    (writable)
//!   [11] delegation_record    (writable)
//!   [12] delegation_metadata  (writable)
//!   [13] delegation_program
//!   [14] magic_program        (forwarded)
//!   [15] magic_context        (writable; forwarded)

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
    program_error::ProgramError,
    pubkey::Pubkey,
    rent::Rent,
    sysvar::Sysvar,
};
use std::mem::size_of;

use crate::program::{
    accounts::{MarketHeader, WithdrawalReceipt},
    error::assert_with_msg,
    system_utils::create_account,
    validation::loaders::{get_vault_address, get_withdrawal_receipt_address},
    PhoenixInstruction,
};

#[derive(BorshDeserialize, BorshSerialize)]
pub struct RequestWithdrawalParams {
    pub base_lots: u64,
    pub quote_lots: u64,
}

pub(crate) fn process_request_withdrawal(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let RequestWithdrawalParams { base_lots, quote_lots } =
        RequestWithdrawalParams::try_from_slice(data)?;
    assert_with_msg(
        base_lots > 0 || quote_lots > 0,
        ProgramError::InvalidInstructionData,
        "RequestWithdrawal must specify a non-zero amount on at least one side",
    )?;

    let account_iter = &mut accounts.iter();
    let trader_info = next_account_info(account_iter)?;
    let system_program_info = next_account_info(account_iter)?;
    let receipt_info = next_account_info(account_iter)?;
    let market_info = next_account_info(account_iter)?;
    let base_account = next_account_info(account_iter)?;
    let quote_account = next_account_info(account_iter)?;
    let base_vault = next_account_info(account_iter)?;
    let quote_vault = next_account_info(account_iter)?;
    let token_program_info = next_account_info(account_iter)?;
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
        "trader must sign RequestWithdrawal",
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

    // Validate vaults match market header (which is delegated, but byte
    // layout is preserved — read directly).
    {
        let data = market_info.try_borrow_data()?;
        let header = MarketHeader::load_bytes(&data[..size_of::<MarketHeader>()])
            .ok_or(ProgramError::InvalidAccountData)?;
        assert_with_msg(
            base_vault.key == &header.base_params.vault_key,
            ProgramError::InvalidArgument,
            "base_vault key mismatch",
        )?;
        assert_with_msg(
            quote_vault.key == &header.quote_params.vault_key,
            ProgramError::InvalidArgument,
            "quote_vault key mismatch",
        )?;
        let (expected_base_pda, _) = get_vault_address(market_info.key, &header.base_params.mint_key);
        let (expected_quote_pda, _) = get_vault_address(market_info.key, &header.quote_params.mint_key);
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
    }

    let (expected_receipt, receipt_bump) =
        get_withdrawal_receipt_address(market_info.key, trader_info.key);
    assert_with_msg(
        receipt_info.key == &expected_receipt,
        ProgramError::InvalidSeeds,
        "WithdrawalReceipt PDA mismatch",
    )?;
    assert_with_msg(
        receipt_info.owner == &solana_program::system_program::ID
            && receipt_info.data_is_empty(),
        ProgramError::InvalidAccountData,
        "WithdrawalReceipt must be uninitialized (close any prior receipt first)",
    )?;

    // Create the receipt PDA owned by Phoenix.
    let rent = Rent::get()?;
    let seeds: Vec<Vec<u8>> = vec![
        b"withdrawal_receipt".to_vec(),
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
        size_of::<WithdrawalReceipt>() as u64,
        seeds,
    )?;
    {
        let mut data = receipt_info.try_borrow_mut_data()?;
        let receipt =
            WithdrawalReceipt::load_mut_bytes(&mut data[..size_of::<WithdrawalReceipt>()])
                .ok_or(ProgramError::InvalidAccountData)?;
        *receipt = WithdrawalReceipt::new_init(
            *trader_info.key,
            *market_info.key,
            base_lots,
            quote_lots,
            receipt_bump,
        )?;
    }

    // Build the post-delegation action: ProcessWithdrawalEr fires on the ER
    // with ALL the forwarded accounts (vault/user-token/token-program) so
    // it can later schedule the post-undelegate settlement.
    let process_er_ix = SolInstruction {
        program_id: crate::ID.to_bytes().into(),
        accounts: vec![
            SolAccountMeta::new(trader_info.key.to_bytes().into(), true),
            SolAccountMeta::new(market_info.key.to_bytes().into(), false),
            SolAccountMeta::new(receipt_info.key.to_bytes().into(), false),
            SolAccountMeta::new_readonly(magic_program_info.key.to_bytes().into(), false),
            SolAccountMeta::new(magic_context_info.key.to_bytes().into(), false),
            // Forwarded to step 3:
            SolAccountMeta::new(base_account.key.to_bytes().into(), false),
            SolAccountMeta::new(quote_account.key.to_bytes().into(), false),
            SolAccountMeta::new(base_vault.key.to_bytes().into(), false),
            SolAccountMeta::new(quote_vault.key.to_bytes().into(), false),
            SolAccountMeta::new_readonly(token_program_info.key.to_bytes().into(), false),
        ],
        data: vec![PhoenixInstruction::ProcessWithdrawalEr as u8],
    };

    let pda_seeds: &[&[u8]] = &[
        b"withdrawal_receipt",
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
