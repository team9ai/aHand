pub mod ahand_client;
pub mod app_tool_registry;
pub mod approval;
pub mod browser;
pub mod browser_setup;
pub mod config;
pub mod device_identity;
pub mod executor;
pub mod file_manager;
pub mod outbox;
pub mod plugin_runtime;
pub mod registry;
pub mod session;
pub mod store;
pub mod updater;

mod public_api;
pub use device_identity::DeviceIdentity;
pub use public_api::{
    AppToolDef, AppToolError, AppToolHandler, DaemonConfig, DaemonConfigBuilder, DaemonHandle,
    DaemonStatus, ErrorKind, SessionMode, load_or_create_identity, spawn,
};
