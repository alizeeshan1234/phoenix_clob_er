//! Percolator risk engine port.
//!
//! Reference: <https://github.com/aeyakovenko/percolator> (spec v12.20.6,
//! Apache-2.0). The five invariants are implemented as pure functions over
//! plain integers + fixed-point i128 so they can be Kani-verified in
//! isolation from Solana account I/O.
//!
//! Modules:
//!   * `haircut`     — Invariant H (withdrawal scaling)
//!   * `side_index`  — A/K/F/B lazy side indices (replaces ADL)
//!   * `envelope`    — per-slot oracle price clamp
//!   * `warmup`      — PnL reserve → matured aging
//!   * `recovery`    — Normal → DrainOnly → ResetPending state machine

pub mod envelope;
pub mod haircut;
pub mod recovery;
pub mod side_index;
pub mod warmup;
