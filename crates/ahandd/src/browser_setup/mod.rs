//! Browser automation setup: checks, installs, and browser detection.
//!
//! This module is designed to be reusable from both the `ahandd` CLI and
//! future Tauri-based frontends. All public types derive `Serialize` so they
//! can be emitted directly to a JavaScript frontend without transformation.
//!
//! The core API returns structured data; display concerns (terminal output,
//! GUI rendering) live in adapter modules (`crate::cli::browser_doctor`,
//! `crate::cli::browser_init`).

pub mod types;

pub use types::*;
