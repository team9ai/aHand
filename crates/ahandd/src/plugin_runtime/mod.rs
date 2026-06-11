pub mod activation;
pub mod capability;

pub mod builtin;
pub mod host_resource;
pub mod manifest;
pub mod path_env;
pub mod provider;
pub mod registry;
pub mod resource;
pub mod runtime_dir;

pub use activation::{ActivationConfig, build_router, router_from_plugins};
pub use capability::{
    CapabilityEntry, CapabilityKind, CapabilityRemediation, CapabilityRouter, CapabilityUnavailable,
};
pub use host_resource::get_host_resource;
pub use manifest::PluginManifest;
pub use provider::{JobProvider, build_provider_registry};
pub use registry::PluginRegistry;
pub use resource::{
    HostResourceSnapshot, HostResourceValue, InstalledPluginResource, PluginStatus,
};
pub use runtime_dir::RuntimeDirs;
