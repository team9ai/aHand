//! Platform abstraction layer for aHand client binaries (`ahandd`, `ahandctl`).
//!
//! All OS-conditional behavior lives here so the rest of the codebase stays
//! `#[cfg]`-free. Each module documents the Unix and Windows semantics it
//! guarantees; anything it cannot make equivalent is documented at the call
//! site it serves.

pub mod paths;
pub mod process;
pub mod signals;
