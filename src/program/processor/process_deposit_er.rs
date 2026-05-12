//! ProcessDepositEr — step 2 of 3, ER, **auto-fired** by the post-delegation
//! action scheduled in `RequestDeposit`.
//!
//! Behaviour:
//!   1. Load the delegated receipt and validate it's unprocessed.
//!   2. Open the delegated market, register the trader if needed, credit
//!      `TraderState.{base,quote}_lots_free` per the receipt amounts.
//!   3. Set `receipt.processed = 1`.
//!   4. `MagicIntentBundleBuilder::new(...).commit_and_undelegate(&[receipt])
//!      .add_post_undelegate_actions([CloseDepositReceipt]).build_and_invoke()`
//!      — the receipt is committed AND undelegated AND the
//!      CloseDepositReceipt action fires on base layer atomically.
//!
//! Account list:
//!   [0] trader        (signer per the escrow-authority chain)
//!   [1] market        (writable; delegated)
//!   [2] receipt       (writable; delegated DepositReceipt)
//!   [3] magic_program
//!   [4] magic_context (writable)

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
        accounts::{DepositReceipt, MarketHeader},
        dispatch_market::load_with_dispatch_mut,
        error::assert_with_msg,
        validation::loaders::get_deposit_receipt_address,
        PhoenixInstruction,
    },
    quantities::{BaseLots, QuoteLots, WrapperU64},
    state::trader_state::TraderState,
};

pub(crate) fn process_process_deposit_er(
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

    assert_with_msg(
        trader_info.is_signer,
        ProgramError::MissingRequiredSignature,
        "trader must sign ProcessDepositEr (via escrow authority chain)",
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
    // program owner (Phoenix), not under the delegation program. We accept
    // either owner here so the same code path works on both layers.
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

    // Snapshot receipt fields.
    let (receipt_trader, base_lots, quote_lots) = {
        let data = receipt_info.try_borrow_data()?;
        let receipt = DepositReceipt::load_bytes(&data[..size_of::<DepositReceipt>()])
            .ok_or(ProgramError::InvalidAccountData)?;
        assert_with_msg(
            receipt.processed == 0,
            ProgramError::InvalidAccountData,
            "DepositReceipt already processed",
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
            get_deposit_receipt_address(&receipt.market, &receipt.trader);
        assert_with_msg(
            &expected_pda == receipt_info.key,
            ProgramError::InvalidSeeds,
            "Receipt PDA mismatch",
        )?;
        (receipt.trader, receipt.base_lots, receipt.quote_lots)
    };

    // Credit TraderState inside the delegated market.
    let size_params = {
        let data = market_info.try_borrow_data()?;
        let header = MarketHeader::load_bytes(&data[..size_of::<MarketHeader>()])
            .ok_or(ProgramError::InvalidAccountData)?;
        header.market_size_params
    };
    {
        let mut data = market_info.try_borrow_mut_data()?;
        let market_bytes = &mut data[size_of::<MarketHeader>()..];
        let market = load_with_dispatch_mut(&size_params, market_bytes)?.inner;
        market
            .get_or_register_trader(&receipt_trader)
            .ok_or(ProgramError::AccountDataTooSmall)?;
        let trader_state: &mut TraderState = market
            .get_trader_state_mut(&receipt_trader)
            .ok_or(ProgramError::InvalidAccountData)?;
        if base_lots > 0 {
            trader_state.deposit_free_base_lots(BaseLots::new(base_lots));
        }
        if quote_lots > 0 {
            trader_state.deposit_free_quote_lots(QuoteLots::new(quote_lots));
        }
    }

    // Mark receipt processed.
    {
        let mut data = receipt_info.try_borrow_mut_data()?;
        let receipt = DepositReceipt::load_mut_bytes(&mut data[..size_of::<DepositReceipt>()])
            .ok_or(ProgramError::InvalidAccountData)?;
        receipt.processed = 1;
    }

    // Schedule the post-undelegate CloseDepositReceipt action. Account
    // order must match close_deposit_receipt.rs.
    // CloseDepositReceipt expects 2 accounts: [trader, receipt]. Trader is
    // NOT a signer here — the MagicBlock validator signs the outer tx for
    // post-undelegate actions; we only verify trader.key == receipt.trader.
    let close_action = CallHandler {
        args: ActionArgs::new(vec![PhoenixInstruction::CloseDepositReceipt as u8]),
        compute_units: 30_000,
        escrow_authority: trader_info.clone(),
        destination_program: crate::ID,
        accounts: vec![
            ShortAccountMeta {
                pubkey: trader_info.key.to_bytes().into(),
                is_writable: true,
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
    .add_post_undelegate_actions([close_action])
    .build_and_invoke()?;

    Ok(())
}
