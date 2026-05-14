//! ProcessCollateralDepositEr — ER, auto-fired post-delegation.
//! Stage 2 of 3. Mirrors `phoenix-v1/src/program/processor/process_deposit_er.rs`.
//!
//! 1. Load delegated DepositReceipt; check processed == 0.
//! 2. Credit `TraderAccount.collateral += receipt.amount`.
//! 3. Bump `GlobalState.{v_total_pool_value, c_total_collateral}`.
//! 4. Mark `receipt.processed = 1`.
//! 5. `MagicIntentBundleBuilder
//!        .commit_and_undelegate(&[receipt])
//!        .add_post_undelegate_actions([CloseCollateralDepositReceipt])
//!        .build_and_invoke()`
//!
//! Account list:
//!   [0] trader            (signer via escrow authority)
//!   [1] receipt           (writable; delegated DepositReceipt)
//!   [2] trader_account    (writable; delegated TraderAccount)
//!   [3] global_state      (writable; delegated GlobalState)
//!   [4] magic_program     (Magic111...)
//!   [5] magic_context     (writable; MagicContext1...)

use ephemeral_rollups_sdk::{
    consts::{DELEGATION_PROGRAM_ID, MAGIC_CONTEXT_ID, MAGIC_PROGRAM_ID},
    ephem::{CallHandler, FoldableIntentBuilder, MagicIntentBundleBuilder},
};
use magicblock_magic_program_api::args::{ActionArgs, ShortAccountMeta};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    program_error::ProgramError,
    pubkey::Pubkey,
};
use std::mem::size_of;

use crate::{
    error::{assert_with_msg, PerpRouterError},
    state::{DepositReceipt, GlobalState, TraderAccount},
    validation::loaders::{
        find_deposit_receipt_address, find_global_state_address,
        find_trader_account_address,
    },
    PerpRouterInstruction,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let it = &mut accounts.iter();
    let trader = next_account_info(it)?;
    let receipt_info = next_account_info(it)?;
    let trader_account_info = next_account_info(it)?;
    let global_state_info = next_account_info(it)?;
    let magic_program = next_account_info(it)?;
    let magic_context = next_account_info(it)?;

    assert_with_msg(
        trader.is_signer,
        ProgramError::MissingRequiredSignature,
        "trader must sign ProcessCollateralDepositEr (via escrow authority chain)",
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
    // On ER, delegated accounts are replicated under their original program
    // owner (perp_router). On base they're owned by the delegation program.
    // Accept either so the same code path is usable both ways.
    for info in [receipt_info, trader_account_info, global_state_info] {
        assert_with_msg(
            info.owner == &crate::ID || info.owner == &DELEGATION_PROGRAM_ID,
            ProgramError::IllegalOwner,
            "Delegated account must be owned by perp_router or delegation program",
        )?;
    }

    // Snapshot receipt fields.
    let (receipt_trader, amount) = {
        let buf = receipt_info.try_borrow_data()?;
        let r = bytemuck::from_bytes::<DepositReceipt>(&buf[..size_of::<DepositReceipt>()]);
        assert_with_msg(
            r.processed == 0,
            PerpRouterError::ReceiptAlreadyProcessed,
            "DepositReceipt already processed",
        )?;
        assert_with_msg(
            &r.trader == trader.key,
            ProgramError::InvalidArgument,
            "receipt.trader != signing trader",
        )?;
        let (expected, _) = find_deposit_receipt_address(&r.trader, program_id);
        assert_with_msg(
            &expected == receipt_info.key,
            PerpRouterError::InvalidPda,
            "DepositReceipt PDA mismatch",
        )?;
        (r.trader, r.amount)
    };

    // Credit TraderAccount.collateral.
    {
        let mut buf = trader_account_info.try_borrow_mut_data()?;
        let t = bytemuck::from_bytes_mut::<TraderAccount>(
            &mut buf[..size_of::<TraderAccount>()],
        );
        assert_with_msg(
            t.owner == receipt_trader,
            ProgramError::InvalidArgument,
            "trader_account.owner != receipt.trader",
        )?;
        let (expected, _) = find_trader_account_address(&t.owner, program_id);
        assert_with_msg(
            &expected == trader_account_info.key,
            PerpRouterError::InvalidPda,
            "TraderAccount PDA mismatch",
        )?;
        t.collateral = t
            .collateral
            .checked_add(amount)
            .ok_or(PerpRouterError::MathOverflow)?;
    }

    // Bump GlobalState balance sheet.
    {
        let mut buf = global_state_info.try_borrow_mut_data()?;
        let g = bytemuck::from_bytes_mut::<GlobalState>(
            &mut buf[..size_of::<GlobalState>()],
        );
        let (expected, _) = find_global_state_address(program_id);
        assert_with_msg(
            &expected == global_state_info.key,
            PerpRouterError::InvalidPda,
            "GlobalState PDA mismatch",
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

    // Mark receipt processed.
    {
        let mut buf = receipt_info.try_borrow_mut_data()?;
        let r = bytemuck::from_bytes_mut::<DepositReceipt>(
            &mut buf[..size_of::<DepositReceipt>()],
        );
        r.processed = 1;
    }

    // Schedule CloseCollateralDepositReceipt as a post-undelegate action.
    let close_action = CallHandler {
        args: ActionArgs::new(vec![
            PerpRouterInstruction::CloseCollateralDepositReceipt as u8,
        ]),
        compute_units: 30_000,
        escrow_authority: trader.clone(),
        destination_program: crate::ID,
        accounts: vec![
            ShortAccountMeta {
                pubkey: trader.key.to_bytes().into(),
                is_writable: true,
            },
            ShortAccountMeta {
                pubkey: receipt_info.key.to_bytes().into(),
                is_writable: true,
            },
        ],
    };

    MagicIntentBundleBuilder::new(
        trader.clone(),
        magic_context.clone(),
        magic_program.clone(),
    )
    .commit_and_undelegate(&[receipt_info.clone()])
    .add_post_undelegate_actions([close_action])
    .build_and_invoke()?;

    Ok(())
}
