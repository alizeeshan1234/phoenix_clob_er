//! ProcessWithdrawalEr — step 2 of 3, ER, **auto-fired** by the
//! post-delegation action scheduled in `RequestWithdrawal`.
//!
//! Reads the delegated receipt + delegated market, debits TraderState
//! (clamped to free balance), then schedules the post-undelegate
//! `ExecuteWithdrawalBaseChain` action with ALL the accounts it needs
//! (forwarded from RequestWithdrawal).
//!
//! Account list (must match the post-delegation action baked by
//! RequestWithdrawal):
//!   [0] trader        (signer via escrow chain)
//!   [1] market        (writable; delegated)
//!   [2] receipt       (writable; delegated WithdrawalReceipt)
//!   [3] magic_program
//!   [4] magic_context (writable)
//!   [5] base_account  (writable; forwarded to step 3)
//!   [6] quote_account (writable; forwarded to step 3)
//!   [7] base_vault    (writable; forwarded to step 3)
//!   [8] quote_vault   (writable; forwarded to step 3)
//!   [9] token_program (forwarded to step 3)

use ephemeral_rollups_sdk::{
    consts::{DELEGATION_PROGRAM_ID, MAGIC_CONTEXT_ID, MAGIC_PROGRAM_ID},
    ephem::{CallHandler, FoldableIntentBuilder, MagicIntentBundleBuilder},
};
use magicblock_magic_program_api::args::{ActionArgs, ShortAccountMeta};
use sokoban::ZeroCopy;
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    program_error::ProgramError,
    pubkey::Pubkey,
};
use std::mem::size_of;

use crate::{
    program::{
        accounts::{MarketHeader, WithdrawalReceipt},
        dispatch_market::load_with_dispatch_mut,
        error::assert_with_msg,
        validation::loaders::get_withdrawal_receipt_address,
        PhoenixInstruction,
    },
    quantities::{BaseLots, QuoteLots, WrapperU64},
    state::trader_state::TraderState,
};

