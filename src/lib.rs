//! Phoenix is a limit order book exchange on the Solana blockchain.
//!
//! It exposes a set of instructions to create, cancel, and fill orders.
//! Each event that modifies the state of the book is recorded in an event log which can
//! be queried from a transaction signature after each transaction is confirmed. This
//! allows clients to build their own order book and trade history.
//!
//! The program is able to atomically match orders and settle trades on chain. This
//! is because each market has a fixed set of users that are allowed to place limit
//! orders on the book. Users who swap against the book will have their funds settle
//! instantaneously, while the funds of users who place orders on the book will be
//! immediately available for withdraw post fill.

#[macro_use]
mod log;
pub mod program;
pub mod quantities;
mod shank_structs;
pub mod state;

use crate::program::processor::*;

use borsh::BorshSerialize;
use solana_program::{declare_id, program::set_return_data, pubkey::Pubkey};

use program::{
    assert_with_msg, event_recorder::EventRecorder, PhoenixInstruction, PhoenixLogContext,
    PhoenixMarketContext,
};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    program_error::ProgramError,
};
use state::markets::MarketEvent;

#[cfg(not(feature = "no-entrypoint"))]
use solana_security_txt::security_txt;

#[cfg(not(feature = "no-entrypoint"))]
security_txt! {
    // Required fields
    name: "Phoenix V1",
    project_url: "https://ellipsislabs.xyz/",
    contacts: "email:maintainers@ellipsislabs.xyz",
    policy: "https://github.com/Ellipsis-Labs/phoenix-v1/blob/master/SECURITY.md",
    // Optional Fields
    preferred_languages: "en",
    source_code: "https://github.com/Ellipsis-Labs/phoenix-v1",
    auditors: "contact@osec.io"
}

declare_id!("AwCrmRz5wSst99eYA7to6vtqCQ7QmCkxV29gVFxwMUXb");

/// Static PDA with seeds `[b"log"]`. Used by the program's self-CPI event
/// recorder so client indexers can parse inner-instruction data without
/// trusting the caller.

/// If the program id changes, this address (and its bump) must be
/// recomputed. The compile-time `declare_pda!` from ellipsis-macros (1.14
/// era) is replaced here by an inline `pubkey!` constant for the address
/// and a runtime `find_program_address` for the bump.
pub mod phoenix_log_authority {
    use solana_program::pubkey::Pubkey;

    /// PDA address — `find_program_address(&[b"log"], &phoenix_id())`.
    /// Computed at runtime so it tracks the deployed program ID.
    #[inline]
    pub fn id() -> Pubkey {
        Pubkey::find_program_address(&[b"log"], &super::id()).0
    }

