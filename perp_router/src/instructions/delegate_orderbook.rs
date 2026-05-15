//! DelegateOrderbook — base layer, admin.
//! Hands the per-market orderbook PDA to the MagicBlock delegation program
//! so the matching engine can mutate it on the Ephemeral Rollup.
//!
//! Mirrors `delegate_perp_market.rs` and `phoenix-v1/src/program/processor/
//! delegate_market.rs`. The seeds we sign with are
//! `[b"orderbook", perp_market]`, with the bump persisted on `PerpMarket`
//! by `InitializeOrderbook`.
//!
//! Account list:
//!   [0] admin              (signer, writable, payer)
//!   [1] system_program
//!   [2] perp_market        (readonly; supplies orderbook_bump + authority)
//!   [3] orderbook          (writable; the PDA being delegated)
//!   [4] owner_program      (= perp_router; readonly)
//!   [5] delegation_buffer  (writable)
//!   [6] delegation_record  (writable)
//!   [7] delegation_metadata(writable)
//!   [8] delegation_program

use borsh::{BorshDeserialize, BorshSerialize};
use ephemeral_rollups_sdk::{
    consts::DELEGATION_PROGRAM_ID,
    cpi::{delegate_account, DelegateAccounts, DelegateConfig},
};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    program_error::ProgramError,
    pubkey::Pubkey,
};
use std::mem::size_of;

use crate::{
    constants::ORDERBOOK_SEED,
    error::{assert_with_msg, PerpRouterError},
    state::PerpMarket,
    validation::loaders::{find_orderbook_address, find_perp_market_address},
};

#[derive(BorshDeserialize, BorshSerialize)]
pub struct DelegateOrderbookParams {
    pub validator: Option<Pubkey>,
}

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let DelegateOrderbookParams { validator } =
        DelegateOrderbookParams::try_from_slice(data)?;

    let it = &mut accounts.iter();
    let admin = next_account_info(it)?;
    let system_program_info = next_account_info(it)?;
    let perp_market_info = next_account_info(it)?;
    let orderbook_info = next_account_info(it)?;
    let owner_program = next_account_info(it)?;
    let delegation_buffer = next_account_info(it)?;
    let delegation_record = next_account_info(it)?;
    let delegation_metadata = next_account_info(it)?;
    let delegation_program = next_account_info(it)?;

    assert_with_msg(
        admin.is_signer,
        ProgramError::MissingRequiredSignature,
        "admin must sign DelegateOrderbook",
    )?;
    assert_with_msg(
        owner_program.key == &crate::ID,
        ProgramError::IncorrectProgramId,
        "owner_program must be perp_router",
    )?;
    assert_with_msg(
        delegation_program.key == &DELEGATION_PROGRAM_ID,
        ProgramError::IncorrectProgramId,
        "delegation_program account mismatch",
    )?;
    // perp_market is a reference (we read its data for the orderbook bump),
    // not the target of this delegation. Accept either still-on-base or
    // already-delegated — order of DelegatePerpMarket vs DelegateOrderbook
    // shouldn't matter.
    assert_with_msg(
        perp_market_info.owner == &crate::ID
            || perp_market_info.owner == &DELEGATION_PROGRAM_ID,
        ProgramError::IllegalOwner,
        "perp_market must be perp_router-owned or already delegated",
    )?;
    assert_with_msg(
        orderbook_info.owner == &crate::ID,
        ProgramError::IllegalOwner,
        "orderbook must currently be perp_router-owned (not already delegated)",
    )?;

    // Pull the stored authority + orderbook bump off perp_market.
    let (recorded_phoenix_market, recorded_authority, stored_orderbook_bump) = {
        let buf = perp_market_info.try_borrow_data()?;
        let m = bytemuck::from_bytes::<PerpMarket>(&buf[..size_of::<PerpMarket>()]);
        (m.phoenix_market, m.authority, m.orderbook_bump)
    };
    assert_with_msg(
        &recorded_authority == admin.key,
        PerpRouterError::InvalidAuthority,
        "Caller is not the recorded PerpMarket.authority",
    )?;
    // Bump of 0 means InitializeOrderbook hasn't run yet (or was on a
    // stale layout). Refuse to delegate — we couldn't sign for the PDA.
    assert_with_msg(
        stored_orderbook_bump != 0,
        ProgramError::InvalidAccountData,
        "Orderbook bump unset on PerpMarket — run InitializeOrderbook first",
    )?;

    // Verify the perp_market PDA itself (defence in depth — admin only,
    // but cheap).
    let (expected_perp_market, _) =
        find_perp_market_address(&recorded_phoenix_market, program_id);
    assert_with_msg(
        &expected_perp_market == perp_market_info.key,
        PerpRouterError::InvalidPda,
        "PerpMarket PDA mismatch",
    )?;

    // Verify the orderbook PDA and that the recorded bump matches.
    let (expected_orderbook, derived_bump) =
        find_orderbook_address(perp_market_info.key, program_id);
    assert_with_msg(
        &expected_orderbook == orderbook_info.key && derived_bump == stored_orderbook_bump,
        PerpRouterError::InvalidPda,
        "Orderbook PDA / bump mismatch",
    )?;

    let pda_seeds: &[&[u8]] = &[ORDERBOOK_SEED, perp_market_info.key.as_ref()];
    delegate_account(
        DelegateAccounts {
            payer: admin,
            pda: orderbook_info,
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
            validator,
        },
    )?;
    Ok(())
}
