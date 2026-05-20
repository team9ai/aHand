# AHand Plugin Runtime Stage 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Route existing aHand `Envelope` capabilities through plugin activation gates before executing `JobRequest`, `FileRequest`, and `BrowserRequest`.

**Architecture:** Add a small capability router under `crates/ahandd/src/plugin_runtime/` that maps protocol capabilities to first-party plugin ids and renders host-neutral remediation messages. Build active capability state from Stage 1 plugin inspection plus daemon host configuration, then call that gate from cloud WebSocket and debug IPC dispatch before existing handlers execute.

**Tech Stack:** Rust 2024, tokio, serde, ahand protobuf types, existing `plugin_runtime`, `browser_setup`, `BrowserManager`, `FileManager`, `ahand_client`, and debug IPC modules.

---

## Scope Check

This plan implements only Stage 2 from `docs/superpowers/specs/2026-05-14-ahand-plugin-runtime-stage2-design.md`.

Included:

- `JobRequest` capability gate through `shell`.
- `FileRequest` capability gate through `file`.
- `BrowserRequest` capability gate through `browser-playwright-cli`.
- Cloud WebSocket dispatch gate.
- Debug IPC gate for the payloads IPC already supports: `JobRequest` and `BrowserRequest`.
- Hello capability advertisement derived from active plugin capability state.
- Host-neutral remediation strings.

Excluded:

- OpenClaw `system.run`, `browser.proxy`, or command registry.
- Third-party plugin packages.
- Automatic install during dispatch.
- Rewriting protobuf schemas.
- Mapping `JobRequest.tool = "node"` or `"python"` to managed runtime binaries.

## File Structure

### Created

| File | Responsibility |
|------|----------------|
| `crates/ahandd/src/plugin_runtime/capability.rs` | Capability ids, router state, remediation model, rendered protocol messages, and pure unit tests. |
| `crates/ahandd/src/plugin_runtime/activation.rs` | Builds current capability state from Stage 1 plugin resources plus `BrowserManager` / `FileManager` host config. |

### Modified

| File | Change |
|------|--------|
| `crates/ahandd/src/plugin_runtime/mod.rs` | Export `capability` and `activation` modules and public router types. |
| `crates/ahandd/src/browser.rs` | Add a public read-only helper to tell whether a system browser is available/configured. |
| `crates/ahandd/src/ahand_client.rs` | Build a capability runtime, derive Hello capabilities from it, and gate `JobRequest`, `FileRequest`, and `BrowserRequest`. |
| `crates/ahandd/src/ipc.rs` | Gate debug IPC `JobRequest` and `BrowserRequest` using the same capability runtime. |
| `crates/ahandd/src/main.rs` | Pass `FileManager` into `ipc::serve_ipc` if needed for shared capability runtime construction. |
| `crates/ahandd/src/public_api.rs` | No behavior change expected, but may need signature updates if `ahand_client::run_with_reporter` takes a capability runtime. |
| `crates/ahandd/tests/hub_handshake.rs` | Update or add Hello capability tests if helper signatures change. |
| `crates/ahandd/tests/hello_signature.rs` | Update or preserve compatibility wrapper for existing signature tests. |

## Design Notes For Implementers

Use a fresh capability check for each request, not a stale startup-only snapshot. That lets a crate host install `browser-playwright-cli` after receiving an unavailable error and retry without restarting the daemon.

Keep concrete execution unchanged after the gate passes. The router is not a replacement for session mode, approvals, file policy, browser domain policy, cancellation, PTY, or idempotency.

When a managed plugin is missing, the message must not hard-code `ahandd plugin install ...`. Use host-neutral wording:

```text
install plugin browser-playwright-cli through the host plugin installer
```

`shell` and `file` are built-in host plugins. Their failures should use host environment/configuration remediation, not plugin install remediation.

---

### Task 1: Add Pure Capability Router Types

**Files:**
- Create: `crates/ahandd/src/plugin_runtime/capability.rs`
- Modify: `crates/ahandd/src/plugin_runtime/mod.rs`
- Test: module tests inside `capability.rs`

- [ ] **Step 1: Write failing tests for capability rendering and active wires**

Create `crates/ahandd/src/plugin_runtime/capability.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_remediation_renders_host_neutral_message() {
        let unavailable = CapabilityUnavailable {
            capability: CapabilityKind::Browser,
            plugin_id: "browser-playwright-cli".to_string(),
            status: PluginStatus::Blocked,
            reason: "dependency node is missing".to_string(),
            remediation: CapabilityRemediation::InstallPlugin {
                plugin_id: "browser-playwright-cli".to_string(),
            },
        };

        assert_eq!(
            unavailable.to_protocol_message(),
            "browser capability unavailable: plugin browser-playwright-cli is blocked because dependency node is missing; install plugin browser-playwright-cli through the host plugin installer"
        );
    }

    #[test]
    fn builtin_file_disabled_renders_configuration_message() {
        let unavailable = CapabilityUnavailable {
            capability: CapabilityKind::File,
            plugin_id: "file".to_string(),
            status: PluginStatus::Blocked,
            reason: "host configuration disabled file operations".to_string(),
            remediation: CapabilityRemediation::HostConfiguration {
                message: "enable file operations in host configuration".to_string(),
            },
        };

        assert_eq!(
            unavailable.to_protocol_message(),
            "file capability unavailable: host configuration disabled file operations; enable file operations in host configuration"
        );
    }

    #[test]
    fn active_wire_capabilities_use_existing_protocol_names() {
        let router = CapabilityRouter::new(vec![
            CapabilityEntry::active(CapabilityKind::Exec, "shell"),
            CapabilityEntry::active(CapabilityKind::File, "file"),
            CapabilityEntry::active(CapabilityKind::Browser, "browser-playwright-cli"),
        ]);

        assert_eq!(
            router.active_wire_capabilities(),
            vec!["exec", "file", "browser-playwright-cli"]
        );
    }

    #[test]
    fn ensure_returns_unavailable_for_inactive_capability() {
        let unavailable = CapabilityUnavailable {
            capability: CapabilityKind::Exec,
            plugin_id: "shell".to_string(),
            status: PluginStatus::Missing,
            reason: "host shell unavailable".to_string(),
            remediation: CapabilityRemediation::HostEnvironment {
                message: "configure a valid host shell".to_string(),
            },
        };
        let router = CapabilityRouter::new(vec![CapabilityEntry::unavailable(unavailable.clone())]);

        assert_eq!(
            router.ensure(CapabilityKind::Exec).unwrap_err(),
            unavailable
        );
    }
}
```

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
cargo test -p ahandd plugin_runtime::capability
```

Expected: compile failure because `CapabilityKind`, `CapabilityRouter`, and related types do not exist.

- [ ] **Step 3: Implement the minimal capability model**

Add the implementation above the tests in `capability.rs`:

```rust
use std::collections::BTreeMap;

