//! ProcessCollateralWithdrawalEr — ER, auto-fired post-delegation.
//! Stage 2 of 3. THE HAIRCUT RUNS HERE.
//!
//! 1. Read receipt.gross_amount.
//! 2. Split the request: pull from collateral first (senior, no haircut),
//!    then from matured PnL (junior, haircut applied).
//! 3. Compute h = compute_h(V, C, I, total_matured_PnL) from GlobalState.
//! 4. Debit TraderAccount; decrement GlobalState totals.
//! 5. Write net_amount + h_num + h_den to receipt; mark processed = 1.
//! 6. commit_and_undelegate + schedule ExecuteCollateralWithdrawalBaseChain.
//!
//! Account list (must match the post-delegation action baked by stage 1):
//!   [0] trader            (signer via escrow chain)
//!   [1] receipt           (writable; delegated WithdrawalReceipt)
//!   [2] magic_program     (Magic111...)
//!   [3] magic_context     (writable)
//!   [4] trader_token      (writable; forwarded → stage 3)
//!   [5] collateral_vault  (writable; forwarded → stage 3)
//!   [6] quote_mint        (readonly; forwarded → stage 3)
//!   [7] token_program     (forwarded → stage 3)
//!
//! Additional delegated singletons (NOT in account list — resolved by ER
//! via their delegation slots):
//!   - trader_account
//!   - global_state
//!
//! Note: the ER must surface trader_account and global_state via remaining
//! accounts; client constructs them. We accept them after the fixed slots.

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
    risk::haircut::{compute_h, split_withdrawal},
    state::{GlobalState, TraderAccount, WithdrawalReceipt},
    validation::loaders::{
        find_global_state_address, find_perp_authority_address,
        find_trader_account_address, find_withdrawal_receipt_address,
    },
    PerpRouterInstruction,
};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let it = &mut accounts.iter();
    let trader = next_account_info(it)?;
    let receipt_info = next_account_info(it)?;
    let magic_program = next_account_info(it)?;
    let magic_context = next_account_info(it)?;
    let trader_token = next_account_info(it)?;
    let collateral_vault = next_account_info(it)?;
    let quote_mint = next_account_info(it)?;
    let token_program = next_account_info(it)?;
    // Delegated singletons (passed by client as remaining accounts):
    let trader_account_info = next_account_info(it)?;
    let global_state_info = next_account_info(it)?;

    assert_with_msg(
        trader.is_signer,
        ProgramError::MissingRequiredSignature,
        "trader must sign ProcessCollateralWithdrawalEr (via escrow chain)",
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
    for info in [
        receipt_info,
        trader_account_info,
        global_state_info,
    ] {
        assert_with_msg(
            info.owner == &crate::ID || info.owner == &DELEGATION_PROGRAM_ID,
            ProgramError::IllegalOwner,
            "Delegated account must be owned by perp_router or delegation program",
        )?;
    }

    // --- Snapshot receipt ---
    let (receipt_trader, gross_amount) = {
        let buf = receipt_info.try_borrow_data()?;
        let r = bytemuck::from_bytes::<WithdrawalReceipt>(
            &buf[..size_of::<WithdrawalReceipt>()],
        );
        assert_with_msg(
            r.processed == 0,
            PerpRouterError::ReceiptAlreadyProcessed,
            "WithdrawalReceipt already processed",
        )?;
        assert_with_msg(
            &r.trader == trader.key,
            ProgramError::InvalidArgument,
            "receipt.trader != signing trader",
        )?;
        let (expected, _) = find_withdrawal_receipt_address(&r.trader, program_id);
        assert_with_msg(
            &expected == receipt_info.key,
            PerpRouterError::InvalidPda,
            "WithdrawalReceipt PDA mismatch",
        )?;
        (r.trader, r.gross_amount)
    };

    // --- Validate PDAs ---
    {
        let buf = trader_account_info.try_borrow_data()?;
        let t = bytemuck::from_bytes::<TraderAccount>(&buf[..size_of::<TraderAccount>()]);
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
    }
    {
        let (expected, _) = find_global_state_address(program_id);
        assert_with_msg(
            &expected == global_state_info.key,
            PerpRouterError::InvalidPda,
            "GlobalState PDA mismatch",
        )?;
    }

    // --- Compute split (collateral senior, PnL junior) + haircut ---
    let g_snap = {
        let buf = global_state_info.try_borrow_data()?;
        *bytemuck::from_bytes::<GlobalState>(&buf[..size_of::<GlobalState>()])
    };
    let (h_num, h_den) = compute_h(
        g_snap.v_total_pool_value,
        g_snap.c_total_collateral,
        g_snap.i_insurance_reserve,
        g_snap.total_matured_pnl,
    );

    let (collateral_request, pnl_request, net_amount) = {
        let buf = trader_account_info.try_borrow_data()?;
        let t = bytemuck::from_bytes::<TraderAccount>(&buf[..size_of::<TraderAccount>()]);
        let coll_req = gross_amount.min(t.collateral);
        let pnl_req = gross_amount.saturating_sub(coll_req).min(t.pnl_matured);
        let (c_pay, p_pay, total) = split_withdrawal(coll_req, pnl_req, h_num, h_den)?;
        let _ = c_pay; // == coll_req
        let _ = p_pay; // == apply_h(pnl_req, h_num, h_den)
        (coll_req, pnl_req, total)
    };

    // --- Mutate TraderAccount + GlobalState ---
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
        // Pool value drops by what we actually pay out (net), not gross.
        g.v_total_pool_value = g
            .v_total_pool_value
            .checked_sub(net_amount)
            .ok_or(PerpRouterError::MathOverflow)?;
        g.c_total_collateral = g
            .c_total_collateral
            .checked_sub(collateral_request)
            .ok_or(PerpRouterError::MathOverflow)?;
        g.total_matured_pnl = g
            .total_matured_pnl
            .checked_sub(pnl_request)
            .ok_or(PerpRouterError::MathOverflow)?;
    }

    // --- Write back receipt with audit values ---
    {
        let mut buf = receipt_info.try_borrow_mut_data()?;
        let r = bytemuck::from_bytes_mut::<WithdrawalReceipt>(
            &mut buf[..size_of::<WithdrawalReceipt>()],
        );
        r.net_amount = net_amount;
        r.h_numerator = h_num;
        r.h_denominator = h_den;
        r.processed = 1;
    }

    // --- Schedule stage 3: ExecuteCollateralWithdrawalBaseChain ---
    let (perp_authority_pda, _) = find_perp_authority_address(program_id);
    let execute_action = CallHandler {
        args: ActionArgs::new(vec![
            PerpRouterInstruction::ExecuteCollateralWithdrawalBaseChain as u8,
        ]),
        compute_units: 50_000,
        escrow_authority: trader.clone(),
        destination_program: crate::ID,
        accounts: vec![
            ShortAccountMeta { pubkey: trader.key.to_bytes().into(),            is_writable: true  },
            ShortAccountMeta { pubkey: receipt_info.key.to_bytes().into(),      is_writable: true  },
            ShortAccountMeta { pubkey: trader_token.key.to_bytes().into(),      is_writable: true  },
            ShortAccountMeta { pubkey: collateral_vault.key.to_bytes().into(),  is_writable: true  },
            ShortAccountMeta { pubkey: quote_mint.key.to_bytes().into(),        is_writable: false },
            ShortAccountMeta { pubkey: token_program.key.to_bytes().into(),     is_writable: false },
            ShortAccountMeta { pubkey: perp_authority_pda.to_bytes().into(),    is_writable: false },
        ],
    };

    MagicIntentBundleBuilder::new(
        trader.clone(),
        magic_context.clone(),
        magic_program.clone(),
    )
    .commit_and_undelegate(&[receipt_info.clone()])
    .add_post_undelegate_actions([execute_action])
    .build_and_invoke()?;

    Ok(())
}
