# AHand Plugin Runtime Stage 3 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add first-party capability providers and explicit managed `node` / `python` runtime execution without changing existing PATH-based `JobRequest.tool` behavior.

**Architecture:** Extend Stage 2 capability activation with `NodeExec` and `PythonExec`, then add a provider registry that resolves `JobRequest.tool` to either the existing shell-backed executor or a managed runtime executable. Existing file and browser flows keep their concrete managers but resolve through the provider registry before dispatch.

**Tech Stack:** Rust, Tokio, prost protocol envelopes, existing `ahandd` executor, `plugin_runtime`, `BrowserManager`, `FileManager`.

---

## File Structure

- Modify `crates/ahandd/src/plugin_runtime/capability.rs`
  - Add `NodeExec` and `PythonExec` capability ids and wire/display names.
  - Include them in active Hello capability enumeration when present and active.
- Modify `crates/ahandd/src/plugin_runtime/activation.rs`
  - Add activation entries for `node-exec` and `python-exec`.
  - Require installed plugin status plus exported executable resource.
- Create `crates/ahandd/src/plugin_runtime/provider.rs`
  - Define provider registry and `JobProvider` selection.
  - Resolve `plugin:node` and `plugin:python` explicitly.
  - Export `build_provider_registry(...)` for cloud WS and IPC.
- Modify `crates/ahandd/src/plugin_runtime/mod.rs`
  - Export provider types.
- Modify `crates/ahandd/src/executor.rs`
  - Add `ExecutionTarget`.
  - Add `run_job_with_target`.
  - Keep `run_job` as compatibility wrapper around `resolve_tool`.
- Modify `crates/ahandd/src/ahand_client.rs`
  - Use provider registry for Job/File/Browser capability checks.
  - Pass resolved job provider into job spawning.
  - Reject managed runtime interactive jobs.
- Modify `crates/ahandd/src/ipc.rs`
  - Mirror cloud job/browser provider resolution.
  - Reject managed runtime interactive jobs.
- Modify tests in the same modules plus `crates/ahandd/tests/job_request_tool.rs`
  - Keep old `node` / `python` pass-through contract.
  - Add explicit `plugin:node` / `plugin:python` provider tests.

---

## Task 1: Extend Capability Activation For Managed Runtime Execution

**Files:**
- Modify: `crates/ahandd/src/plugin_runtime/capability.rs`
- Modify: `crates/ahandd/src/plugin_runtime/activation.rs`

- [ ] **Step 1: Write failing capability tests**

Add tests to `crates/ahandd/src/plugin_runtime/capability.rs`:

```rust
#[test]
fn active_wire_capabilities_include_managed_runtime_exec_when_active() {
    let router = CapabilityRouter::new(vec![
        CapabilityEntry::active(CapabilityKind::Exec, "shell"),
        CapabilityEntry::active(CapabilityKind::NodeExec, "node"),
        CapabilityEntry::active(CapabilityKind::PythonExec, "python"),
    ]);

    assert_eq!(
        router.active_wire_capabilities(),
        vec!["exec", "node-exec", "python-exec"]
    );
}
```

Add tests to `crates/ahandd/src/plugin_runtime/activation.rs`:

```rust
#[test]
fn node_exec_active_when_node_resource_is_exported() {
    let mut plugins = base_plugins();
    plugins[2].resources.insert(
        "node".to_string(),
        crate::plugin_runtime::HostResourceValue::Executable {
            name: "node".to_string(),
            path: "/tmp/ahand/node/bin/node".to_string(),
            version: Some("v24.13.0".to_string()),
        },
    );

    let router = router_from_plugins(
        &plugins,
        ActivationConfig {
            browser_enabled: true,
            file_enabled: true,
            system_browser_available: true,
        },
    );

    assert!(router.ensure(CapabilityKind::NodeExec).is_ok());
}

#[test]
fn python_exec_missing_recommends_installing_python_plugin() {
    let plugins = base_plugins();

    let router = router_from_plugins(
        &plugins,
        ActivationConfig {
            browser_enabled: true,
            file_enabled: true,
            system_browser_available: true,
        },
    );

    let err = router.ensure(CapabilityKind::PythonExec).unwrap_err();
    assert_eq!(err.plugin_id, "python");
    assert!(matches!(
        err.remediation,
        CapabilityRemediation::InstallPlugin { ref plugin_id } if plugin_id == "python"
    ));
}
```

