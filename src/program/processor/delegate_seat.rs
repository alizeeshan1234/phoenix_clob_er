//! DelegateSeat — base-layer instruction that hands a Seat PDA to the
//! MagicBlock delegation program so seat-gated trading ixs
//! (PlaceLimitOrder*, SwapWithFreeFunds, PlaceMultiplePostOnly*) can run
//! on the ER.
//!
//! Flow:
//!   1. Verify caller is the market's authority.
//!   2. Verify the seat is at the canonical PDA `[b"seat", market, trader]`
//!      and is currently owned by Phoenix.
//!   3. CPI into the MagicBlock delegation program with the seat's PDA
//!      seeds.
//!
//! Account list:
//!   [0] authority           (signer, writable, payer)
//!   [1] system_program
//!   [2] market              (read-only)
//!   [3] seat                (writable; the Seat PDA to delegate)
//!   [4] trader              (read-only; used to derive seat seeds)
//!   [5] owner_program       (= Phoenix; readonly)
//!   [6] delegation_buffer   (writable)
//!   [7] delegation_record   (writable)
//!   [8] delegation_metadata (writable)
//!   [9] delegation_program

use borsh::{BorshDeserialize, BorshSerialize};
use sokoban::ZeroCopy;
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    program_error::ProgramError,
    pubkey::Pubkey,
};
use std::mem::size_of;

use ephemeral_rollups_sdk::{
    consts::DELEGATION_PROGRAM_ID,
    cpi::{delegate_account, DelegateAccounts, DelegateConfig},
};

use crate::program::{
    accounts::MarketHeader,
    error::assert_with_msg,
    validation::loaders::get_seat_address,
};

#[derive(BorshDeserialize, BorshSerialize)]
pub struct DelegateSeatParams {
    pub validator: Option<Pubkey>,
}

pub(crate) fn process_delegate_seat(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let DelegateSeatParams { validator } = DelegateSeatParams::try_from_slice(data)?;

    let account_iter = &mut accounts.iter();
    let authority_info = next_account_info(account_iter)?;
    let system_program_info = next_account_info(account_iter)?;
    let market_info = next_account_info(account_iter)?;
    let seat_info = next_account_info(account_iter)?;
    let trader_info = next_account_info(account_iter)?;
    let owner_program_info = next_account_info(account_iter)?;
    let delegation_buffer = next_account_info(account_iter)?;
    let delegation_record = next_account_info(account_iter)?;
    let delegation_metadata = next_account_info(account_iter)?;
    let delegation_program = next_account_info(account_iter)?;

    assert_with_msg(
        authority_info.is_signer,
        ProgramError::MissingRequiredSignature,
        "authority must sign DelegateSeat",
    )?;
    assert_with_msg(
        owner_program_info.key == &crate::ID,
        ProgramError::IncorrectProgramId,
        "owner_program must be Phoenix",
    )?;
    assert_with_msg(
        delegation_program.key == &DELEGATION_PROGRAM_ID,
        ProgramError::IncorrectProgramId,
        "delegation_program account must be DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh",
    )?;
    assert_with_msg(
        seat_info.owner == &crate::ID,
        ProgramError::IllegalOwner,
        "Seat must currently be owned by Phoenix (not already delegated)",
    )?;
    // Market is read-only here; the authority check is keyed off the
    // MarketHeader.authority field.
    assert_with_msg(
        market_info.owner == &crate::ID || market_info.owner == &DELEGATION_PROGRAM_ID,
        ProgramError::IllegalOwner,
        "Market must be owned by Phoenix or the delegation program",
    )?;

    // Verify caller is the market's authority.
    {
        let market_data = market_info.try_borrow_data()?;
        let header = MarketHeader::load_bytes(&market_data[..size_of::<MarketHeader>()])
            .ok_or(ProgramError::InvalidAccountData)?;
        assert_with_msg(
            &header.authority == authority_info.key,
            ProgramError::InvalidArgument,
            "Caller is not the market authority",
        )?;
    }

    // Verify seat is at the canonical PDA.
    let (expected_seat, seat_bump) = get_seat_address(market_info.key, trader_info.key);
    assert_with_msg(
        &expected_seat == seat_info.key,
        ProgramError::InvalidSeeds,
        "Seat PDA does not match (market, trader)",
    )?;
    let _ = seat_bump;

    let pda_seeds: &[&[u8]] = &[
        b"seat",
        market_info.key.as_ref(),
        trader_info.key.as_ref(),
    ];

    delegate_account(
        DelegateAccounts {
            payer: authority_info,
            pda: seat_info,
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
            validator,
        },
    )?;

    Ok(())
}
