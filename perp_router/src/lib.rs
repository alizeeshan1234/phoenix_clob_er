//! perp_router — Perpetual futures router on Phoenix CLOB.
//!
//! Architecture:
//!   * Phoenix v1 (sibling crate) is the spot CLOB matching engine ("slab").
//!   * perp_router holds collateral, tracks synthetic positions, marks PnL
//!     against an oracle, and runs the Percolator risk engine.
//!   * MagicBlock Ephemeral Rollup hosts the hot path (open/close/liquidate).
//!     Cross-layer flows mirror Phoenix's existing Magic-Action patterns.
//!
//! Percolator invariants live in `risk/`:
//!   * H  (haircut)         — withdrawals scale by Residual / total_PnL
//!   * A/K/F/B              — lazy side indices replace ADL
//!   * Envelope             — per-slot oracle price clamp
//!   * Warmup               — fresh PnL ages before becoming withdrawable
//!   * Recovery             — Normal → DrainOnly → ResetPending → Normal

#![allow(clippy::result_large_err)]

pub mod constants;
pub mod cpi;
pub mod error;
pub mod instructions;
pub mod risk;
pub mod state;
pub mod system_utils;
pub mod validation;

use borsh::{BorshDeserialize, BorshSerialize};
use num_enum::TryFromPrimitive;
use solana_program::{
    account_info::AccountInfo, declare_id, entrypoint::ProgramResult,
    program_error::ProgramError, pubkey::Pubkey,
};

#[cfg(not(feature = "no-entrypoint"))]
use solana_security_txt::security_txt;

#[cfg(not(feature = "no-entrypoint"))]
security_txt! {
    name: "perp_router",
    project_url: "",
    contacts: "",
    policy: "",
    preferred_languages: "en",
    source_code: "",
    auditors: "unaudited"
}

// Placeholder program id — replace before deploy.
declare_id!("CYP9hmL492qupiKScaYspTjdhCfK9Xwb7uckChR9Wc2x");

/// All instructions handled by perp_router. Discriminant is the first byte of
/// the instruction data. Order matches the 19-instruction table in the plan.
#[derive(BorshSerialize, BorshDeserialize, TryFromPrimitive, Copy, Clone, Debug)]
#[repr(u8)]
pub enum PerpRouterInstruction {
    // --- Initialization (base layer) ---
    InitializeMarket = 0,
    InitializeTrader = 1,

    // --- Delegation (base ⇄ ER) ---
    DelegateTraderAccount = 2,
    DelegateGlobalState = 3,
    DelegatePerpMarket = 4,
    UndelegateTraderAccount = 5,

    // --- Collateral deposit chain (3 stages, 1 user signature) ---
    RequestCollateralDeposit = 6,
    ProcessCollateralDepositEr = 7,
    CloseCollateralDepositReceipt = 8,

    // --- Collateral withdrawal chain (3 stages, haircut applied on ER) ---
    RequestCollateralWithdrawal = 9,
    ProcessCollateralWithdrawalEr = 10,
    ExecuteCollateralWithdrawalBaseChain = 11,

    // --- Trading (ER) ---
    OpenPosition = 12,
    ClosePosition = 13,

    // --- Risk operations (ER) ---
    Liquidate = 14,
    MaturePnl = 15,
    CrankFunding = 16,
    RecoveryCheck = 17,

    // --- Crank scheduling (ER, admin once) ---
    ScheduleCranks = 18,

    // --- Initialization (admin, base layer) ---
    InitializeGlobalState = 19,

    // --- Direct (no ER) deposit/withdraw, single tx, used for local /
    // CI testing and as a fallback when MagicBlock auto-fire isn't
    // configured for the program. Same Percolator semantics (haircut on
    // withdraw), but no delegation, no Magic Action chain, one signature.
    DirectDeposit = 20,
    DirectWithdraw = 21,

    // --- Direct (oracle-priced, no Phoenix CPI) open/close. Runs on ER
    // when accounts are delegated; on base otherwise. Used for v1.1 ER
    // trading demos before a real Phoenix market exists on the ER. ---
    DirectOpenPosition = 22,
    DirectClosePosition = 23,
}

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process_instruction);

