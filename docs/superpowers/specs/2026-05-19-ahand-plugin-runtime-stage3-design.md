# AHand Plugin Runtime Stage 3 Design

**Date:** 2026-05-19
**Status:** Draft for review
**Base:** Stage 2 plugin runtime dispatch activation in `codex/plugin-runtime-stage2`

## Overview

Stage 3 turns Stage 2's capability gates into first-party capability providers. Stage 2 already decides whether a capability is active before dispatch. Stage 3 makes the active capability own the execution adapter too, so cloud WebSocket and debug IPC dispatch can resolve a provider from a registry rather than hard-coding `executor`, `FileManager`, and `BrowserManager` calls directly at every entry point.

This stage also introduces explicit managed runtime execution capabilities:

```text
node-exec   -> node plugin
python-exec -> python plugin
```

These capabilities must not change existing `JobRequest.tool = "node"` or `JobRequest.tool = "python"` behavior. Those existing values remain PATH-based executable names. Managed runtime execution is selected only through explicit provider-owned tool tokens.

OpenClaw remains out of scope.

## Goals

- Add a first-party capability provider registry for the current daemon-owned capabilities.
- Move cloud WebSocket and debug IPC execution paths to provider lookup after Stage 2 activation checks.
- Preserve current protocol payloads and current request/response envelopes.
- Add explicit managed runtime direct execution for the `node` and `python` plugins.
- Keep `node` and `python` shell-independent: provider execution should run the managed binary path directly, not via shell.
- Keep existing policy, approval, idempotency, cancellation, PTY, file policy, and browser domain behavior intact.
- Keep unavailable capability responses host-neutral and compatible with team9 crate-mode hosts.

## Non-Goals

- Third-party plugin package loading.
- Dynamic plugin ABI or sandboxing.
- Dashboard plugin management UI.
- Automatic plugin installation during dispatch.
- OpenClaw command handler migration.
- Changing protobuf schemas.
- Changing the meaning of existing `JobRequest.tool = "node"` or `"python"`.

## Capability Provider Model

Stage 3 adds an internal provider layer under `crates/ahandd/src/plugin_runtime/`. A provider is a first-party adapter that knows how to execute one capability after the capability router says it is active.

Recommended shape:

```rust
pub enum CapabilityProviderKind {
    Exec,
    File,
    Browser,
    NodeExec,
    PythonExec,
}

pub struct CapabilityProviderRegistry {
    providers: BTreeMap<CapabilityProviderKind, CapabilityProviderEntry>,
}

pub struct CapabilityProviderEntry {
    pub capability: CapabilityProviderKind,
    pub owner_plugin_id: String,
    pub activation: CapabilityEntry,
}
```

The exact type names can change during implementation, but Stage 3 should keep a clear boundary:

1. `activation.rs` derives active/unavailable state from host resources and config.
2. `provider.rs` registers first-party providers from activation state.
3. cloud WS and debug IPC dispatch resolve providers from the registry.
4. provider execution delegates to existing concrete managers.

The provider registry is not a plugin installer and must not mutate runtime resources.

## Capability Set

### `exec`

Owner: `shell`

Provider behavior:

- Existing `JobRequest` flow remains the default shell-backed execution path.
- `JobRequest.tool = "$SHELL"` and `"shell"` keep current login-shell behavior.
- Literal tools keep current PATH/path resolution behavior.
- Interactive jobs continue using PTY support.

### `file`

Owner: `file`

Provider behavior:

- Existing `FileManager` policy, approval, and dispatch stay authoritative.
- Stage 3 should only change where the handler is resolved from, not the file operation semantics.

### `browser-playwright-cli`

Owner: `browser-playwright-cli`

Provider behavior:

- Existing `BrowserManager` domain policy and command execution stay authoritative.
- Browser session tracking and close handling stay unchanged.

### `node-exec`

Owner: `node`

Provider behavior:

- Active only when the `node` plugin is installed and exports a `node` executable resource.
- Selected by explicit `JobRequest.tool` token:

```text
plugin:node
```

- The provider runs the managed Node binary directly with `req.args`.
- `req.cwd`, `req.env`, cancellation, stdout/stderr events, run store, idempotency, and non-interactive job lifecycle match the current executor path.
- Interactive PTY is not supported in Stage 3. If a `plugin:node` request has `interactive = true`, return `JobRejected` with a clear reason.

### `python-exec`

Owner: `python`

Provider behavior:

- Active only when the `python` plugin is installed and exports a `python` executable resource.
- Selected by explicit `JobRequest.tool` token:

```text
plugin:python
```

- The provider runs the managed Python binary directly with `req.args`.
- `req.cwd`, `req.env`, cancellation, stdout/stderr events, run store, idempotency, and non-interactive job lifecycle match the current executor path.
- Interactive PTY is not supported in Stage 3. If a `plugin:python` request has `interactive = true`, return `JobRejected` with a clear reason.

