//! DelegateMarket — base-layer instruction that hands market account
//! ownership to the MagicBlock delegation program so the market can be
//! mutated on an Ephemeral Rollup.
//!
//! Flow:
//!   1. Verify caller is the market's authority.
//!   2. Read market PDA seeds (base_mint, quote_mint, market_creator) from
//!      the market header so we can sign with the canonical seeds.
//!   3. CPI into the MagicBlock delegation program (vendored helpers in
//!      `crate::magicblock`).
//!
//! Account list:
//!   [0] authority           (signer, writable, payer)
//!   [1] system_program
//!   [2] market              (writable; must be a PDA, market_bump != 0)
//!   [3] owner_program       (= Phoenix; readonly)
//!   [4] delegation_buffer   (writable)
//!   [5] delegation_record   (writable)
//!   [6] delegation_metadata (writable)
//!   [7] delegation_program

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
    accounts::MarketHeader, error::assert_with_msg, processor::initialize::find_market_address,
};

#[derive(BorshDeserialize, BorshSerialize)]
pub struct DelegateMarketParams {
    /// Validator pinning. `None` = any validator on the network the client
    /// connects to. `Some(pk)` = only that validator may process txs against
    /// this account (for self-hosted / paid dedicated validators).
    pub validator: Option<Pubkey>,
}

pub(crate) fn process_delegate_market(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let DelegateMarketParams { validator } = DelegateMarketParams::try_from_slice(data)?;

    let account_iter = &mut accounts.iter();
    let authority_info = next_account_info(account_iter)?;
    let system_program_info = next_account_info(account_iter)?;
    let market_info = next_account_info(account_iter)?;
    let owner_program_info = next_account_info(account_iter)?;
    let delegation_buffer = next_account_info(account_iter)?;
    let delegation_record = next_account_info(account_iter)?;
    let delegation_metadata = next_account_info(account_iter)?;
    let delegation_program = next_account_info(account_iter)?;

    assert_with_msg(
        authority_info.is_signer,
        ProgramError::MissingRequiredSignature,
        "authority must sign DelegateMarket",
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
        market_info.owner == &crate::ID,
        ProgramError::IllegalOwner,
        "Market must currently be owned by Phoenix (not already delegated)",
    )?;

    // Pull the (base_mint, quote_mint, authority, market_bump) from the
    // market header so we can verify the PDA seeds match.
    let (base_mint, quote_mint, market_authority, market_bump) = {
        let data = market_info.try_borrow_data()?;
        let header = MarketHeader::load_bytes(&data[..size_of::<MarketHeader>()])
            .ok_or(ProgramError::InvalidAccountData)?;
        (
            header.base_params.mint_key,
            header.quote_params.mint_key,
            header.authority,
            header.market_bump,
        )
    };

    assert_with_msg(
        market_authority == *authority_info.key,
        ProgramError::InvalidArgument,
        "Caller is not the market authority",
    )?;
    // market_bump == 0 means the market was created via the legacy keypair
    // flow and is not a PDA — refuse to delegate (we wouldn't be able to
    // sign for it).
    assert_with_msg(
        market_bump != 0,
        ProgramError::InvalidAccountData,
        "Market was not allocated as a PDA (market_bump = 0); cannot delegate",
    )?;

    // Verify the recorded bump actually produces this market's pubkey.
    let (expected_market, derived_bump) =
        find_market_address(&base_mint, &quote_mint, authority_info.key, program_id);
    assert_with_msg(
        &expected_market == market_info.key && derived_bump == market_bump,
        ProgramError::InvalidSeeds,
        "Market PDA seeds do not match (base_mint, quote_mint, authority, bump)",
    )?;

    let pda_seeds: &[&[u8]] = &[
        super::initialize::MARKET_SEED_PREFIX,
        base_mint.as_ref(),
        quote_mint.as_ref(),
        authority_info.key.as_ref(),
    ];

    delegate_account(
        DelegateAccounts {
            payer: authority_info,
            pda: market_info,
            owner_program: owner_program_info,
            buffer: delegation_buffer,
            delegation_record,
            delegation_metadata,
            delegation_program,
            system_program: system_program_info,
        },
        pda_seeds,
        DelegateConfig {
            // Manual / crank-driven commits; no auto-commit on the validator.
            commit_frequency_ms: u32::MAX,
            validator,
        },
    )?;

    Ok(())
}