// Discriminator the MagicBlock delegation program CPIs with when
// undelegating an account back to its owner. The owner program is
// expected to re-create the PDA and copy the buffered state back. See
// `magicblock-delegation-program::consts::EXTERNAL_UNDELEGATE_DISCRIMINATOR`.
const EXTERNAL_UNDELEGATE_DISCRIMINATOR: [u8; 8] = [196, 28, 41, 206, 48, 37, 51, 167];

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    if instruction_data.len() >= 8
        && &instruction_data[..8] == EXTERNAL_UNDELEGATE_DISCRIMINATOR
    {
        return process_external_undelegate(accounts, &instruction_data[8..]);
    }
    let (tag, data) = instruction_data
        .split_first()
        .ok_or(ProgramError::InvalidInstructionData)?;
    let ix = PerpRouterInstruction::try_from(*tag)
        .map_err(|_| ProgramError::InvalidInstructionData)?;

    match ix {
        PerpRouterInstruction::InitializeMarket => {
            instructions::initialize_market::process(program_id, accounts, data)
        }
        PerpRouterInstruction::InitializeTrader => {
            instructions::initialize_trader::process(program_id, accounts, data)
        }
        PerpRouterInstruction::DelegateTraderAccount => {
            instructions::delegate_trader_account::process(program_id, accounts, data)
        }
        PerpRouterInstruction::DelegateGlobalState => {
            instructions::delegate_global_state::process(program_id, accounts, data)
        }
        PerpRouterInstruction::DelegatePerpMarket => {
            instructions::delegate_perp_market::process(program_id, accounts, data)
        }
        PerpRouterInstruction::UndelegateTraderAccount => {
            instructions::undelegate_trader_account::process(program_id, accounts, data)
        }
        PerpRouterInstruction::RequestCollateralDeposit => {
            instructions::request_collateral_deposit::process(program_id, accounts, data)
        }
        PerpRouterInstruction::ProcessCollateralDepositEr => {
            instructions::process_collateral_deposit_er::process(program_id, accounts, data)
        }
        PerpRouterInstruction::CloseCollateralDepositReceipt => {
            instructions::close_collateral_deposit_receipt::process(program_id, accounts, data)
        }
        PerpRouterInstruction::RequestCollateralWithdrawal => {
            instructions::request_collateral_withdrawal::process(program_id, accounts, data)
        }
        PerpRouterInstruction::ProcessCollateralWithdrawalEr => {
            instructions::process_collateral_withdrawal_er::process(program_id, accounts, data)
        }
        PerpRouterInstruction::ExecuteCollateralWithdrawalBaseChain => {
            instructions::execute_collateral_withdrawal_base_chain::process(
                program_id, accounts, data,
            )
        }
        PerpRouterInstruction::OpenPosition => {
            instructions::open_position::process(program_id, accounts, data)
        }
        PerpRouterInstruction::ClosePosition => {
            instructions::close_position::process(program_id, accounts, data)
        }
        PerpRouterInstruction::Liquidate => {
            instructions::liquidate::process(program_id, accounts, data)
        }
        PerpRouterInstruction::MaturePnl => {
            instructions::mature_pnl::process(program_id, accounts, data)
        }
        PerpRouterInstruction::CrankFunding => {
            instructions::crank_funding::process(program_id, accounts, data)
        }
        PerpRouterInstruction::RecoveryCheck => {
            instructions::recovery_check::process(program_id, accounts, data)
        }
        PerpRouterInstruction::ScheduleCranks => {
            instructions::schedule_cranks::process(program_id, accounts, data)
        }
        PerpRouterInstruction::InitializeGlobalState => {
            instructions::initialize_global_state::process(program_id, accounts, data)
        }
        PerpRouterInstruction::DirectDeposit => {
            instructions::direct_deposit::process(program_id, accounts, data)
        }
        PerpRouterInstruction::DirectWithdraw => {
            instructions::direct_withdraw::process(program_id, accounts, data)
        }
        PerpRouterInstruction::DirectOpenPosition => {
            instructions::direct_open_position::process(program_id, accounts, data)
        }
        PerpRouterInstruction::DirectClosePosition => {
            instructions::direct_close_position::process(program_id, accounts, data)
        }
    }
}

/// Handle the post-commit CPI from the MagicBlock delegation program.
/// Accounts (per `magicblock-delegation-program` v1.1):
///   [0] delegated_account (writable)
///   [1] undelegate_buffer (writable, signer)
///   [2] payer             (writable, signer)
///   [3] system_program
/// Data after the 8-byte discriminator is borsh-serialized `Vec<Vec<u8>>`
/// of the PDA's signer seeds (excluding the bump).
fn process_external_undelegate(accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    use solana_program::account_info::next_account_info;
    let seeds: Vec<Vec<u8>> = borsh::BorshDeserialize::try_from_slice(data)
        .map_err(|_| ProgramError::InvalidInstructionData)?;
    let it = &mut accounts.iter();
    let delegated_account = next_account_info(it)?;
    let buffer = next_account_info(it)?;
    let payer = next_account_info(it)?;
    let system_program = next_account_info(it)?;
    ephemeral_rollups_sdk::cpi::undelegate_account(
        delegated_account,
        &crate::ID,
        buffer,
        payer,
        system_program,
        seeds,
    )
}