## Tool Token Semantics

Stage 3 introduces explicit plugin tool tokens while preserving existing tool resolution:

```text
node          -> current PATH-based executable resolution
python        -> current PATH-based executable resolution
plugin:node   -> managed node runtime provider
plugin:python -> managed python runtime provider
```

This avoids surprising users who currently rely on shell PATH behavior while allowing agents to deliberately choose the managed runtime after reading `getHostResource()`.

Future protocol versions may add structured fields for provider selection. Stage 3 intentionally avoids schema changes.

## Activation

Stage 2 `CapabilityKind` should be extended to include:

```rust
NodeExec
PythonExec
```

Wire capability names:

```text
node-exec
python-exec
```

These names may be advertised in Hello when active. They are additive and do not change existing capability names.

Activation rules:

- `node-exec` active when the `node` plugin status is `Installed` and the host resource snapshot contains a `node` executable resource.
- `python-exec` active when the `python` plugin status is `Installed` and the host resource snapshot contains a `python` executable resource.
- Missing or failed runtimes use `InstallPlugin { plugin_id }` remediation.
- Runtime capability unavailable responses should mention the explicit tool token, for example:

```text
node capability unavailable: plugin node is missing because node plugin is missing; install plugin node through the host plugin installer
```

Exact wording can be adjusted, but it must remain host-neutral and must not hard-code `ahandd plugin install`.

## Dispatch Flow

### JobRequest

Stage 3 splits JobRequest provider selection:

```text
JobRequest
  -> resolve job provider from req.tool
  -> CapabilityRouter.ensure(provider capability)
  -> if unavailable: JobRejected
  -> existing idempotency/session/approval flow
  -> provider executes job
```

Provider resolution:

```text
plugin:node   -> NodeExec provider
plugin:python -> PythonExec provider
anything else -> Exec provider
```

Approval and idempotency must still happen before execution. Provider resolution is allowed before approval so the daemon can reject unavailable managed runtimes without prompting for an operation it cannot run.

### FileRequest

```text
FileRequest
  -> provider registry resolves File
  -> CapabilityRouter.ensure(File)
  -> existing policy/session/approval flow
  -> File provider delegates to FileManager
```

### BrowserRequest

```text
BrowserRequest
  -> provider registry resolves Browser
  -> CapabilityRouter.ensure(Browser)
  -> existing session/domain checks
  -> Browser provider delegates to BrowserManager
```

## Executor Changes

The existing `executor::run_job` resolves `JobRequest.tool` internally. Stage 3 needs a way for providers to pass an already-resolved executable path without changing default behavior.

Recommended minimal change:

```rust
pub struct ExecutionTarget {
    pub path: String,
    pub leading_args: Vec<String>,
}

pub async fn run_job_with_target<T>(
    device_id: String,
    req: JobRequest,
    target: ExecutionTarget,
    tx: T,
    cancel_rx: mpsc::Receiver<()>,
    store: Option<Arc<RunStore>>,
) -> (i32, String)
```

`run_job()` becomes a compatibility wrapper that calls `resolve_tool()` and then `run_job_with_target()`. This keeps existing tests and external behavior stable.

PTY execution remains shell/default-exec only in Stage 3.

## Error Semantics

Stage 3 keeps protocol-level errors in existing message types:

- unavailable job providers -> `JobRejected`
- unsupported interactive managed runtime job -> `JobRejected`
- unavailable file provider -> `FileResponse` policy-style error
- unavailable browser provider -> `BrowserResponse` error

The provider registry may use structured `CapabilityUnavailable` internally, but rendered protocol errors must remain stable strings.

## Testing Strategy

Add tests before implementation:

- Router advertises `node-exec` when node resource is installed.
- Router advertises `python-exec` when python resource is installed.
- Missing node returns install remediation for `node-exec`.
- Missing python returns install remediation for `python-exec`.
- `plugin:node` resolves to `NodeExec`; `node` resolves to default `Exec`.
- `plugin:python` resolves to `PythonExec`; `python` resolves to default `Exec`.
- `executor::run_job_with_target` runs an explicit executable path and does not call `resolve_tool`.
- managed runtime interactive requests return `JobRejected`.
- Hello advertises active `node-exec` / `python-exec` additively.
- Existing `job_request_tool` contract stays green for `node` / `python` PATH semantics.

## Acceptance Criteria

- Stage 3 introduces a first-party provider registry used by cloud WS and debug IPC dispatch.
- `plugin:node` and `plugin:python` execute managed runtime binaries directly when installed.
- Plain `node` and `python` retain current PATH-based execution.
- Hello advertises `node-exec` and `python-exec` only when active.
- Missing runtime plugins return host-neutral install guidance.
- Existing exec/file/browser behavior remains compatible.
- OpenClaw code paths are unchanged.
