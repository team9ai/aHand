//! CLI adapters that format browser_setup output for terminal display.
//!
//! The core logic lives in `crate::browser_setup`. These modules add
//! presentation — formatting, colors, progress bars, exit codes.

pub mod browser_doctor;
pub mod browser_init;
