//! OpenClaw Gateway adapter module.
//!
//! This module enables ahandd to connect to an OpenClaw Gateway as a node host,
//! providing command execution capabilities via the OpenClaw protocol.

pub mod client;
pub mod device_identity;
pub mod exec_approvals;
pub mod handler;
pub mod pairing;
pub mod protocol;

pub use client::OpenClawClient;