- [ ] **Step 2: Run tests to verify RED**

Run:

```bash
cargo test -p ahandd plugin_runtime::capability::tests::active_wire_capabilities_include_managed_runtime_exec_when_active
cargo test -p ahandd plugin_runtime::activation::tests::node_exec_active_when_node_resource_is_exported
cargo test -p ahandd plugin_runtime::activation::tests::python_exec_missing_recommends_installing_python_plugin
```

Expected: compile failures because `CapabilityKind::NodeExec` and `CapabilityKind::PythonExec` do not exist.

- [ ] **Step 3: Implement capability ids and activation**

In `capability.rs`, extend `CapabilityKind`:

```rust
pub enum CapabilityKind {
    Exec,
    File,
    Browser,
    NodeExec,
    PythonExec,
}
```

Update `wire_name()`:

```rust
Self::NodeExec => "node-exec",
Self::PythonExec => "python-exec",
```

Update `display_name()`:

```rust
Self::NodeExec => "node",
Self::PythonExec => "python",
```

Update `active_wire_capabilities()` order:

```rust
[
    CapabilityKind::Exec,
    CapabilityKind::File,
    CapabilityKind::Browser,
    CapabilityKind::NodeExec,
    CapabilityKind::PythonExec,
]
```

In `activation.rs`, add entries to `CapabilityRouter::new(...)`:

```rust
runtime_entry(plugins, CapabilityKind::NodeExec, "node", "node"),
runtime_entry(plugins, CapabilityKind::PythonExec, "python", "python"),
```

Add helpers:

```rust
fn runtime_entry(
    plugins: &[InstalledPluginResource],
    capability: CapabilityKind,
    plugin_id: &str,
    resource_name: &str,
) -> CapabilityEntry {
    match plugin_status(plugins, plugin_id) {
        Some(PluginStatus::Installed) if has_executable_resource(plugins, plugin_id, resource_name) => {
            CapabilityEntry::active(capability, plugin_id)
        }
        Some(PluginStatus::Installed) => CapabilityEntry::unavailable(CapabilityUnavailable {
            capability,
            plugin_id: plugin_id.to_string(),
            status: PluginStatus::Missing,
            reason: format!("{plugin_id} executable resource is missing"),
            remediation: CapabilityRemediation::InstallPlugin {
                plugin_id: plugin_id.to_string(),
            },
        }),
        Some(status) => CapabilityEntry::unavailable(CapabilityUnavailable {
            capability,
            plugin_id: plugin_id.to_string(),
            status,
            reason: format!("{plugin_id} plugin is {}", status_word(status)),
            remediation: CapabilityRemediation::InstallPlugin {
                plugin_id: plugin_id.to_string(),
            },
        }),
        None => CapabilityEntry::unavailable(CapabilityUnavailable {
            capability,
            plugin_id: plugin_id.to_string(),
            status: PluginStatus::Missing,
            reason: format!("{plugin_id} plugin is not registered"),
            remediation: CapabilityRemediation::InstallPlugin {
                plugin_id: plugin_id.to_string(),
            },
        }),
    }
}

fn has_executable_resource(
    plugins: &[InstalledPluginResource],
    plugin_id: &str,
    resource_name: &str,
) -> bool {
    plugins
        .iter()
        .find(|plugin| plugin.id == plugin_id)
        .and_then(|plugin| plugin.resources.get(resource_name))
        .is_some_and(|resource| matches!(
            resource,
            crate::plugin_runtime::HostResourceValue::Executable { .. }
        ))
}
```

