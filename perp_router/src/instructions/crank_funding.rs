//! CrankFunding — ER, crank target (~every 1s).
//!
//! v1 minimal implementation: refreshes `PerpMarket.last_oracle_price` via
//! a client-supplied price clamped by the envelope. Funding accrual proper
//! (mark vs index gap × dt) ships in v1.1, alongside on-chain Pyth Pull
//! parsing — without those, F would drift on un-attested input.
//!
//! Account list:
//!   [0] caller        (signer; crank)
//!   [1] perp_market   (writable; delegated)
//!   [2] clock         (Sysvar)

use borsh::{BorshDeserialize, BorshSerialize};
use ephemeral_rollups_sdk::consts::DELEGATION_PROGRAM_ID;
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    clock::Clock,
    entrypoint::ProgramResult,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvar::Sysvar,
};
use std::mem::size_of;

use crate::{
    error::{assert_with_msg, PerpRouterError},
    risk::envelope::safe_oracle_read,
    state::PerpMarket,
    validation::loaders::find_perp_market_address,
};

#[derive(BorshSerialize, BorshDeserialize, Default)]
pub struct CrankFundingParams {
    /// Client-supplied mark price (envelope-clamped).
    pub mark_price: u64,
}

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let params = CrankFundingParams::try_from_slice(data).unwrap_or_default();
    if params.mark_price == 0 {
        return Ok(());
    }

    let it = &mut accounts.iter();
    let caller = next_account_info(it)?;
    let perp_market_info = next_account_info(it)?;

    assert_with_msg(
        caller.is_signer,
        ProgramError::MissingRequiredSignature,
        "caller must sign CrankFunding",
    )?;
    assert_with_msg(
        perp_market_info.owner == &crate::ID
            || perp_market_info.owner == &DELEGATION_PROGRAM_ID,
        ProgramError::IllegalOwner,
        "perp_market ownership mismatch",
    )?;

    let current_slot = Clock::get()?.slot;
    let mut buf = perp_market_info.try_borrow_mut_data()?;
    let m = bytemuck::from_bytes_mut::<PerpMarket>(&mut buf[..size_of::<PerpMarket>()]);
    let (expected, _) = find_perp_market_address(&m.phoenix_market, program_id);
    assert_with_msg(
        &expected == perp_market_info.key,
        PerpRouterError::InvalidPda,
        "perp_market PDA mismatch",
    )?;

    let dt = current_slot.saturating_sub(m.last_oracle_slot);
    let clamped =
        safe_oracle_read(params.mark_price, m.last_oracle_price, dt, m.max_bps_per_slot)?;
    m.last_oracle_price = clamped;
    m.last_oracle_slot = current_slot;
    Ok(())
}