use super::PluginStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CapabilityKind {
    Exec,
    File,
    Browser,
}

impl CapabilityKind {
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Exec => "exec",
            Self::File => "file",
            Self::Browser => "browser-playwright-cli",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Exec => "exec",
            Self::File => "file",
            Self::Browser => "browser",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityRemediation {
    None,
    HostConfiguration { message: String },
    HostEnvironment { message: String },
    InstallPlugin { plugin_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityUnavailable {
    pub capability: CapabilityKind,
    pub plugin_id: String,
    pub status: PluginStatus,
    pub reason: String,
    pub remediation: CapabilityRemediation,
}

impl CapabilityUnavailable {
    pub fn to_protocol_message(&self) -> String {
        match &self.remediation {
            CapabilityRemediation::InstallPlugin { plugin_id } => {
                format!(
                    "{} capability unavailable: plugin {} is {} because {}; install plugin {} through the host plugin installer",
                    self.capability.display_name(),
                    self.plugin_id,
                    plugin_status_word(self.status),
                    self.reason,
                    plugin_id
                )
            }
            CapabilityRemediation::HostConfiguration { message: hint }
            | CapabilityRemediation::HostEnvironment { message: hint } => {
                let mut message = format!(
                    "{} capability unavailable: {}",
                    self.capability.display_name(),
                    self.reason
                );
                message.push_str("; ");
                message.push_str(hint);
                message
            }
            CapabilityRemediation::None => {
                format!(
                    "{} capability unavailable: {}",
                    self.capability.display_name(),
                    self.reason
                )
            }
        }
    }
}

fn plugin_status_word(status: PluginStatus) -> &'static str {
    match status {
        PluginStatus::Installed => "installed",
        PluginStatus::Missing => "missing",
        PluginStatus::Outdated => "outdated",
        PluginStatus::Failed => "failed",
        PluginStatus::Blocked => "blocked",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityState {
    Active,
    Unavailable(CapabilityUnavailable),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityEntry {
    pub capability: CapabilityKind,
    pub owner_plugin_id: String,
    pub state: CapabilityState,
}

impl CapabilityEntry {
    pub fn active(capability: CapabilityKind, owner_plugin_id: impl Into<String>) -> Self {
        Self {
            capability,
            owner_plugin_id: owner_plugin_id.into(),
            state: CapabilityState::Active,
        }
    }

    pub fn unavailable(unavailable: CapabilityUnavailable) -> Self {
        Self {
            capability: unavailable.capability,
            owner_plugin_id: unavailable.plugin_id.clone(),
            state: CapabilityState::Unavailable(unavailable),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityRouter {
    entries: BTreeMap<CapabilityKind, CapabilityEntry>,
}

impl CapabilityRouter {
    pub fn new(entries: Vec<CapabilityEntry>) -> Self {
        Self {
            entries: entries
                .into_iter()
                .map(|entry| (entry.capability, entry))
                .collect(),
        }
    }

    pub fn ensure(&self, capability: CapabilityKind) -> Result<(), CapabilityUnavailable> {
        match self.entries.get(&capability).map(|entry| &entry.state) {
            Some(CapabilityState::Active) => Ok(()),
            Some(CapabilityState::Unavailable(unavailable)) => Err(unavailable.clone()),
            None => Err(CapabilityUnavailable {
                capability,
                plugin_id: capability.wire_name().to_string(),
                status: PluginStatus::Missing,
                reason: "capability is not registered".to_string(),
                remediation: CapabilityRemediation::None,
            }),
        }
    }

    pub fn active_wire_capabilities(&self) -> Vec<&'static str> {
        [CapabilityKind::Exec, CapabilityKind::File, CapabilityKind::Browser]
            .into_iter()
            .filter(|capability| self.ensure(*capability).is_ok())
            .map(CapabilityKind::wire_name)
            .collect()
    }
}
```

Modify `crates/ahandd/src/plugin_runtime/mod.rs`:

```rust
pub mod activation;
pub mod builtin;
pub mod capability;
pub mod host_resource;
pub mod manifest;
pub mod registry;
pub mod resource;
pub mod runtime_dir;

pub use capability::{
    CapabilityEntry, CapabilityKind, CapabilityRemediation, CapabilityRouter, CapabilityState,
    CapabilityUnavailable,
};
```

Keep the existing exports in `mod.rs`.

- [ ] **Step 4: Run tests and verify GREEN**

Run:

```bash
cargo test -p ahandd plugin_runtime::capability
```

Expected: capability tests pass.

- [ ] **Step 5: Commit**

Run:

```bash
git add crates/ahandd/src/plugin_runtime/capability.rs crates/ahandd/src/plugin_runtime/mod.rs
git commit -m "feat(ahandd): add capability router model"
```

---

### Task 2: Build Capability State From Plugin Resources And Host Config

**Files:**
- Create: `crates/ahandd/src/plugin_runtime/activation.rs`
- Modify: `crates/ahandd/src/plugin_runtime/mod.rs`
- Modify: `crates/ahandd/src/browser.rs`
- Test: module tests inside `activation.rs`

- [ ] **Step 1: Write failing activation tests**

Create `crates/ahandd/src/plugin_runtime/activation.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_runtime::{InstalledPluginResource, PluginStatus};
    use std::collections::BTreeMap;

    fn plugin(id: &str, status: PluginStatus, dependencies: &[&str]) -> InstalledPluginResource {
        InstalledPluginResource {
            id: id.to_string(),
            version: "0.1.0".to_string(),
            status,
            dependencies: dependencies.iter().map(|dep| dep.to_string()).collect(),
            capabilities: Vec::new(),
            resources: BTreeMap::new(),
            help_prompt: None,
        }
    }

    fn base_plugins() -> Vec<InstalledPluginResource> {
        vec![
            plugin("shell", PluginStatus::Installed, &[]),
            plugin("file", PluginStatus::Installed, &[]),
            plugin("node", PluginStatus::Installed, &[]),
            plugin("browser-playwright-cli", PluginStatus::Installed, &["shell", "node"]),
        ]
    }

    #[test]
    fn file_disabled_by_host_config_is_not_installable() {
        let router = router_from_plugins(
            &base_plugins(),
            ActivationConfig {
                browser_enabled: true,
                file_enabled: false,
                system_browser_available: true,
            },
        );

        let err = router.ensure(CapabilityKind::File).unwrap_err();
        assert_eq!(err.plugin_id, "file");
        assert_eq!(
            err.remediation,
            CapabilityRemediation::HostConfiguration {
                message: "enable file operations in host configuration".to_string()
            }
        );
    }

    #[test]
    fn browser_missing_node_recommends_installing_browser_plugin() {
        let mut plugins = base_plugins();
        plugins[2].status = PluginStatus::Missing;
        plugins[3].status = PluginStatus::Blocked;

        let router = router_from_plugins(
            &plugins,
            ActivationConfig {
                browser_enabled: true,
                file_enabled: true,
                system_browser_available: true,
            },
        );

        let err = router.ensure(CapabilityKind::Browser).unwrap_err();
        assert_eq!(err.plugin_id, "browser-playwright-cli");
        assert_eq!(err.reason, "dependency node is missing");
        assert_eq!(
            err.remediation,
            CapabilityRemediation::InstallPlugin {
                plugin_id: "browser-playwright-cli".to_string()
            }
        );
    }

    #[test]
    fn browser_disabled_by_host_config_does_not_suggest_install() {
        let router = router_from_plugins(
            &base_plugins(),
            ActivationConfig {
                browser_enabled: false,
                file_enabled: true,
                system_browser_available: true,
            },
        );

        let err = router.ensure(CapabilityKind::Browser).unwrap_err();
        assert_eq!(
            err.remediation,
            CapabilityRemediation::HostConfiguration {
                message: "enable browser capabilities in host configuration".to_string()
            }
        );
    }

    #[test]
    fn missing_system_browser_is_host_environment_error() {
        let router = router_from_plugins(
            &base_plugins(),
            ActivationConfig {
                browser_enabled: true,
                file_enabled: true,
                system_browser_available: false,
            },
        );

        let err = router.ensure(CapabilityKind::Browser).unwrap_err();
        assert_eq!(
            err.remediation,
            CapabilityRemediation::HostEnvironment {
                message: "install or configure a supported system browser".to_string()
            }
        );
    }
}
```

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
cargo test -p ahandd plugin_runtime::activation
```

Expected: compile failure because `ActivationConfig` and `router_from_plugins` do not exist.

- [ ] **Step 3: Implement pure activation mapping**

Add this implementation above the tests in `activation.rs`:

```rust
use crate::browser::BrowserManager;
use crate::file_manager::FileManager;

use super::{
    CapabilityEntry, CapabilityKind, CapabilityRemediation, CapabilityRouter,
    CapabilityUnavailable, InstalledPluginResource, PluginStatus,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivationConfig {
    pub browser_enabled: bool,
    pub file_enabled: bool,
    pub system_browser_available: bool,
}

pub fn router_from_plugins(
    plugins: &[InstalledPluginResource],
    config: ActivationConfig,
) -> CapabilityRouter {
    CapabilityRouter::new(vec![
        exec_entry(plugins),
        file_entry(plugins, config),
        browser_entry(plugins, config),
    ])
}

fn exec_entry(plugins: &[InstalledPluginResource]) -> CapabilityEntry {
    match plugin_status(plugins, "shell") {
        Some(PluginStatus::Installed) => CapabilityEntry::active(CapabilityKind::Exec, "shell"),
        Some(status) => CapabilityEntry::unavailable(CapabilityUnavailable {
            capability: CapabilityKind::Exec,
            plugin_id: "shell".to_string(),
            status,
            reason: "host shell unavailable".to_string(),
            remediation: CapabilityRemediation::HostEnvironment {
                message: "configure a valid host shell".to_string(),
            },
        }),
        None => CapabilityEntry::unavailable(CapabilityUnavailable {
            capability: CapabilityKind::Exec,
            plugin_id: "shell".to_string(),
            status: PluginStatus::Missing,
            reason: "shell plugin is not registered".to_string(),
            remediation: CapabilityRemediation::HostEnvironment {
                message: "configure a valid host shell".to_string(),
            },
        }),
    }
}

fn file_entry(plugins: &[InstalledPluginResource], config: ActivationConfig) -> CapabilityEntry {
    if !config.file_enabled {
        return CapabilityEntry::unavailable(CapabilityUnavailable {
            capability: CapabilityKind::File,
            plugin_id: "file".to_string(),
            status: PluginStatus::Blocked,
            reason: "host configuration disabled file operations".to_string(),
            remediation: CapabilityRemediation::HostConfiguration {
                message: "enable file operations in host configuration".to_string(),
            },
        });
    }

    match plugin_status(plugins, "file") {
        Some(PluginStatus::Installed) => CapabilityEntry::active(CapabilityKind::File, "file"),
        Some(status) => CapabilityEntry::unavailable(CapabilityUnavailable {
            capability: CapabilityKind::File,
            plugin_id: "file".to_string(),
            status,
            reason: format!("file plugin is {}", status_word(status)),
            remediation: CapabilityRemediation::HostEnvironment {
                message: "file capability is not available in this host".to_string(),
            },
        }),
        None => CapabilityEntry::unavailable(CapabilityUnavailable {
            capability: CapabilityKind::File,
            plugin_id: "file".to_string(),
            status: PluginStatus::Missing,
            reason: "file plugin is not registered".to_string(),
            remediation: CapabilityRemediation::HostEnvironment {
                message: "file capability is not available in this host".to_string(),
            },
        }),
    }
}

fn browser_entry(plugins: &[InstalledPluginResource], config: ActivationConfig) -> CapabilityEntry {
    if !config.browser_enabled {
        return CapabilityEntry::unavailable(CapabilityUnavailable {
            capability: CapabilityKind::Browser,
            plugin_id: "browser-playwright-cli".to_string(),
            status: PluginStatus::Blocked,
            reason: "host configuration disabled browser capabilities".to_string(),
            remediation: CapabilityRemediation::HostConfiguration {
                message: "enable browser capabilities in host configuration".to_string(),
            },
        });
    }

    if let Some(reason) = first_missing_dependency(plugins, "browser-playwright-cli") {
        return CapabilityEntry::unavailable(CapabilityUnavailable {
            capability: CapabilityKind::Browser,
            plugin_id: "browser-playwright-cli".to_string(),
            status: PluginStatus::Blocked,
            reason,
            remediation: CapabilityRemediation::InstallPlugin {
                plugin_id: "browser-playwright-cli".to_string(),
            },
        });
    }

    match plugin_status(plugins, "browser-playwright-cli") {
        Some(PluginStatus::Installed) if config.system_browser_available => {
            CapabilityEntry::active(CapabilityKind::Browser, "browser-playwright-cli")
        }
        Some(PluginStatus::Installed) => CapabilityEntry::unavailable(CapabilityUnavailable {
            capability: CapabilityKind::Browser,
            plugin_id: "browser-playwright-cli".to_string(),
            status: PluginStatus::Blocked,
            reason: "no supported system browser is available".to_string(),
            remediation: CapabilityRemediation::HostEnvironment {
                message: "install or configure a supported system browser".to_string(),
            },
        }),
        Some(status) => CapabilityEntry::unavailable(CapabilityUnavailable {
            capability: CapabilityKind::Browser,
            plugin_id: "browser-playwright-cli".to_string(),
            status,
            reason: format!("browser-playwright-cli plugin is {}", status_word(status)),
            remediation: CapabilityRemediation::InstallPlugin {
                plugin_id: "browser-playwright-cli".to_string(),
            },
        }),
        None => CapabilityEntry::unavailable(CapabilityUnavailable {
            capability: CapabilityKind::Browser,
            plugin_id: "browser-playwright-cli".to_string(),
            status: PluginStatus::Missing,
            reason: "browser-playwright-cli plugin is not registered".to_string(),
            remediation: CapabilityRemediation::InstallPlugin {
                plugin_id: "browser-playwright-cli".to_string(),
            },
        }),
    }
}

fn first_missing_dependency(plugins: &[InstalledPluginResource], plugin_id: &str) -> Option<String> {
    let plugin = plugins.iter().find(|plugin| plugin.id == plugin_id)?;
    plugin.dependencies.iter().find_map(|dependency| {
        let status = plugin_status(plugins, dependency).unwrap_or(PluginStatus::Missing);
        if status == PluginStatus::Installed {
            None
        } else {
            Some(format!("dependency {} is {}", dependency, status_word(status)))
        }
    })
}

fn plugin_status(plugins: &[InstalledPluginResource], plugin_id: &str) -> Option<PluginStatus> {
    plugins
        .iter()
        .find(|plugin| plugin.id == plugin_id)
        .map(|plugin| plugin.status)
}

fn status_word(status: PluginStatus) -> &'static str {
    match status {
        PluginStatus::Installed => "installed",
        PluginStatus::Missing => "missing",
        PluginStatus::Outdated => "outdated",
        PluginStatus::Failed => "failed",
        PluginStatus::Blocked => "blocked",
    }
}
```

- [ ] **Step 4: Add live router builder and browser helper**

Modify `crates/ahandd/src/browser.rs`:

```rust
impl BrowserManager {
    pub fn has_system_browser(&self) -> bool {
        self.resolve_executable_path().is_some()
    }
}
```

Add this to `activation.rs`:

```rust
pub async fn build_router(
    browser_mgr: &BrowserManager,
    file_mgr: &FileManager,
) -> anyhow::Result<CapabilityRouter> {
    let snapshot = super::get_host_resource().await?;
    Ok(router_from_plugins(
        &snapshot.plugins,
        ActivationConfig {
            browser_enabled: browser_mgr.is_enabled(),
            file_enabled: file_mgr.is_enabled(),
            system_browser_available: browser_mgr.has_system_browser(),
        },
    ))
}
```

Modify `crates/ahandd/src/plugin_runtime/mod.rs`:

```rust
pub mod activation;

pub use activation::{ActivationConfig, build_router, router_from_plugins};
```

- [ ] **Step 5: Run tests and verify GREEN**

Run:

```bash
cargo test -p ahandd plugin_runtime::activation
cargo test -p ahandd browser::tests::resolve_browser
```

Expected: activation tests pass. The browser test filter may run zero tests if no matching names exist; that is acceptable here because Task 5 reruns the full relevant browser setup tests.

- [ ] **Step 6: Commit**

Run:

```bash
git add crates/ahandd/src/plugin_runtime/activation.rs crates/ahandd/src/plugin_runtime/mod.rs crates/ahandd/src/browser.rs
git commit -m "feat(ahandd): derive capability activation state"
```

---

### Task 3: Derive Hello Capabilities From The Router

**Files:**
- Modify: `crates/ahandd/src/ahand_client.rs`
- Test: module tests inside `ahand_client.rs` and existing handshake tests

- [ ] **Step 1: Write a failing Hello capability test**

Add this test to the existing `#[cfg(test)]` module in `crates/ahandd/src/ahand_client.rs`:

```rust
#[test]
fn hello_capabilities_preserve_router_order_and_names() {
    let capabilities = hello_capabilities_from_wire_names(vec![
        "exec".to_string(),
        "file".to_string(),
        "browser-playwright-cli".to_string(),
    ]);

    assert_eq!(capabilities, vec!["exec", "file", "browser-playwright-cli"]);
}
```

- [ ] **Step 2: Run the test and verify RED**

Run:

```bash
cargo test -p ahandd hello_capabilities_preserve_router_order_and_names
```

Expected: compile failure because `hello_capabilities_from_wire_names` does not exist.

- [ ] **Step 3: Add capability-aware Hello helpers**

Modify `crates/ahandd/src/ahand_client.rs`.

Add:

```rust
fn hello_capabilities_from_wire_names(capabilities: Vec<String>) -> Vec<String> {
    capabilities
}
```

Change `build_hello_envelope` to keep the current public signature as a compatibility wrapper:

```rust
pub fn build_hello_envelope(
    device_id: &str,
    identity: &DeviceIdentity,
    last_ack: u64,
    browser_enabled: bool,
    file_enabled: bool,
    challenge_nonce: &[u8],
    bearer_token: Option<String>,
) -> Envelope {
    let mut capabilities = vec!["exec".to_string()];
    if browser_enabled {
        capabilities.push("browser-playwright-cli".to_string());
    }
    if file_enabled {
        capabilities.push("file".to_string());
    }
    build_hello_envelope_with_capabilities(
        device_id,
        identity,
        last_ack,
        capabilities,
        challenge_nonce,
        bearer_token,
    )
}
```

Add the new function:

```rust
pub fn build_hello_envelope_with_capabilities(
    device_id: &str,
    identity: &DeviceIdentity,
    last_ack: u64,
    capabilities: Vec<String>,
    challenge_nonce: &[u8],
    bearer_token: Option<String>,
) -> Envelope {
    let signed_at_ms = identity.next_hello_signed_at_ms();
    let mut hello = Hello {
        version: env!("CARGO_PKG_VERSION").to_string(),
        hostname: gethostname::gethostname().to_string_lossy().to_string(),
        os: std::env::consts::OS.to_string(),
        capabilities: hello_capabilities_from_wire_names(capabilities),
        last_ack,
        auth: None,
    };

    let signature = identity.sign_hello(device_id, &hello, signed_at_ms, challenge_nonce);
    hello.auth = if let Some(token) = bearer_token {
        Some(hello::Auth::Bootstrap(ahand_protocol::BootstrapAuth {
            bearer_token: token,
            public_key: identity.public_key_bytes(),
            signature,
            signed_at_ms,
        }))
    } else {
        Some(hello::Auth::Ed25519(ahand_protocol::Ed25519Auth {
            public_key: identity.public_key_bytes(),
            signature,
            signed_at_ms,
        }))
    };

    Envelope {
        device_id: device_id.to_string(),
        msg_id: "hello-0".to_string(),
        ts_ms: signed_at_ms,
        payload: Some(envelope::Payload::Hello(hello)),
        ..Default::default()
    }
}
```

Move the existing body from `build_hello_envelope` into the new function instead of duplicating it.

- [ ] **Step 4: Use router-derived Hello capabilities during connect**

In `connect_with_auth`, replace:

```rust
let hello = build_hello_envelope(
    device_id,
    identity,
    last_ack,
    browser_mgr.is_enabled(),
    file_mgr.is_enabled(),
    &challenge.nonce,
    match auth_mode {
        HelloAuthMode::Ed25519 => None,
        HelloAuthMode::Bootstrap(token) => Some(token.clone()),
    },
);
```

with:

```rust
let capability_router = crate::plugin_runtime::build_router(browser_mgr, file_mgr)
    .await
    .map_err(ConnectError::Session)?;
let hello = build_hello_envelope_with_capabilities(
    device_id,
    identity,
    last_ack,
    capability_router
        .active_wire_capabilities()
        .into_iter()
        .map(str::to_string)
        .collect(),
    &challenge.nonce,
    match auth_mode {
        HelloAuthMode::Ed25519 => None,
        HelloAuthMode::Bootstrap(token) => Some(token.clone()),
    },
);
```

- [ ] **Step 5: Run tests and verify GREEN**

Run:

```bash
cargo test -p ahandd hello_capabilities_preserve_router_order_and_names
cargo test -p ahandd --test hub_handshake
cargo test -p ahandd --test hello_signature
```

Expected: all tests pass.

- [ ] **Step 6: Commit**

Run:

```bash
git add crates/ahandd/src/ahand_client.rs crates/ahandd/tests/hub_handshake.rs crates/ahandd/tests/hello_signature.rs
git commit -m "feat(ahandd): advertise active plugin capabilities"
```

If the test files do not need changes because the wrapper preserved the old signature, stage only `crates/ahandd/src/ahand_client.rs`.

---

### Task 4: Gate Cloud WebSocket Dispatch

**Files:**
- Modify: `crates/ahandd/src/ahand_client.rs`
- Test: module tests inside `ahand_client.rs`

- [ ] **Step 1: Add pure response helper tests**

Add tests to the existing `#[cfg(test)]` module in `crates/ahandd/src/ahand_client.rs`:

```rust
#[test]
fn browser_unavailable_response_preserves_request_ids() {
    let req = ahand_protocol::BrowserRequest {
        request_id: "browser-1".to_string(),
        session_id: "session-1".to_string(),
        action: "open".to_string(),
        ..Default::default()
    };
    let unavailable = crate::plugin_runtime::CapabilityUnavailable {
        capability: crate::plugin_runtime::CapabilityKind::Browser,
        plugin_id: "browser-playwright-cli".to_string(),
        status: crate::plugin_runtime::PluginStatus::Blocked,
        reason: "dependency node is missing".to_string(),
        remediation: crate::plugin_runtime::CapabilityRemediation::InstallPlugin {
            plugin_id: "browser-playwright-cli".to_string(),
        },
    };

    let resp = browser_unavailable_response(&req, &unavailable);

    assert_eq!(resp.request_id, "browser-1");
    assert_eq!(resp.session_id, "session-1");
    assert!(!resp.success);
    assert!(resp.error.contains("install plugin browser-playwright-cli through the host plugin installer"));
}

#[test]
fn file_unavailable_response_uses_policy_denied_error() {
    let req = ahand_protocol::FileRequest {
        request_id: "file-1".to_string(),
        ..Default::default()
    };
    let unavailable = crate::plugin_runtime::CapabilityUnavailable {
        capability: crate::plugin_runtime::CapabilityKind::File,
        plugin_id: "file".to_string(),
        status: crate::plugin_runtime::PluginStatus::Blocked,
        reason: "host configuration disabled file operations".to_string(),
        remediation: crate::plugin_runtime::CapabilityRemediation::HostConfiguration {
            message: "enable file operations in host configuration".to_string(),
        },
    };

    let resp = file_unavailable_response(&req, &unavailable);
    let Some(ahand_protocol::file_response::Result::Error(err)) = resp.result else {
        panic!("expected file error");
    };

    assert_eq!(resp.request_id, "file-1");
    assert_eq!(err.code, ahand_protocol::FileErrorCode::PolicyDenied as i32);
    assert!(err.message.contains("file capability unavailable"));
}
```

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
cargo test -p ahandd unavailable_response
```

Expected: compile failure because helper functions do not exist.

- [ ] **Step 3: Add protocol response helpers**

Add these private helpers in `crates/ahandd/src/ahand_client.rs` near the existing request handlers:

```rust
fn browser_unavailable_response(
    req: &ahand_protocol::BrowserRequest,
    unavailable: &crate::plugin_runtime::CapabilityUnavailable,
) -> BrowserResponse {
    BrowserResponse {
        request_id: req.request_id.clone(),
        session_id: req.session_id.clone(),
        success: false,
        error: unavailable.to_protocol_message(),
        ..Default::default()
    }
}

fn file_unavailable_response(
    req: &ahand_protocol::FileRequest,
    unavailable: &crate::plugin_runtime::CapabilityUnavailable,
) -> ahand_protocol::FileResponse {
    crate::file_manager::error_response(
        req.request_id.clone(),
        ahand_protocol::FileErrorCode::PolicyDenied,
        "",
        &unavailable.to_protocol_message(),
    )
}
```

- [ ] **Step 4: Gate `handle_job_request`**

At the top of `handle_job_request`, before idempotency/session checks, add:

```rust
match crate::plugin_runtime::build_router(browser_mgr, file_mgr)
    .await
    .and_then(|router| router.ensure(crate::plugin_runtime::CapabilityKind::Exec).map_err(anyhow::Error::from))
{
    Ok(()) => {}
    Err(err) => {
        let reason = err.to_string();
        warn!(job_id = %req.job_id, reason = %reason, "job rejected by capability router");
        let reject_env = Envelope {
            device_id: device_id.to_string(),
            msg_id: new_msg_id(),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::JobRejected(JobRejected {
                job_id: req.job_id.clone(),
                reason,
            })),
            ..Default::default()
        };
        let _ = tx.send(reject_env);
        return;
    }
}
```

To avoid awkward `anyhow::Error::from` for a custom type, prefer this exact shape instead:

```rust
let capability_router = match crate::plugin_runtime::build_router(browser_mgr, file_mgr).await {
    Ok(router) => router,
    Err(err) => {
        reject_job_for_capability_error(device_id, &req, tx, err.to_string());
        return;
    }
};
if let Err(unavailable) = capability_router.ensure(crate::plugin_runtime::CapabilityKind::Exec) {
    reject_job_for_capability_error(device_id, &req, tx, unavailable.to_protocol_message());
    return;
}
```

Add a helper:

```rust
fn reject_job_for_capability_error<T>(
    device_id: &str,
    req: &ahand_protocol::JobRequest,
    tx: &T,
    reason: String,
) where
    T: crate::executor::EnvelopeSink,
{
    let reject_env = Envelope {
        device_id: device_id.to_string(),
        msg_id: new_msg_id(),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::JobRejected(JobRejected {
            job_id: req.job_id.clone(),
            reason,
        })),
        ..Default::default()
    };
    let _ = tx.send(reject_env);
}
```

Update `handle_job_request` signature to receive `browser_mgr: &Arc<BrowserManager>` and `file_mgr: &Arc<FileManager>` so it can build the router.

- [ ] **Step 5: Gate `handle_browser_request`**

At the start of `handle_browser_request`, after the initial log and before `browser_mgr.is_enabled()`, add:

```rust
let capability_router = match crate::plugin_runtime::build_router(browser_mgr, file_mgr).await {
    Ok(router) => router,
    Err(err) => {
        let resp = BrowserResponse {
            request_id: req.request_id.clone(),
            session_id: req.session_id.clone(),
            success: false,
            error: format!("browser capability unavailable: {}", err),
            ..Default::default()
        };
        let _ = tx.send(Envelope {
            device_id: device_id.to_string(),
            msg_id: new_msg_id(),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::BrowserResponse(resp)),
            ..Default::default()
        });
        return;
    }
};
if let Err(unavailable) = capability_router.ensure(crate::plugin_runtime::CapabilityKind::Browser) {
    let resp = browser_unavailable_response(req, &unavailable);
    let _ = tx.send(Envelope {
        device_id: device_id.to_string(),
        msg_id: new_msg_id(),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::BrowserResponse(resp)),
        ..Default::default()
    });
    return;
}
```

Update `handle_browser_request` signature to receive `file_mgr: &Arc<FileManager>` because router construction needs both managers.

- [ ] **Step 6: Gate `handle_file_request`**

At the start of `handle_file_request`, after the log and before `req_paths_joined`, add:

```rust
let capability_router = match crate::plugin_runtime::build_router(browser_mgr, file_mgr).await {
    Ok(router) => router,
    Err(err) => {
        send_file_response(crate::file_manager::error_response(
            req.request_id.clone(),
            ahand_protocol::FileErrorCode::PolicyDenied,
            "",
            &format!("file capability unavailable: {}", err),
        ));
        return;
    }
};
if let Err(unavailable) = capability_router.ensure(crate::plugin_runtime::CapabilityKind::File) {
    send_file_response(file_unavailable_response(&req, &unavailable));
    return;
}
```

Update `handle_file_request` signature to receive `browser_mgr: &Arc<BrowserManager>`.

- [ ] **Step 7: Update call sites**

Update the `match envelope.payload` block in `connect_with_auth`:

```rust
Some(envelope::Payload::JobRequest(req)) => {
    handle_job_request(
        req,
        device_id,
        caller_uid,
        &tx,
        session_mgr,
        registry,
        store,
        approval_mgr,
        approval_broadcast_tx,
        browser_mgr,
        file_mgr,
    )
    .await;
}
Some(envelope::Payload::BrowserRequest(req)) => {
    handle_browser_request(device_id, caller_uid, &req, &tx, session_mgr, browser_mgr, file_mgr)
        .await;
}
Some(envelope::Payload::FileRequest(req)) => {
    handle_file_request(
        device_id,
        caller_uid,
        req,
        &tx,
        session_mgr,
        browser_mgr,
        file_mgr,
        approval_mgr,
        approval_broadcast_tx,
        &close_rx,
    )
    .await;
}
```

- [ ] **Step 8: Run tests and verify GREEN**

Run:

```bash
cargo test -p ahandd unavailable_response
cargo test -p ahandd ahand_client
cargo check -p ahandd
```

Expected: tests and check pass.

- [ ] **Step 9: Commit**

Run:

```bash
git add crates/ahandd/src/ahand_client.rs
git commit -m "feat(ahandd): gate cloud requests by plugin capability"
```

---

### Task 5: Gate Debug IPC Dispatch

**Files:**
- Modify: `crates/ahandd/src/ipc.rs`
- Modify: `crates/ahandd/src/main.rs`
- Test: module tests inside `ipc.rs` if helper functions are added

- [ ] **Step 1: Add IPC helper tests**

Add a small `#[cfg(test)]` module to `crates/ahandd/src/ipc.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_browser_unavailable_response_preserves_ids() {
        let req = ahand_protocol::BrowserRequest {
            request_id: "ipc-browser-1".to_string(),
            session_id: "ipc-session-1".to_string(),
            ..Default::default()
        };
        let unavailable = crate::plugin_runtime::CapabilityUnavailable {
            capability: crate::plugin_runtime::CapabilityKind::Browser,
            plugin_id: "browser-playwright-cli".to_string(),
            status: crate::plugin_runtime::PluginStatus::Missing,
            reason: "browser-playwright-cli plugin is missing".to_string(),
            remediation: crate::plugin_runtime::CapabilityRemediation::InstallPlugin {
                plugin_id: "browser-playwright-cli".to_string(),
            },
        };

        let resp = browser_unavailable_response(&req, &unavailable);

        assert_eq!(resp.request_id, "ipc-browser-1");
        assert_eq!(resp.session_id, "ipc-session-1");
        assert!(!resp.success);
        assert!(resp.error.contains("host plugin installer"));
    }
}
```

- [ ] **Step 2: Run test and verify RED**

Run:

```bash
cargo test -p ahandd ipc_browser_unavailable_response_preserves_ids
```

Expected: compile failure because the IPC helper does not exist.

- [ ] **Step 3: Pass `FileManager` into IPC**

Modify imports in `crates/ahandd/src/ipc.rs`:

```rust
use crate::file_manager::FileManager;
```

Modify `serve_ipc` signature:

```rust
pub async fn serve_ipc(
    socket_path: PathBuf,
    socket_mode: u32,
    registry: Arc<JobRegistry>,
    store: Option<Arc<RunStore>>,
    session_mgr: Arc<SessionManager>,
    approval_mgr: Arc<ApprovalManager>,
    approval_broadcast_tx: broadcast::Sender<Envelope>,
    device_id: String,
    browser_mgr: Arc<BrowserManager>,
    file_mgr: Arc<FileManager>,
) -> anyhow::Result<()>
```

Clone `file_mgr` in the accept loop and pass it into `handle_ipc_conn`.

Modify `handle_ipc_conn` signature to accept `file_mgr: Arc<FileManager>`.

Update both `ipc::serve_ipc` call sites in `crates/ahandd/src/main.rs` to pass `Arc::clone(&file_mgr)`.

- [ ] **Step 4: Add IPC unavailable helpers**

Add to `ipc.rs`:

```rust
fn browser_unavailable_response(
    req: &ahand_protocol::BrowserRequest,
    unavailable: &crate::plugin_runtime::CapabilityUnavailable,
) -> BrowserResponse {
    BrowserResponse {
        request_id: req.request_id.clone(),
        session_id: req.session_id.clone(),
        success: false,
        error: unavailable.to_protocol_message(),
        ..Default::default()
    }
}