- [ ] **Step 4: Run tests to verify GREEN**

Run:

```bash
cargo test -p ahandd plugin_runtime::capability
cargo test -p ahandd plugin_runtime::activation
```

Expected: all targeted tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ahandd/src/plugin_runtime/capability.rs crates/ahandd/src/plugin_runtime/activation.rs
git commit -m "feat(ahandd): activate managed runtime exec capabilities"
```

---

## Task 2: Add Explicit Executor Target Support

**Files:**
- Modify: `crates/ahandd/src/executor.rs`

- [ ] **Step 1: Write failing executor test**

Add to `crates/ahandd/src/executor.rs` tests:

```rust
#[tokio::test]
async fn run_job_with_target_uses_explicit_executable_path() {
    use std::os::unix::fs::PermissionsExt;
    use tokio::sync::mpsc;

    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("managed-runtime");
    std::fs::write(&script, "#!/bin/sh\necho managed:$1\n").unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).unwrap();

    let (tx, mut rx) = mpsc::unbounded_channel();
    let (_cancel_tx, cancel_rx) = mpsc::channel(1);
    let req = JobRequest {
        job_id: "managed-runtime-job".to_string(),
        tool: "plugin:node".to_string(),
        args: vec!["ok".to_string()],
        ..Default::default()
    };

    let (exit_code, error) = run_job_with_target(
        "device-1".to_string(),
        req,
        ExecutionTarget {
            path: script.to_string_lossy().to_string(),
            leading_args: Vec::new(),
        },
        tx,
        cancel_rx,
        None,
    )
    .await;

    assert_eq!(exit_code, 0);
    assert_eq!(error, "");

    let mut saw_stdout = false;
    while let Ok(env) = rx.try_recv() {
        if let Some(ahand_protocol::envelope::Payload::JobEvent(event)) = env.payload {
            if let Some(ahand_protocol::job_event::Event::StdoutChunk(bytes)) = event.event {
                saw_stdout |= String::from_utf8_lossy(&bytes).contains("managed:ok");
            }
        }
    }
    assert!(saw_stdout, "managed runtime stdout should be streamed");
}
```

- [ ] **Step 2: Run test to verify RED**

Run:

```bash
cargo test -p ahandd executor::tool_resolution_tests::run_job_with_target_uses_explicit_executable_path
```

Expected: compile failure because `ExecutionTarget` and `run_job_with_target` do not exist.

- [ ] **Step 3: Implement explicit target runner**

In `executor.rs`, add:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionTarget {
    pub path: String,
    pub leading_args: Vec<String>,
}

impl From<ResolvedTool> for ExecutionTarget {
    fn from(value: ResolvedTool) -> Self {
        Self {
            path: value.path,
            leading_args: value.leading_args,
        }
    }
}
```

Refactor `run_job`:

```rust
pub async fn run_job<T>(...) -> (i32, String)
where
    T: EnvelopeSink,
{
    let target = ExecutionTarget::from(resolve_tool(
        &req.tool,
        std::env::var("SHELL").ok().as_deref(),
    ));
    run_job_with_target(device_id, req, target, tx, cancel_rx, store).await
}
```

Add `run_job_with_target` by moving the current body of `run_job` and replacing internal `resolve_tool(...)` usage with the passed `target`:

```rust
let mut cmd = Command::new(&target.path);
for leading in &target.leading_args {
    cmd.arg(leading);
}
cmd.args(&req.args);
```

- [ ] **Step 4: Run tests to verify GREEN**

Run:

```bash
cargo test -p ahandd executor::tool_resolution_tests
cargo test -p ahandd --test job_request_tool
```

Expected: executor tests pass; external job tool contract still passes.

- [ ] **Step 5: Commit**

