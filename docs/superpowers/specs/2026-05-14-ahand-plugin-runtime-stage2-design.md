# AHand Plugin Runtime Stage 2 Design

**Date:** 2026-05-14
**Status:** Draft for review
**Base:** Stage 1 plugin runtime foundation in `codex/plugin-runtime-stage1`

## Overview

Stage 2 moves aHand's existing `Envelope` capability dispatch behind first-party plugin activation while keeping the wire protocol stable. This covers the cloud WebSocket dispatch path and the debug IPC dispatch path for the payloads they already support.

Stage 1 made plugins inspectable and discoverable. Stage 2 makes those plugin states authoritative for runtime capability entry points:

```text
JobRequest     -> exec capability    -> shell plugin
FileRequest    -> file capability    -> file plugin
BrowserRequest -> browser capability -> browser-playwright-cli plugin
```

If a capability's owner plugin is not active, the daemon rejects the request before calling the old handler. The rejection includes a host-neutral remediation hint. CLI hosts may render that hint as an `ahandd plugin install ...` command, but embedded crate hosts such as team9 should map it to their own host installer action or Rust API.

OpenClaw is explicitly out of scope for this stage.

## Goals

- Put `JobRequest`, `FileRequest`, and `BrowserRequest` behind plugin capability activation.
- Keep protobuf messages and hub wire behavior unchanged.
- Preserve existing policy, approval, idempotency, cancellation, PTY, browser-domain, and file-policy behavior after a capability is accepted.
- Make plugin state and actual dispatch behavior consistent: unavailable plugins cannot be bypassed by falling back to old handlers.
- Return actionable, host-neutral install or configuration guidance when a capability is unavailable.
- Keep `shell` and `file` as built-in host capability plugins, not downloadable dependencies.
- Keep `node` and `python` as standalone runtime plugins that can later expose direct execution without depending on `shell`.

## Non-Goals

- OpenClaw `system.run`, `browser.proxy`, or command registry migration.
- Third-party plugin packages or dynamic code loading.
- Automatic plugin installation as a side effect of request dispatch.
- Rewriting `JobRequest`, `FileRequest`, or `BrowserRequest` protobuf schemas.
- Reinterpreting `JobRequest.tool = "node"` or `"python"` to mean the managed runtime path.
- Dashboard plugin management UI.

## Plugin And Capability Model

Stage 2 separates three concepts that Stage 1 represented loosely:

1. **Plugin installed state**: whether the plugin's own runtime resources are present and valid.
2. **Host configuration state**: whether the daemon configuration allows the capability to be used.
3. **Active capability state**: whether the capability may currently handle requests.

A capability is active only when both its plugin installed state and relevant host configuration state allow it.

### `shell`

`shell` is a built-in host capability plugin.

It owns the protocol-level `exec` capability and handles `JobRequest`. It is active when the host has an executable shell path. It should not produce an installer hint because there is no first-party downloadable shell plugin in Stage 2. If unavailable, the error should describe a host environment problem, such as an invalid `$SHELL` or missing platform shell.

### `file`

`file` is a built-in host capability plugin.

It owns `FileRequest`. It is active when the plugin is built in and file operations are enabled by daemon file policy configuration. If file policy disables file operations, Stage 2 reports a configuration-disabled capability rather than a missing plugin.

### `browser-playwright-cli`

`browser-playwright-cli` is a managed capability plugin.

It owns `BrowserRequest`. It is active when:

- host browser capability config is enabled,
- `shell` is active,
- `node` is installed,
- `playwright-cli` is installed at the managed runtime path, and
- a supported system browser is available or configured.

If `node` or `playwright-cli` is missing, the response should recommend installing `browser-playwright-cli` through the host plugin installer. The installer may install dependencies first.

### `node` and `python`

`node` and `python` are standalone runtime plugins.

They do not depend on `shell`. They export executable resources and can later expose direct execution capabilities through explicit protocol or host action surfaces. Stage 2 should not silently route a generic `JobRequest.tool = "node"` or `"python"` to managed runtime binaries, because that would change existing PATH-based command semantics.

The Stage 2 router may reserve internal capability ids for future use:

```text
node-exec   -> node
python-exec -> python
```

Those reserved capabilities are not advertised or dispatched until a later stage defines the request surface.

## Capability Router

Add a small internal router under `crates/ahandd/src/plugin_runtime/`:

```rust
pub enum CapabilityKind {
    Exec,
    File,
    Browser,
}

pub struct CapabilityRouter {
    entries: BTreeMap<CapabilityKind, CapabilityEntry>,
}

pub struct CapabilityEntry {
    pub capability: CapabilityKind,
    pub owner_plugin_id: String,
    pub state: CapabilityState,
}

pub enum CapabilityState {
    Active,
    Unavailable(CapabilityUnavailable),
}

pub struct CapabilityUnavailable {
    pub capability: CapabilityKind,
    pub plugin_id: String,
    pub status: PluginStatus,
    pub reason: String,
    pub remediation: CapabilityRemediation,
}

pub enum CapabilityRemediation {
    None,
    HostConfiguration { message: String },
    HostEnvironment { message: String },
    InstallPlugin { plugin_id: String },
}
```

Exact names can change during implementation, but the boundary should stay the same: request handlers ask one component whether a capability is active and receive a structured denial reason if it is not.

The router is built from:

- built-in plugin manifests,
- read-only plugin inspection / host resource state,
- daemon config relevant to capability enablement.

The router must not install, repair, or mutate the runtime.