    /// Bump seed for the log authority PDA.
    #[inline]
    pub fn bump() -> u8 {
        Pubkey::find_program_address(&[b"log"], &super::id()).1
    }
}

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process_instruction);

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    // The MagicBlock delegation program calls back into Phoenix with an
    // 8-byte discriminator after a commit-and-undelegate completes. Tail of
    // the instruction data is a borsh-serialized `Vec<Vec<u8>>` of PDA seeds.
    // Handle this before single-byte instruction dispatch.
    if instruction_data.len() >= 8
        && instruction_data[..8] == ephemeral_rollups_sdk::consts::EXTERNAL_UNDELEGATE_DISCRIMINATOR
    {
        let account_seeds: Vec<Vec<u8>> =
            borsh::BorshDeserialize::try_from_slice(&instruction_data[8..])
                .map_err(|_| ProgramError::InvalidInstructionData)?;
        return crate::program::processor::undelegate_market::process_undelegate_market_with_seeds(
            program_id,
            accounts,
            account_seeds,
        );
    }

    let (tag, data) = instruction_data
        .split_first()
        .ok_or(ProgramError::InvalidInstructionData)?;

    let instruction =
        PhoenixInstruction::try_from(*tag).or(Err(ProgramError::InvalidInstructionData))?;

    // MagicBlock Ephemeral Rollup integration. These ixs do NOT follow the
    // standard `[phoenix_program, log_authority, market, signer]` first-4
    // convention used by Phoenix's matching/governance ixs, so dispatch
    // them before the `split_at(4)` and context loading below.
    match instruction {
        PhoenixInstruction::DelegateMarket => {
            phoenix_log!("PhoenixInstruction::DelegateMarket");
            return crate::program::processor::delegate_market::process_delegate_market(
                program_id, accounts, data,
            );
        }
        PhoenixInstruction::CommitMarket => {
            phoenix_log!("PhoenixInstruction::CommitMarket");
            return crate::program::processor::commit_market::process_commit_market(
                program_id, accounts, data,
            );
        }
        PhoenixInstruction::CommitAndUndelegateMarket => {
            phoenix_log!("PhoenixInstruction::CommitAndUndelegateMarket");
            return crate::program::processor::commit_and_undelegate_market::process_commit_and_undelegate_market(
                program_id, accounts, data,
            );
        }
        PhoenixInstruction::DelegateSeat => {
            phoenix_log!("PhoenixInstruction::DelegateSeat");
            return crate::program::processor::delegate_seat::process_delegate_seat(
                program_id, accounts, data,
            );
        }
        PhoenixInstruction::RequestDeposit => {
            phoenix_log!("PhoenixInstruction::RequestDeposit");
            return crate::program::processor::request_deposit::process_request_deposit(
                program_id, accounts, data,
            );
        }
        PhoenixInstruction::ProcessDepositEr => {
            phoenix_log!("PhoenixInstruction::ProcessDepositEr");
            return crate::program::processor::process_deposit_er::process_process_deposit_er(
                program_id, accounts, data,
            );
        }
        PhoenixInstruction::CloseDepositReceipt => {
            phoenix_log!("PhoenixInstruction::CloseDepositReceipt");
            return crate::program::processor::close_deposit_receipt::process_close_deposit_receipt(
                program_id, accounts, data,
            );
        }
        PhoenixInstruction::RequestWithdrawal => {
            phoenix_log!("PhoenixInstruction::RequestWithdrawal");
            return crate::program::processor::request_withdrawal::process_request_withdrawal(
                program_id, accounts, data,
            );
        }
        PhoenixInstruction::ProcessWithdrawalEr => {
            phoenix_log!("PhoenixInstruction::ProcessWithdrawalEr");
            return crate::program::processor::process_withdrawal_er::process_process_withdrawal_er(
                program_id, accounts, data,
            );
        }
        PhoenixInstruction::ExecuteWithdrawalBaseChain => {
            phoenix_log!("PhoenixInstruction::ExecuteWithdrawalBaseChain");
            return crate::program::processor::execute_withdrawal_base_chain::process_execute_withdrawal_base_chain(
                program_id, accounts, data,
            );
        }
        PhoenixInstruction::CreateSessionToken => {
            phoenix_log!("PhoenixInstruction::CreateSessionToken");
            return crate::program::processor::create_session_token::process_create_session_token(
                program_id, accounts, data,
            );
        }
        PhoenixInstruction::RevokeSessionToken => {
            phoenix_log!("PhoenixInstruction::RevokeSessionToken");
            return crate::program::processor::revoke_session_token::process_revoke_session_token(
                program_id, accounts, data,
            );
        }
        PhoenixInstruction::PlaceLimitOrderViaSession
        | PhoenixInstruction::SwapViaSession
        | PhoenixInstruction::CancelAllOrdersViaSession => {
            phoenix_log!("PhoenixInstruction::*ViaSession");
            return crate::program::processor::session::dispatch(
                instruction, program_id, accounts, data,
            );
        }
        _ => {}
    }

    // This is a special instruction that is only used for recording
    // inner instruction data from recursive CPI calls.
    //
    // Market events can be searched by querying the transaction hash and parsing
    // the inner instruction data according to a pre-defined schema.
    //
    // Only the log authority is allowed to call this instruction.
    if let PhoenixInstruction::Log = instruction {
        let authority = next_account_info(&mut accounts.iter())?;
        assert_with_msg(
            authority.is_signer,
            ProgramError::MissingRequiredSignature,
            "Log authority must sign through CPI",
        )?;
        assert_with_msg(
            authority.key == &phoenix_log_authority::id(),
            ProgramError::InvalidArgument,
            "Invalid log authority",
        )?;
        return Ok(());
    }

    let (program_accounts, accounts) = accounts.split_at(4);
    let accounts_iter = &mut program_accounts.iter();
    let phoenix_log_context = PhoenixLogContext::load(accounts_iter)?;
    let market_context = if instruction == PhoenixInstruction::InitializeMarket {
        // If the market account is still system-owned (uninit), allocate it
        // as a PDA before anything reads the header. No-op when the caller
        // pre-allocated the account (legacy keypair flow).
        initialize::maybe_allocate_market_pda(
            program_id,
            &program_accounts[2], // market
            &program_accounts[3], // market_creator
            accounts,
            data,
        )?;
        PhoenixMarketContext::load_init(accounts_iter)?
    } else {
        PhoenixMarketContext::load(accounts_iter)?
    };

    let mut event_recorder = EventRecorder::new(phoenix_log_context, &market_context, instruction)?;

    let mut record_event_fn = |e: MarketEvent<Pubkey>| event_recorder.add_event(e);
    let mut order_ids = Vec::new();

    match instruction {
        PhoenixInstruction::InitializeMarket => {
            phoenix_log!("PhoenixInstruction::Initialize");
            initialize::process_initialize_market(program_id, &market_context, accounts, data)?
        }
        PhoenixInstruction::Swap => {
            phoenix_log!("PhoenixInstruction::Swap");
            new_order::process_swap(
                program_id,
                &market_context,
                accounts,
                data,
                &mut record_event_fn,
            )?;
        }
        PhoenixInstruction::SwapWithFreeFunds => {
            phoenix_log!("PhoenixInstruction::SwapWithFreeFunds");
            new_order::process_swap_with_free_funds(
                program_id,
                &market_context,
                accounts,
                data,
                &mut record_event_fn,
            )?;
        }
        PhoenixInstruction::PlaceLimitOrder => {
            phoenix_log!("PhoenixInstruction::PlaceLimitOrder");
            new_order::process_place_limit_order(
                program_id,
                &market_context,
                accounts,
                data,
                &mut record_event_fn,
                &mut order_ids,
            )?
        }
        PhoenixInstruction::PlaceLimitOrderWithFreeFunds => {
            phoenix_log!("PhoenixInstruction::PlaceLimitOrderWithFreeFunds");
            new_order::process_place_limit_order_with_free_funds(
                program_id,
                &market_context,
                accounts,
                data,
                &mut record_event_fn,
                &mut order_ids,
            )?;
        }
        PhoenixInstruction::PlaceMultiplePostOnlyOrders => {
            phoenix_log!("PhoenixInstruction::PlaceMultiplePostOnlyOrders");
            new_order::process_place_multiple_post_only_orders(
                program_id,
                &market_context,
                accounts,
                data,
                &mut record_event_fn,
                &mut order_ids,
            )?;
        }
        PhoenixInstruction::PlaceMultiplePostOnlyOrdersWithFreeFunds => {
            phoenix_log!("PhoenixInstruction::PlaceMultiplePostOnlyOrdersWithFreeFunds");
            new_order::process_place_multiple_post_only_orders_with_free_funds(
                program_id,
                &market_context,
                accounts,
                data,
                &mut record_event_fn,
                &mut order_ids,
            )?;
        }
        PhoenixInstruction::ReduceOrder => {
            phoenix_log!("PhoenixInstruction::ReduceOrder");
            reduce_order::process_reduce_order(
                program_id,
                &market_context,
                accounts,
                data,
                true,
                &mut record_event_fn,
            )?
        }
        PhoenixInstruction::ReduceOrderWithFreeFunds => {
            phoenix_log!("PhoenixInstruction::ReduceOrderWithFreeFunds");
            reduce_order::process_reduce_order(
                program_id,
                &market_context,
                accounts,
                data,
                false,
                &mut record_event_fn,
            )?
        }
        PhoenixInstruction::CancelAllOrders => {
            phoenix_log!("PhoenixInstruction::CancelAllOrders");
            cancel_multiple_orders::process_cancel_all_orders(
                program_id,
                &market_context,
                accounts,
                data,
                true,
                &mut record_event_fn,
            )?
        }
        PhoenixInstruction::CancelAllOrdersWithFreeFunds => {
            phoenix_log!("PhoenixInstruction::CancelAllOrdersWithFreeFunds");
            cancel_multiple_orders::process_cancel_all_orders(
                program_id,
                &market_context,
                accounts,
                data,
                false,
                &mut record_event_fn,
            )?
        }
        PhoenixInstruction::CancelUpTo => {
            phoenix_log!("PhoenixInstruction::CancelMultipleOrders");
            cancel_multiple_orders::process_cancel_up_to(
                program_id,
                &market_context,
                accounts,
                data,
                true,
                &mut record_event_fn,
            )?
        }
        PhoenixInstruction::CancelUpToWithFreeFunds => {
            phoenix_log!("PhoenixInstruction::CancelUpToWithFreeFunds");
            cancel_multiple_orders::process_cancel_up_to(
                program_id,
                &market_context,
                accounts,
                data,
                false,
                &mut record_event_fn,
            )?
        }
        PhoenixInstruction::CancelMultipleOrdersById => {
            phoenix_log!("PhoenixInstruction::CancelMultipleOrdersById");
            cancel_multiple_orders::process_cancel_multiple_orders_by_id(
                program_id,
                &market_context,
                accounts,
                data,
                true,
                &mut record_event_fn,
            )?
        }
        PhoenixInstruction::CancelMultipleOrdersByIdWithFreeFunds => {
            phoenix_log!("PhoenixInstruction::CancelMultipleOrdersByIdWithFreeFunds");
            cancel_multiple_orders::process_cancel_multiple_orders_by_id(
                program_id,
                &market_context,
                accounts,
                data,
                false,
                &mut record_event_fn,
            )?
        }
        PhoenixInstruction::WithdrawFunds => {
            phoenix_log!("PhoenixInstruction::WithdrawFunds");
            withdraw::process_withdraw_funds(program_id, &market_context, accounts, data)?;
        }
        PhoenixInstruction::DepositFunds => {
            phoenix_log!("PhoenixInstruction::DepositFunds");
            deposit::process_deposit_funds(program_id, &market_context, accounts, data)?
        }
        PhoenixInstruction::ForceCancelOrders => {
            phoenix_log!("PhoenixInstruction::ForceCancelOrders");
            governance::process_force_cancel_orders(
                program_id,
                &market_context,
                accounts,
                data,
                &mut record_event_fn,
            )?
        }
        PhoenixInstruction::EvictSeat => {
            phoenix_log!("PhoenixInstruction::EvictSeat");
            governance::process_evict_seat(program_id, &market_context, accounts, data)?
        }
        PhoenixInstruction::ClaimAuthority => {
            phoenix_log!("PhoenixInstruction::ClaimAuthority");
            governance::process_claim_authority(program_id, &market_context, data)?
        }
        PhoenixInstruction::NameSuccessor => {
            phoenix_log!("PhoenixInstruction::NameSuccessor");
            governance::process_name_successor(program_id, &market_context, data)?
        }
        PhoenixInstruction::ChangeMarketStatus => {
            phoenix_log!("PhoenixInstruction::ChangeMarketStatus");
            governance::process_change_market_status(program_id, &market_context, accounts, data)?
        }
        PhoenixInstruction::RequestSeatAuthorized => {
            phoenix_log!("PhoenixInstruction::RequestSeatAuthorized");
            manage_seat::process_request_seat_authorized(
                program_id,
                &market_context,
                accounts,
                data,
            )?
        }
        PhoenixInstruction::RequestSeat => {
            phoenix_log!("PhoenixInstruction::RequestSeat");
            manage_seat::process_request_seat(program_id, &market_context, accounts, data)?
        }
        PhoenixInstruction::ChangeSeatStatus => {
            phoenix_log!("PhoenixInstruction::ChangeSeatStatus");
            manage_seat::process_change_seat_status(program_id, &market_context, accounts, data)?;
        }
        PhoenixInstruction::CollectFees => {
            phoenix_log!("PhoenixInstruction::CollectFees");
            fees::process_collect_fees(
                program_id,
                &market_context,
                accounts,
                data,
                &mut record_event_fn,
            )?
        }
        PhoenixInstruction::ChangeFeeRecipient => {
            phoenix_log!("PhoenixInstruction::ChangeFeeRecipient");
            fees::process_change_fee_recipient(program_id, &market_context, accounts, data)?
        }
        _ => unreachable!(),
    }
    event_recorder.increment_market_sequence_number_and_flush(market_context.market_info)?;
    // We set the order ids at the end of the instruction because the return data gets cleared after
    // every CPI call.
    if !order_ids.is_empty() {
        set_return_data(order_ids.try_to_vec()?.as_ref());
    }
    Ok(())
}