fn reject_job_for_capability_error(
    device_id: &str,
    req: &ahand_protocol::JobRequest,
    tx: &mpsc::UnboundedSender<Envelope>,
    reason: String,
) {
    let reject_env = Envelope {
        device_id: device_id.to_string(),
        msg_id: new_msg_id(),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::JobRejected(JobRejected {
            job_id: req.job_id.clone(),
            reason,
        })),
        ..Default::default()
    };
    let _ = tx.send(reject_env);
}
```

- [ ] **Step 5: Gate IPC `JobRequest`**

In `handle_ipc_conn`, inside `Some(envelope::Payload::JobRequest(req))`, before idempotency checks, add:

```rust
let capability_router = match crate::plugin_runtime::build_router(&browser_mgr, &file_mgr).await {
    Ok(router) => router,
    Err(err) => {
        reject_job_for_capability_error(&device_id, &req, &tx, err.to_string());
        continue;
    }
};
if let Err(unavailable) = capability_router.ensure(crate::plugin_runtime::CapabilityKind::Exec) {
    reject_job_for_capability_error(&device_id, &req, &tx, unavailable.to_protocol_message());
    continue;
}
```

- [ ] **Step 6: Gate IPC `BrowserRequest`**

In `handle_ipc_conn`, inside `Some(envelope::Payload::BrowserRequest(req))`, before `browser_mgr.is_enabled()`, add:

```rust
let capability_router = match crate::plugin_runtime::build_router(&browser_mgr, &file_mgr).await {
    Ok(router) => router,
    Err(err) => {
        let resp_env = Envelope {
            device_id: device_id.clone(),
            msg_id: new_msg_id(),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::BrowserResponse(BrowserResponse {
                request_id: req.request_id.clone(),
                session_id: req.session_id.clone(),
                success: false,
                error: format!("browser capability unavailable: {}", err),
                ..Default::default()
            })),
            ..Default::default()
        };
        let _ = tx.send(resp_env);
        continue;
    }
};
if let Err(unavailable) = capability_router.ensure(crate::plugin_runtime::CapabilityKind::Browser) {
    let resp_env = Envelope {
        device_id: device_id.clone(),
        msg_id: new_msg_id(),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::BrowserResponse(browser_unavailable_response(
            &req,
            &unavailable,
        ))),
        ..Default::default()
    };
    let _ = tx.send(resp_env);
    continue;
}
```

- [ ] **Step 7: Run tests and verify GREEN**

Run:

```bash
cargo test -p ahandd ipc_browser_unavailable_response_preserves_ids
cargo check -p ahandd
```

Expected: test and check pass.

- [ ] **Step 8: Commit**

Run:

```bash
git add crates/ahandd/src/ipc.rs crates/ahandd/src/main.rs
git commit -m "feat(ahandd): gate ipc requests by plugin capability"
```

---

### Task 6: Verification And Cleanup

**Files:**
- Modify only if verification exposes compile, formatting, or test issues.

- [ ] **Step 1: Run targeted formatter**

Run:

```bash
rustfmt --edition 2024 --check --config skip_children=true \
  crates/ahandd/src/plugin_runtime/capability.rs \
  crates/ahandd/src/plugin_runtime/activation.rs \
  crates/ahandd/src/plugin_runtime/mod.rs \
  crates/ahandd/src/browser.rs \
  crates/ahandd/src/ahand_client.rs \
  crates/ahandd/src/ipc.rs \
  crates/ahandd/src/main.rs