## Dispatch Flow

### JobRequest

Current flow:

```text
JobRequest -> session/idempotency/approval -> executor::run_job or run_job_pty
```

Stage 2 flow:

```text
JobRequest
  -> CapabilityRouter.ensure(Exec)
  -> if unavailable: JobRejected
  -> existing idempotency/session/approval flow
  -> executor::run_job or run_job_pty
```

`shell` unavailable should be rare and should be reported as host environment unavailable, not as a plugin install request.

### FileRequest

Current flow:

```text
FileRequest -> FileManager::is_enabled/check_request_approval/handle
```

Stage 2 flow:

```text
FileRequest
  -> CapabilityRouter.ensure(File)
  -> if unavailable: FileResponse error
  -> existing file policy/session/approval flow
  -> FileManager::handle
```

File capability disabled by config should return the same broad error class used today (`PolicyDenied`) but with a message that names plugin capability activation, for example:

```text
file capability unavailable: host configuration disabled file operations
```

### BrowserRequest

Current flow:

```text
BrowserRequest -> BrowserManager::is_enabled/check_domain/execute
```

Stage 2 flow:

```text
BrowserRequest
  -> CapabilityRouter.ensure(Browser)
  -> if unavailable: BrowserResponse error
  -> existing session/domain checks
  -> BrowserManager::execute
```

When runtime resources are missing, the error should be host-neutral:

```text
browser capability unavailable: plugin browser-playwright-cli is blocked because dependency node is missing; install plugin browser-playwright-cli through the host plugin installer
```

The error must not assume an `ahandd` binary exists.

## Hello Capability Advertisement

Hello currently reports:

```text
exec
browser-playwright-cli
file
```

Stage 2 should compute the same wire-compatible names from active router state:

- `exec` is advertised when `CapabilityKind::Exec` is active.
- `file` is advertised when `CapabilityKind::File` is active.
- `browser-playwright-cli` is advertised when `CapabilityKind::Browser` is active.

This keeps hub behavior stable while making advertised capabilities match local dispatch availability. If the hub sends a request for an unavailable capability anyway, the local router still rejects it.

## Error Semantics

Stage 2 does not add new protobuf fields, so structured errors are kept inside the daemon and rendered into existing string fields at the protocol boundary.

Rendered messages should include:

- capability name,
- owner plugin id,
- status or config reason,
- host-neutral remediation when one exists.

Examples:

```text
exec capability unavailable: host shell unavailable
file capability unavailable: host configuration disabled file operations
browser capability unavailable: plugin browser-playwright-cli is blocked because dependency node is missing; install plugin browser-playwright-cli through the host plugin installer
```

Future host APIs can expose `CapabilityUnavailable` directly without changing the Stage 2 router boundary.

## Policy And Approval

The router is a gate, not a replacement for policy.

Order matters:

1. Capability unavailable: reject immediately.
2. Capability available: run existing session mode, approval, and policy checks unchanged.

This avoids approval prompts for operations that cannot execute and keeps existing safety controls authoritative after the capability gate passes.

## Implementation Shape

Stage 2 should prefer a conservative adapter over a full dynamic plugin trait system.

Recommended modules:

```text
crates/ahandd/src/plugin_runtime/capability.rs
crates/ahandd/src/plugin_runtime/activation.rs
```

`capability.rs` owns ids, router types, remediation rendering, and unit tests.

`activation.rs` builds a router from Stage 1 inspection data and host config. It may start small and delegate to current `host_resource` helpers.

Existing handlers keep their concrete executor / file manager / browser manager calls after the gate. Full handler trait registration can wait until Stage 3 when external plugin packages and richer runtime state exist.

## Compatibility

- The hub wire protocol stays unchanged.
- Existing browser setup commands stay unchanged.
- `getHostResource` stays read-only.
- Existing CLI and crate callers can keep using current daemon entry points.
- OpenClaw behavior is unchanged.
- `JobRequest.tool` resolution remains current behavior, including `$SHELL`, `shell`, and literal executable names.

## Testing Strategy

Add focused tests before implementation:

- Router marks `exec` active when `shell` is installed.
- Router marks `exec` unavailable with `HostEnvironment` when shell is missing.
- Router marks `file` unavailable with `HostConfiguration` when file policy disables operations.
- Router marks `browser` unavailable with `InstallPlugin { plugin_id: "browser-playwright-cli" }` when `node` is missing.
- Router marks `browser` unavailable when browser config is disabled, with host configuration remediation rather than install remediation.
- Hello capability construction only advertises active capabilities.
- `BrowserRequest` returns a `BrowserResponse` error instead of spawning when browser plugin is unavailable.
- `FileRequest` returns a `FileResponse` policy error instead of dispatching when file capability is unavailable.
- `JobRequest` returns `JobRejected` instead of spawning when exec capability is unavailable.

Existing tests for executor, file policy, browser domain policy, browser setup, and host resource serialization should remain green.

## Acceptance Criteria

- `JobRequest`, `FileRequest`, and `BrowserRequest` all pass through the capability router before executing.
- Cloud WebSocket and debug IPC handlers use the same capability gate for supported aHand `Envelope` payloads.
- Unavailable capabilities are rejected with clear protocol-level errors.
- Managed plugin failures produce host-neutral install hints, not hard-coded CLI commands.
- Built-in plugin failures produce host environment or host configuration hints, not install hints.
- Hello capabilities are derived from active router state.
- No OpenClaw code paths change.
- `node` and `python` remain shell-independent runtime plugins.
