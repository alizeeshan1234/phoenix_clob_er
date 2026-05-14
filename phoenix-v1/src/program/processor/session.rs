//! Session dispatch — handles `PlaceLimitOrderViaSession`,
//! `SwapViaSession`, and `CancelAllOrdersViaSession`.
//!
//! Verifies the session token, then constructs a `PhoenixMarketContext`
//! whose `signer` is the **owner** (not the actual transaction signer),
//! and delegates to the existing matching-engine processors. The
//! `Signer::new_session_authorized` constructor skips the `is_signer`
//! check — it's safe here because:
//!   1. The actual `session_signer` *did* sign the tx (we verify
//!      `session_signer.is_signer` below).
//!   2. The session token records who the `session_signer` is allowed to
//!      act for; we verify `session_signer.key == token.session_signer`
//!      and `owner.key == token.owner`.
//!   3. We verify the token PDA is at the canonical address, so it must
//!      have been created via `CreateSessionToken` (which requires the
//!      owner's signature).
//!   4. We check the token is not expired.
//!
//! Account list (passed in by the caller):
//!   [0] phoenix_program
//!   [1] log_authority
//!   [2] market (writable)
//!   [3] session_signer (signer)
//!   [4] owner (read-only)
//!   [5] session_token (read-only; the PDA proving authorization)
//!   [6+] inner-ix accounts (seat for trading ixs; nothing for cancel-all)

use borsh::BorshSerialize;
use sokoban::ZeroCopy;
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    clock::Clock,
    entrypoint::ProgramResult,
    program::set_return_data,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvar::Sysvar,
};
use std::mem::size_of;

use crate::{
    phoenix_log_authority,
    program::{
        accounts::SessionToken,
        error::assert_with_msg,
        event_recorder::EventRecorder,
        processor::{cancel_multiple_orders, new_order},
        validation::{
            checkers::{
                phoenix_checkers::MarketAccountInfo, Program, Signer, PDA,
            },
            loaders::{get_session_token_address, PhoenixLogContext, PhoenixMarketContext},
        },
        PhoenixInstruction,
    },
    state::markets::MarketEvent,
};

pub(crate) fn dispatch(
    instruction: PhoenixInstruction,
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let account_iter = &mut accounts.iter();
    let phoenix_program_info = next_account_info(account_iter)?;
    let log_authority_info = next_account_info(account_iter)?;
    let market_info = next_account_info(account_iter)?;
    let session_signer_info = next_account_info(account_iter)?;
    let owner_info = next_account_info(account_iter)?;
    let session_token_info = next_account_info(account_iter)?;

    // Standard program / log authority checks.
    let _phoenix_program: Program =
        Program::new(phoenix_program_info, &crate::id())?;
    let _log_authority: PDA = PDA::new(log_authority_info, &phoenix_log_authority::id())?;

    // Verify the session token + authorization.
    let session_signer = Signer::new(session_signer_info)?;
    assert_with_msg(
        session_token_info.owner == &crate::ID,
        ProgramError::IllegalOwner,
        "session_token must be owned by Phoenix",
    )?;
    {
        let token_data = session_token_info.try_borrow_data()?;
        let token = SessionToken::load_bytes(&token_data[..size_of::<SessionToken>()])
            .ok_or(ProgramError::InvalidAccountData)?;
        assert_with_msg(
            &token.owner == owner_info.key,
            ProgramError::InvalidArgument,
            "session_token.owner != passed owner account",
        )?;
        assert_with_msg(
            &token.session_signer == session_signer.key,
            ProgramError::MissingRequiredSignature,
            "session_token.session_signer != actual signer",
        )?;
        let (expected_pda, _) =
            get_session_token_address(&token.owner, &token.session_signer);
        assert_with_msg(
            session_token_info.key == &expected_pda,
            ProgramError::InvalidSeeds,
            "session_token PDA mismatch",
        )?;
        let now_unix = Clock::get()?.unix_timestamp;
        assert_with_msg(
            !token.is_expired(now_unix),
            ProgramError::InvalidAccountData,
            "session_token expired",
        )?;
    }

    // Build the same contexts the normal dispatch would have built — but
    // with the OWNER as the (session-authorized) signer.
    let market_account_info = MarketAccountInfo::new(market_info)?;
    let market_context = PhoenixMarketContext {
        market_info: market_account_info,
        signer: Signer::new_session_authorized(owner_info),
    };
    let phoenix_log_context = PhoenixLogContext {
        phoenix_program: Program::new(phoenix_program_info, &crate::id())?,
        log_authority: PDA::new(log_authority_info, &phoenix_log_authority::id())?,
    };

    let mut event_recorder =
        EventRecorder::new(phoenix_log_context, &market_context, instruction)?;
    let mut record_event_fn = |e: MarketEvent<Pubkey>| event_recorder.add_event(e);
    let mut order_ids = Vec::new();

    let inner_accounts: &[AccountInfo] = &accounts[6..];

    match instruction {
        PhoenixInstruction::PlaceLimitOrderViaSession => {
            new_order::process_place_limit_order_with_free_funds(
                program_id,
                &market_context,
                inner_accounts,
                data,
                &mut record_event_fn,
                &mut order_ids,
            )?;
        }
        PhoenixInstruction::SwapViaSession => {
            new_order::process_swap_with_free_funds(
                program_id,
                &market_context,
                inner_accounts,
                data,
                &mut record_event_fn,
            )?;
        }
        PhoenixInstruction::CancelAllOrdersViaSession => {
            cancel_multiple_orders::process_cancel_all_orders(
                program_id,
                &market_context,
                inner_accounts,
                data,
                false, // claim_funds=false (free-funds variant)
                &mut record_event_fn,
            )?;
        }
        _ => unreachable!("session::dispatch called with non-session instruction"),
    }

    event_recorder.increment_market_sequence_number_and_flush(market_context.market_info)?;
    if !order_ids.is_empty() {
        set_return_data(order_ids.try_to_vec()?.as_ref());
    }
    Ok(())
}
