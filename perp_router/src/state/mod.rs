//! On-chain state accounts.
//!
//! All structs use `#[repr(C)]` + bytemuck Pod/Zeroable so they can be loaded
//! zero-copy via `sokoban::ZeroCopy`, matching Phoenix's account style.

pub mod global;
pub mod perp_market;
pub mod receipts;
pub mod trader_account;
pub mod vault;

pub use global::GlobalState;
pub use perp_market::PerpMarket;
pub use receipts::{DepositReceipt, WithdrawalReceipt};
pub use trader_account::{Position, ReserveEntry, TraderAccount};