pub(crate) fn process_process_withdrawal_er(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    _data: &[u8],
) -> ProgramResult {
    let account_iter = &mut accounts.iter();
    let trader_info = next_account_info(account_iter)?;
    let market_info = next_account_info(account_iter)?;
    let receipt_info = next_account_info(account_iter)?;
    let magic_program = next_account_info(account_iter)?;
    let magic_context = next_account_info(account_iter)?;
    let base_account = next_account_info(account_iter)?;
    let quote_account = next_account_info(account_iter)?;
    let base_vault = next_account_info(account_iter)?;
    let quote_vault = next_account_info(account_iter)?;
    let token_program = next_account_info(account_iter)?;

    assert_with_msg(
        trader_info.is_signer,
        ProgramError::MissingRequiredSignature,
        "trader must sign ProcessWithdrawalEr (via escrow authority chain)",
    )?;
    assert_with_msg(
        magic_program.key == &MAGIC_PROGRAM_ID,
        ProgramError::IncorrectProgramId,
        "Invalid MagicBlock program id",
    )?;
    assert_with_msg(
        magic_context.key == &MAGIC_CONTEXT_ID,
        ProgramError::InvalidArgument,
        "Invalid MagicBlock context id",
    )?;
    // On the ER, delegated accounts are replicated under their original
    // program owner (Phoenix), not under the delegation program.
    assert_with_msg(
        market_info.owner == &crate::ID || market_info.owner == &DELEGATION_PROGRAM_ID,
        ProgramError::IllegalOwner,
        "Market must be owned by Phoenix (ER) or the delegation program (base)",
    )?;
    assert_with_msg(
        receipt_info.owner == &crate::ID || receipt_info.owner == &DELEGATION_PROGRAM_ID,
        ProgramError::IllegalOwner,
        "Receipt must be owned by Phoenix (ER) or the delegation program (base)",
    )?;

    let (receipt_trader, requested_base, requested_quote) = {
        let data = receipt_info.try_borrow_data()?;
        let receipt = WithdrawalReceipt::load_bytes(&data[..size_of::<WithdrawalReceipt>()])
            .ok_or(ProgramError::InvalidAccountData)?;
        assert_with_msg(
            receipt.processed == 0,
            ProgramError::InvalidAccountData,
            "WithdrawalReceipt already processed",
        )?;
        assert_with_msg(
            &receipt.trader == trader_info.key,
            ProgramError::InvalidArgument,
            "Receipt trader does not match signing trader",
        )?;
        assert_with_msg(
            &receipt.market == market_info.key,
            ProgramError::InvalidArgument,
            "Receipt market does not match market account",
        )?;
        let (expected_pda, _) =
            get_withdrawal_receipt_address(&receipt.market, &receipt.trader);
        assert_with_msg(
            &expected_pda == receipt_info.key,
            ProgramError::InvalidSeeds,
            "Receipt PDA mismatch",
        )?;
        (receipt.trader, receipt.base_lots, receipt.quote_lots)
    };

    let size_params = {
        let data = market_info.try_borrow_data()?;
        let header = MarketHeader::load_bytes(&data[..size_of::<MarketHeader>()])
            .ok_or(ProgramError::InvalidAccountData)?;
        header.market_size_params
    };

    let (debited_base, debited_quote) = {
        let mut data = market_info.try_borrow_mut_data()?;
        let market_bytes = &mut data[size_of::<MarketHeader>()..];
        let market = load_with_dispatch_mut(&size_params, market_bytes)?.inner;
        let trader_state: &mut TraderState = market
            .get_trader_state_mut(&receipt_trader)
            .ok_or(ProgramError::InvalidAccountData)?;
        let avail_base = trader_state.base_lots_free.as_u64();
        let avail_quote = trader_state.quote_lots_free.as_u64();
        let debit_base = requested_base.min(avail_base);
        let debit_quote = requested_quote.min(avail_quote);
        assert_with_msg(
            debit_base > 0 || debit_quote > 0,
            ProgramError::InsufficientFunds,
            "Trader has zero free funds on both sides",
        )?;
        if debit_base > 0 {
            trader_state.use_free_base_lots(BaseLots::new(debit_base));
        }
        if debit_quote > 0 {
            trader_state.use_free_quote_lots(QuoteLots::new(debit_quote));
        }
        (debit_base, debit_quote)
    };

    {
        let mut data = receipt_info.try_borrow_mut_data()?;
        let receipt =
            WithdrawalReceipt::load_mut_bytes(&mut data[..size_of::<WithdrawalReceipt>()])
                .ok_or(ProgramError::InvalidAccountData)?;
        receipt.base_lots = debited_base;
        receipt.quote_lots = debited_quote;
        receipt.processed = 1;
    }

    // ExecuteWithdrawalBaseChain expects 8 accounts (no payer):
    //   [0] trader, [1] market, [2] base_account, [3] quote_account,
    //   [4] base_vault, [5] quote_vault, [6] token_program, [7] receipt
    // Trader is NOT a signer — MagicBlock validator signs the outer tx.
    let exec_action = CallHandler {
        args: ActionArgs::new(vec![PhoenixInstruction::ExecuteWithdrawalBaseChain as u8]),
        compute_units: 100_000,
        escrow_authority: trader_info.clone(),
        destination_program: crate::ID,
        accounts: vec![
            ShortAccountMeta {
                pubkey: trader_info.key.to_bytes().into(),
                is_writable: true,
            },
            ShortAccountMeta {
                pubkey: market_info.key.to_bytes().into(),
                is_writable: false,
            },
            ShortAccountMeta {
                pubkey: base_account.key.to_bytes().into(),
                is_writable: true,
            },
            ShortAccountMeta {
                pubkey: quote_account.key.to_bytes().into(),
                is_writable: true,
            },
            ShortAccountMeta {
                pubkey: base_vault.key.to_bytes().into(),
                is_writable: true,
            },
            ShortAccountMeta {
                pubkey: quote_vault.key.to_bytes().into(),
                is_writable: true,
            },
            ShortAccountMeta {
                pubkey: token_program.key.to_bytes().into(),
                is_writable: false,
            },
            ShortAccountMeta {
                pubkey: receipt_info.key.to_bytes().into(),
                is_writable: true,
            },
        ],
    };

    MagicIntentBundleBuilder::new(
        trader_info.clone(),
        magic_context.clone(),
        magic_program.clone(),
    )
    .commit_and_undelegate(&[receipt_info.clone()])
    .add_post_undelegate_actions([exec_action])
    .build_and_invoke()?;

    Ok(())
}
