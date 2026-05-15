//! Instruction processors. One file per instruction; each exposes
//! `pub fn process(program_id, accounts, data) -> ProgramResult`.

pub mod claim_seat;
pub mod close_collateral_deposit_receipt;
pub mod close_position;
pub mod crank_funding;
pub mod delegate_global_state;
pub mod delegate_orderbook;
pub mod delegate_perp_market;
pub mod delegate_trader_account;
pub mod direct_close_position;
pub mod direct_deposit;
pub mod direct_open_position;
pub mod direct_withdraw;
pub mod execute_collateral_withdrawal_base_chain;
pub mod initialize_global_state;
pub mod initialize_market;
pub mod initialize_orderbook;
pub mod initialize_trader;
pub mod liquidate;
pub mod mature_pnl;
pub mod open_position;
pub mod place_order_perp;
pub mod process_collateral_deposit_er;
pub mod process_collateral_withdrawal_er;
pub mod recovery_check;
pub mod request_collateral_deposit;
pub mod request_collateral_withdrawal;
pub mod schedule_cranks;
pub mod undelegate_trader_account;