```bash
git add crates/ahandd/src/executor.rs
git commit -m "feat(ahandd): run jobs with explicit execution targets"
```

---

## Task 3: Add Provider Registry And Job Tool Selection

**Files:**
- Create: `crates/ahandd/src/plugin_runtime/provider.rs`
- Modify: `crates/ahandd/src/plugin_runtime/mod.rs`

- [ ] **Step 1: Write failing provider tests**

Create `crates/ahandd/src/plugin_runtime/provider.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_plugin_tokens_select_managed_runtime_providers() {
        assert_eq!(
            resolve_job_provider_kind("plugin:node"),
            JobProviderKind::ManagedRuntime(CapabilityKind::NodeExec)
        );
        assert_eq!(
            resolve_job_provider_kind("plugin:python"),
            JobProviderKind::ManagedRuntime(CapabilityKind::PythonExec)
        );
    }

    #[test]
    fn plain_node_and_python_stay_default_exec_provider() {
        assert_eq!(resolve_job_provider_kind("node"), JobProviderKind::DefaultExec);
        assert_eq!(resolve_job_provider_kind("python"), JobProviderKind::DefaultExec);
    }
}
```

- [ ] **Step 2: Run test to verify RED**

Run:

```bash
cargo test -p ahandd plugin_runtime::provider
```

Expected: compile failure because the module and provider types do not exist.

- [ ] **Step 3: Implement provider module**

Add `crates/ahandd/src/plugin_runtime/provider.rs`:

```rust
use std::collections::BTreeMap;

use crate::browser::BrowserManager;
use crate::executor::ExecutionTarget;
use crate::file_manager::FileManager;

use super::{
    ActivationConfig, CapabilityKind, CapabilityRemediation, CapabilityRouter,
    CapabilityUnavailable, HostResourceValue, InstalledPluginResource, PluginStatus,
    router_from_plugins,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobProviderKind {
    DefaultExec,
    ManagedRuntime(CapabilityKind),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobProvider {
    DefaultExec,
    ManagedRuntime {
        capability: CapabilityKind,
        target: ExecutionTarget,
    },
}

#[derive(Debug, Clone)]
pub struct CapabilityProviderRegistry {
    router: CapabilityRouter,
    runtime_targets: BTreeMap<CapabilityKind, ExecutionTarget>,
}

pub async fn build_provider_registry(
    browser_mgr: &BrowserManager,
    file_mgr: &FileManager,
) -> anyhow::Result<CapabilityProviderRegistry> {
    let snapshot = super::get_host_resource().await?;
    Ok(CapabilityProviderRegistry::from_plugins(
        &snapshot.plugins,
        ActivationConfig {
            browser_enabled: browser_mgr.is_enabled(),
            file_enabled: file_mgr.is_enabled(),
            system_browser_available: browser_mgr.has_system_browser(),
        },
    ))
}

impl CapabilityProviderRegistry {
    pub fn from_plugins(
        plugins: &[InstalledPluginResource],
        config: ActivationConfig,
    ) -> Self {
        let router = router_from_plugins(plugins, config);
        let mut runtime_targets = BTreeMap::new();
        if let Some(target) = executable_target(plugins, "node", "node") {
            runtime_targets.insert(CapabilityKind::NodeExec, target);
        }
        if let Some(target) = executable_target(plugins, "python", "python") {
            runtime_targets.insert(CapabilityKind::PythonExec, target);
        }
        Self {
            router,
            runtime_targets,
        }
    }

    pub fn ensure(&self, capability: CapabilityKind) -> Result<(), super::CapabilityUnavailable> {
        self.router.ensure(capability)
    }

    pub fn active_wire_capabilities(&self) -> Vec<&'static str> {
        self.router.active_wire_capabilities()
    }

    pub fn resolve_job_provider(
        &self,
        tool: &str,
    ) -> Result<JobProvider, super::CapabilityUnavailable> {
        match resolve_job_provider_kind(tool) {
            JobProviderKind::DefaultExec => {
                self.ensure(CapabilityKind::Exec)?;
                Ok(JobProvider::DefaultExec)
            }
            JobProviderKind::ManagedRuntime(capability) => {
                self.ensure(capability)?;
                let target = self
                    .runtime_targets
                    .get(&capability)
                    .cloned()
                    .ok_or_else(|| missing_runtime_target(capability))?;
                Ok(JobProvider::ManagedRuntime { capability, target })
            }
        }
    }
}

pub fn resolve_job_provider_kind(tool: &str) -> JobProviderKind {
    match tool {
        "plugin:node" => JobProviderKind::ManagedRuntime(CapabilityKind::NodeExec),
        "plugin:python" => JobProviderKind::ManagedRuntime(CapabilityKind::PythonExec),
        _ => JobProviderKind::DefaultExec,
    }
}

fn executable_target(
    plugins: &[InstalledPluginResource],
    plugin_id: &str,
    resource_name: &str,
) -> Option<ExecutionTarget> {
    let plugin = plugins.iter().find(|plugin| plugin.id == plugin_id)?;
    match plugin.resources.get(resource_name)? {
        HostResourceValue::Executable { path, .. } => Some(ExecutionTarget {
            path: path.clone(),
            leading_args: Vec::new(),
        }),
        _ => None,
    }
}

fn missing_runtime_target(capability: CapabilityKind) -> CapabilityUnavailable {
    let plugin_id = match capability {
        CapabilityKind::NodeExec => "node",
        CapabilityKind::PythonExec => "python",
        _ => capability.wire_name(),
    };
    CapabilityUnavailable {
        capability,
        plugin_id: plugin_id.to_string(),
        status: PluginStatus::Missing,
        reason: format!("{plugin_id} executable resource is missing"),
        remediation: CapabilityRemediation::InstallPlugin {
            plugin_id: plugin_id.to_string(),
        },
    }
}
```

