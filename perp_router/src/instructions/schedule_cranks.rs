//! ScheduleCranks — ER, admin one-time.
//!
//! Schedules three recurring tasks via MagicBlock's magic program:
//!   Task 1: CrankFunding   every 1000 ms
//!   Task 2: MaturePnl      every  400 ms
//!   Task 3: RecoveryCheck  every 5000 ms
//!
//! Each task is bincode-serialized as `MagicBlockInstruction::ScheduleTask`
//! and invoked against the magic program. Cranked ix uses the SAME
//! delegated `global_state` (and optionally `trader_account` / `perp_market`
//! depending on the cranked target) supplied here.
//!
//! Account list:
//!   [0] admin         (signer, writable, payer)
//!   [1] global_state  (writable; delegated)
//!   [2] magic_program (Magic111...)

use borsh::{BorshDeserialize, BorshSerialize};
use ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID;
use magicblock_magic_program_api::{
    args::ScheduleTaskArgs, instruction::MagicBlockInstruction,
};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    instruction::{AccountMeta, Instruction},
    program::invoke_signed,
    program_error::ProgramError,
    pubkey::Pubkey,
};

use crate::{
    error::{assert_with_msg, PerpRouterError},
    PerpRouterInstruction,
};

#[derive(BorshSerialize, BorshDeserialize, Default)]
pub struct ScheduleCranksParams {
    pub funding_ms: u64,
    pub mature_pnl_ms: u64,
    pub recovery_check_ms: u64,
    /// 0 → forever (`i64::MAX`).
    pub iterations: u64,
}

pub fn process(_program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let params = ScheduleCranksParams::try_from_slice(data).unwrap_or_default();
    let funding_ms = if params.funding_ms == 0 { 1_000 } else { params.funding_ms } as i64;
    let mature_ms = if params.mature_pnl_ms == 0 { 400 } else { params.mature_pnl_ms } as i64;
    let recovery_ms = if params.recovery_check_ms == 0 { 5_000 } else { params.recovery_check_ms } as i64;
    let iters = if params.iterations == 0 { i64::MAX } else { params.iterations as i64 };

    let it = &mut accounts.iter();
    let admin = next_account_info(it)?;
    let global_state_info = next_account_info(it)?;
    let magic_program = next_account_info(it)?;

    assert_with_msg(
        admin.is_signer,
        ProgramError::MissingRequiredSignature,
        "admin must sign ScheduleCranks",
    )?;
    assert_with_msg(
        magic_program.key == &MAGIC_PROGRAM_ID,
        ProgramError::IncorrectProgramId,
        "magic_program account mismatch",
    )?;

    let schedule = |task_id: i64, interval_ms: i64, ix_tag: PerpRouterInstruction|
        -> Result<(), ProgramError>
    {
        let cranked = Instruction {
            program_id: crate::ID,
            accounts: vec![
                AccountMeta::new(*admin.key, true),
                AccountMeta::new(*global_state_info.key, false),
            ],
            data: vec![ix_tag as u8],
        };
        let ix_data = bincode::serialize(&MagicBlockInstruction::ScheduleTask(
            ScheduleTaskArgs {
                task_id,
                execution_interval_millis: interval_ms,
                iterations: iters,
                instructions: vec![cranked],
            },
        ))
        .map_err(|_| PerpRouterError::MathOverflow)?;

        let schedule_ix = Instruction::new_with_bytes(
            *magic_program.key,
            &ix_data,
            vec![
                AccountMeta::new(*admin.key, true),
                AccountMeta::new(*global_state_info.key, false),
            ],
        );
        invoke_signed(
            &schedule_ix,
            &[admin.clone(), global_state_info.clone(), magic_program.clone()],
            &[],
        )
    };

    schedule(1, funding_ms, PerpRouterInstruction::CrankFunding)?;
    schedule(2, mature_ms, PerpRouterInstruction::MaturePnl)?;
    schedule(3, recovery_ms, PerpRouterInstruction::RecoveryCheck)?;
    Ok(())
}
