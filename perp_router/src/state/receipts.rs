//! Receipt PDAs — transient state carriers for the 3-stage Magic Action
//! cross-layer flows.
//!
//! DepositReceipt:
//!   stage 1 (base):  user signs, SPL→vault, receipt created here, delegated
//!   stage 2 (ER):    auto-fired, credits TraderAccount, marks processed=1,
//!                     schedules close_deposit_receipt on base post-undelegate
//!   stage 3 (base):  receipt closed, rent refunded
//!
//! WithdrawalReceipt:
//!   stage 1 (ER):    user signs via session, haircut math runs, debits
//!                     TraderAccount, receipt holds *post-haircut* net_amount
//!   stage 2 (ER):    auto-fired, marks processed=1, schedules
//!                     execute_withdrawal_base_chain on base
//!   stage 3 (base):  validator-signed, vault PDA invoke_signed SPL→user,
//!                     receipt closed

use bytemuck::{Pod, Zeroable};
use solana_program::pubkey::Pubkey;

#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
pub struct DepositReceipt {
    pub trader: Pubkey,
    pub market: Pubkey,
    pub amount: u64,
    pub processed: u8,
    pub bump: u8,
    pub _pad: [u8; 6],
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
pub struct WithdrawalReceipt {
    pub trader: Pubkey,
    pub market: Pubkey,
    /// What the user asked for (pre-haircut, for audit).
    pub gross_amount: u64,
    /// What the user actually gets (post-haircut). Vault pays this.
    pub net_amount: u64,
    /// Haircut numerator (for audit; `h = num / den`).
    pub h_numerator: u64,
    /// Haircut denominator.
    pub h_denominator: u64,
    pub processed: u8,
    pub bump: u8,
    pub _pad: [u8; 6],
}