Modify `mod.rs`:

```rust
pub mod provider;
pub use provider::{
    CapabilityProviderRegistry, JobProvider, JobProviderKind, build_provider_registry,
    resolve_job_provider_kind,
};
```

- [ ] **Step 4: Run provider tests to verify GREEN**

Run:

```bash
cargo test -p ahandd plugin_runtime::provider
```

Expected: provider tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ahandd/src/plugin_runtime/provider.rs crates/ahandd/src/plugin_runtime/mod.rs
git commit -m "feat(ahandd): add capability provider registry"
```

---

## Task 4: Route Cloud Job Dispatch Through Providers

**Files:**
- Modify: `crates/ahandd/src/ahand_client.rs`

- [ ] **Step 1: Write failing cloud provider tests**

Add tests to `crates/ahandd/src/ahand_client.rs`:

```rust
#[test]
fn managed_runtime_interactive_rejection_preserves_job_id() {
    let req = ahand_protocol::JobRequest {
        job_id: "node-interactive-1".to_string(),
        tool: "plugin:node".to_string(),
        interactive: true,
        ..Default::default()
    };

    let env = super::managed_runtime_interactive_rejection("device-1", &req);

    assert_eq!(env.device_id, "device-1");
    match env.payload {
        Some(envelope::Payload::JobRejected(rejected)) => {
            assert_eq!(rejected.job_id, "node-interactive-1");
            assert!(rejected.reason.contains("plugin:node"));
            assert!(rejected.reason.contains("interactive"));
        }
        other => panic!("expected JobRejected, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run test to verify RED**

Run:

```bash
cargo test -p ahandd managed_runtime_interactive_rejection_preserves_job_id
```

Expected: compile failure because `managed_runtime_interactive_rejection` does not exist.

- [ ] **Step 3: Implement cloud provider dispatch**

Add helper:

```rust
fn managed_runtime_interactive_rejection(
    device_id: &str,
    req: &ahand_protocol::JobRequest,
) -> Envelope {
    Envelope {
        device_id: device_id.to_string(),
        msg_id: new_msg_id(),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::JobRejected(JobRejected {
            job_id: req.job_id.clone(),
            reason: format!(
                "managed runtime tool {} does not support interactive PTY jobs",
                req.tool
            ),
        })),
        ..Default::default()
    }
}
```

In `handle_job_request`, replace `build_router(...)` with:

```rust
let provider_registry =
    match crate::plugin_runtime::build_provider_registry(browser_mgr, file_mgr).await {
        Ok(registry) => registry,
        Err(err) => { ... }
    };
let job_provider = match provider_registry.resolve_job_provider(&req.tool) {
    Ok(provider) => provider,
    Err(unavailable) => { ... }
};
if req.interactive && matches!(job_provider, crate::plugin_runtime::JobProvider::ManagedRuntime { .. }) {
    let _ = tx.send(managed_runtime_interactive_rejection(device_id, &req));
    return;
}
```

Change `spawn_job` signature:

```rust
async fn spawn_job<T>(
    device_id: &str,
    req: ahand_protocol::JobRequest,
    provider: crate::plugin_runtime::JobProvider,
    tx: &T,
    registry: &Arc<JobRegistry>,
    store: &Option<Arc<RunStore>>,
)
```

In non-interactive branch:

```rust
match provider {
    crate::plugin_runtime::JobProvider::DefaultExec => {
        executor::run_job(did, req, tx_clone, cancel_rx, st).await
    }
    crate::plugin_runtime::JobProvider::ManagedRuntime { target, .. } => {
        executor::run_job_with_target(did, req, target, tx_clone, cancel_rx, st).await
    }
}
```

Pass `job_provider.clone()` into the Allow and approval-granted paths.

For `handle_browser_request` and `handle_file_request`, replace `build_router(...)` with `build_provider_registry(...)` and call `provider_registry.ensure(CapabilityKind::Browser/File)`.

- [ ] **Step 4: Run cloud tests to verify GREEN**

Run:

```bash
cargo test -p ahandd ahand_client
cargo test -p ahandd --test hub_handshake
cargo test -p ahandd --test hello_signature
```

Expected: all targeted tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ahandd/src/ahand_client.rs
git commit -m "feat(ahandd): route cloud jobs through capability providers"
```

---

## Task 5: Route Debug IPC Job Dispatch Through Providers

**Files:**
- Modify: `crates/ahandd/src/ipc.rs`

- [ ] **Step 1: Write failing IPC provider test**

Add to `crates/ahandd/src/ipc.rs` tests:

```rust
#[test]
fn ipc_managed_runtime_interactive_rejection_preserves_job_id() {
    let req = ahand_protocol::JobRequest {
        job_id: "ipc-python-interactive-1".to_string(),
        tool: "plugin:python".to_string(),
        interactive: true,
        ..Default::default()
    };

    let env = managed_runtime_interactive_rejection_envelope("device-1", &req);

    match env.payload {
        Some(envelope::Payload::JobRejected(rejected)) => {
            assert_eq!(rejected.job_id, "ipc-python-interactive-1");
            assert!(rejected.reason.contains("plugin:python"));
            assert!(rejected.reason.contains("interactive"));
        }
        other => panic!("expected JobRejected, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run test to verify RED**

Run:

```bash
cargo test -p ahandd ipc_managed_runtime_interactive_rejection_preserves_job_id
```

Expected: compile failure because helper does not exist.

- [ ] **Step 3: Implement IPC provider dispatch**

Add helper:

```rust
fn managed_runtime_interactive_rejection_envelope(
    device_id: &str,
    req: &ahand_protocol::JobRequest,
) -> Envelope {
    Envelope {
        device_id: device_id.to_string(),
        msg_id: new_msg_id(),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::JobRejected(JobRejected {
            job_id: req.job_id.clone(),
            reason: format!(
                "managed runtime tool {} does not support interactive PTY jobs",
                req.tool
            ),
        })),
        ..Default::default()
    }
}
```

In IPC `JobRequest` branch:

```rust
let provider_registry =
    match crate::plugin_runtime::build_provider_registry(&browser_mgr, &file_mgr).await { ... };
let job_provider = match provider_registry.resolve_job_provider(&req.tool) { ... };
if req.interactive && matches!(job_provider, crate::plugin_runtime::JobProvider::ManagedRuntime { .. }) {
    let _ = tx.send(managed_runtime_interactive_rejection_envelope(&device_id, &req));
    continue;
}
```

In Allow and approval-granted task execution, call:

```rust
match job_provider {
    crate::plugin_runtime::JobProvider::DefaultExec => {
        executor::run_job(did, req, tx_clone, cancel_rx, st).await
    }
    crate::plugin_runtime::JobProvider::ManagedRuntime { target, .. } => {
        executor::run_job_with_target(did, req, target, tx_clone, cancel_rx, st).await
    }
}
```

In BrowserRequest branch, replace `build_router(...)` with `build_provider_registry(...)`.

- [ ] **Step 4: Run IPC tests to verify GREEN**

Run:

```bash
cargo test -p ahandd ipc
cargo check -p ahandd
```

Expected: IPC tests and check pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ahandd/src/ipc.rs
git commit -m "feat(ahandd): route ipc jobs through capability providers"
```

---

## Task 6: Verify Runtime Resource CLI And Full Stage 3 Behavior

**Files:**
- No code edits expected.

- [ ] **Step 1: Run formatting check for touched Rust files**

Run:

```bash
rustfmt --edition 2024 --config skip_children=true --check \
  crates/ahandd/src/ahand_client.rs \
  crates/ahandd/src/executor.rs \
  crates/ahandd/src/ipc.rs \
  crates/ahandd/src/plugin_runtime/activation.rs \
  crates/ahandd/src/plugin_runtime/capability.rs \
  crates/ahandd/src/plugin_runtime/mod.rs \
  crates/ahandd/src/plugin_runtime/provider.rs
```

Expected: exit 0.

- [ ] **Step 2: Run targeted tests**

Run:

```bash
cargo test -p ahandd plugin_runtime
cargo test -p ahandd executor::tool_resolution_tests
cargo test -p ahandd ahand_client
cargo test -p ahandd ipc
cargo test -p ahandd --test job_request_tool
cargo test -p ahandd --test hub_handshake
cargo test -p ahandd --test hello_signature
```

Expected: all pass.

- [ ] **Step 3: Run build/check smoke tests**

Run:

```bash
cargo check -p ahandd -p ahandctl
cargo build -p ahandd --bin ahandd
target/debug/ahandd plugin host-resource --json >/tmp/ahand-stage3-host-resource.json
jq -r '[.platform,.arch,((.plugins|map(.id)|sort)|join(","))] | @tsv' /tmp/ahand-stage3-host-resource.json
target/debug/ahandd plugin doctor
```

Expected:

- build/check exit 0.
- host-resource includes `browser-playwright-cli,file,node,python,shell`.
- doctor prints statuses for those five plugins.

- [ ] **Step 4: Run diff and status checks**

Run:

```bash
git diff --check codex/plugin-runtime-stage2..HEAD
git status --short
```

Expected: no whitespace errors; clean worktree.

- [ ] **Step 5: Push and create PR**

Run:

```bash
git push -u origin codex/plugin-runtime-stage3
gh pr create --draft --repo team9ai/aHand --base codex/plugin-runtime-stage2 --head codex/plugin-runtime-stage3 --title "[codex] Plugin runtime stage 3 providers" --body-file /tmp/ahand-stage3-pr.md
```

Expected: draft PR created against Stage 2.