```

Expected: exit 0. If it fails, run the same command without `--check`, inspect `git diff`, then rerun with `--check`.

- [ ] **Step 2: Run capability and plugin runtime tests**

Run:

```bash
cargo test -p ahandd plugin_runtime
```

Expected: all plugin runtime tests pass.

- [ ] **Step 3: Run request-path tests**

Run:

```bash
cargo test -p ahandd ahand_client
cargo test -p ahandd ipc
cargo test -p ahandd --test hub_handshake
cargo test -p ahandd --test hello_signature
```

Expected: all tests pass.

- [ ] **Step 4: Run browser setup regression tests**

Run:

```bash
cargo test -p ahandd browser_setup
cargo test -p ahandd --test browser_doctor
```

Expected: all tests pass.

- [ ] **Step 5: Run package checks and CLI smoke tests**

Run:

```bash
cargo check -p ahandd -p ahandctl
cargo build -p ahandd --bin ahandd
target/debug/ahandd plugin host-resource --json >/tmp/ahand-stage2-host-resource.json
jq -r '[.platform,.arch,((.plugins|map(.id)|sort)|join(","))] | @tsv' /tmp/ahand-stage2-host-resource.json
target/debug/ahandd plugin doctor
```

Expected:

- `cargo check` and `cargo build` pass.
- JSON smoke prints `darwin arm64 browser-playwright-cli,file,node,python,shell` on this machine.
- `plugin doctor` prints statuses for the five built-in plugins.

- [ ] **Step 6: Verify diff hygiene**

Run:

```bash
git diff --check 8d65c32..HEAD
git status --short
```

Expected: no whitespace errors. `git status --short` should show no unexpected changes before the final commit. If `pnpm-lock.yaml` appears, do not stage it unless a task explicitly required it.

- [ ] **Step 7: Commit final fixes if needed**

If Task 6 produced fixes, commit them:

```bash
git add \
  crates/ahandd/src/plugin_runtime/capability.rs \
  crates/ahandd/src/plugin_runtime/activation.rs \
  crates/ahandd/src/plugin_runtime/mod.rs \
  crates/ahandd/src/browser.rs \
  crates/ahandd/src/ahand_client.rs \
  crates/ahandd/src/ipc.rs \
  crates/ahandd/src/main.rs
git commit -m "fix: stabilize plugin runtime stage 2"
```

If no fixes were needed, do not create an empty commit.

## Completion Criteria

Stage 2 is complete when:

- Capability router model and activation tests pass.
- Hello advertises active capabilities from plugin activation state.
- Cloud WebSocket `JobRequest`, `FileRequest`, and `BrowserRequest` gate before executing.
- Debug IPC `JobRequest` and `BrowserRequest` gate before executing.
- Managed browser plugin failures return host-neutral install guidance.
- Built-in shell/file failures return host environment/configuration guidance.
- OpenClaw files are unchanged.
- Verification commands in Task 6 pass or any residual environmental failures are clearly documented.
