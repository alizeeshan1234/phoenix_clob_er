//! InitializeMarket — base layer, admin.
//!
//! Allocates a PerpMarket PDA bound to an existing Phoenix CLOB market.
//! Caches the Phoenix market's base/quote mint and vault keys so subsequent
//! `OpenPosition` / `ClosePosition` CPIs don't re-derive them.
//!
//! Account list:
//!   [0] admin                  (signer, writable, payer)
//!   [1] system_program
//!   [2] perp_market            (writable, system-owned uninit)
//!   [3] phoenix_market         (readonly, the sibling Phoenix CLOB market)
//!   [4] base_mint              (readonly)
//!   [5] quote_mint             (readonly)
//!   [6] phoenix_base_vault     (readonly; Phoenix's `[b"vault", market, base_mint]`)
//!   [7] phoenix_quote_vault    (readonly; Phoenix's `[b"vault", market, quote_mint]`)
//!   [8] oracle                 (readonly, Pyth feed)

use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    program_error::ProgramError,
    pubkey::Pubkey,
    rent::Rent,
    system_program,
    sysvar::Sysvar,
};
use std::mem::size_of;

use crate::{
    constants::{MAX_BPS_PER_SLOT, MAX_LEVERAGE_BPS, PERP_MARKET_SEED},
    error::{assert_with_msg, PerpRouterError},
    state::PerpMarket,
    system_utils::create_account,
    validation::loaders::find_perp_market_address,
};

#[derive(BorshSerialize, BorshDeserialize)]
pub struct InitializeMarketParams {
    pub max_bps_per_slot: u32,
    pub max_leverage_bps: u32,
}

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let params = InitializeMarketParams::try_from_slice(data)?;
    let it = &mut accounts.iter();
    let admin = next_account_info(it)?;
    let system_program_info = next_account_info(it)?;
    let perp_market_info = next_account_info(it)?;
    let phoenix_market_info = next_account_info(it)?;
    let base_mint = next_account_info(it)?;
    let quote_mint = next_account_info(it)?;
    let phoenix_base_vault = next_account_info(it)?;
    let phoenix_quote_vault = next_account_info(it)?;
    let oracle_info = next_account_info(it)?;

    assert_with_msg(
        admin.is_signer,
        ProgramError::MissingRequiredSignature,
        "admin must sign InitializeMarket",
    )?;
    assert_with_msg(
        system_program_info.key == &system_program::ID,
        ProgramError::IncorrectProgramId,
        "system_program mismatch",
    )?;

    let (expected, bump) = find_perp_market_address(phoenix_market_info.key, program_id);
    assert_with_msg(
        perp_market_info.key == &expected,
        PerpRouterError::InvalidPda,
        "perp_market PDA mismatch",
    )?;
    assert_with_msg(
        perp_market_info.owner == &system_program::ID && perp_market_info.data_is_empty(),
        PerpRouterError::AlreadyInitialized,
        "PerpMarket already initialised",
    )?;

    let rent = Rent::get()?;
    let seeds: Vec<Vec<u8>> = vec![
        PERP_MARKET_SEED.to_vec(),
        phoenix_market_info.key.as_ref().to_vec(),
        vec![bump],
    ];
    create_account(
        admin,
        perp_market_info,
        system_program_info,
        program_id,
        &rent,
        size_of::<PerpMarket>() as u64,
        seeds,
    )?;

    let mut buf = perp_market_info.try_borrow_mut_data()?;
    let m = bytemuck::from_bytes_mut::<PerpMarket>(&mut buf[..size_of::<PerpMarket>()]);
    *m = PerpMarket::default();
    m.phoenix_market = *phoenix_market_info.key;
    m.oracle = *oracle_info.key;
    m.base_mint = *base_mint.key;
    m.quote_mint = *quote_mint.key;
    m.phoenix_base_vault = *phoenix_base_vault.key;
    m.phoenix_quote_vault = *phoenix_quote_vault.key;
    m.max_bps_per_slot = if params.max_bps_per_slot == 0 {
        MAX_BPS_PER_SLOT
    } else {
        params.max_bps_per_slot
    };
    m.max_leverage_bps = if params.max_leverage_bps == 0 {
        MAX_LEVERAGE_BPS
    } else {
        params.max_leverage_bps
    };
    m.authority = *admin.key;
    m.bump = bump;
    Ok(())
}
