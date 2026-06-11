# AHand App Tool Registry Design

**Date:** 2026-06-11
**Status:** Approved (brainstorm output)
**Base:** `dev` (plugin runtime Stage 1–3 merged; worktree branch `feat/app-tool-registry` off `origin/dev` d8af808)
**Scope:** Umbrella design for the cross-repo "app tools" initiative; detailed design for sub-project ① (this repo). Sub-projects ② (team9-agent-pi) and ③ (Coffice) are specified at architecture level here and get their own specs in their repos.

## Overview

Host applications that embed `ahandd` (team9 Tauri client today, Coffice next) need to expose
application-defined tools — tool definitions owned by the app, executed inside the running app
process — so that a cloud agent (team9-agent-pi) can discover and invoke them.

Today the agent only sees generic host tools (`run_command`, file ops, browser) routed through
`AhandBackend`/`HostComponent`. There is no channel for an app to say "I provide tool X with
schema S" and have the agent call it as a first-class tool.

This design adds:

1. **Proto**: dedicated messages for advertising and invoking app tools.
2. **ahandd**: an `app_tool_registry` module plus public `DaemonHandle` API for in-process
   registration with async handlers, gated by the existing session-mode approval flow.
3. **hub**: per-device tool catalog cache, control-plane query/invoke endpoints, webhook event,
   audit logging.
4. **SDK**: `CloudClient.listAppTools` / `CloudClient.invokeAppTool`.
5. **agent-pi** (sub-project ②): a `DeviceToolsProvider` implementing the existing
   `IToolProvider` interface, modeled on the capability-hub provider.
6. **Coffice** (sub-project ③): first real consumer — embeds `ahandd` in-process following the
   team9 pattern and registers document tools.

## Decision Record

All decisions below were confirmed with the user on 2026-06-11.

| # | Decision | Choice |
|---|----------|--------|
| 1 | Project decomposition | Three sub-projects, sequential: ① aHand full chain → ② agent-pi DeviceToolsProvider → ③ Coffice end-to-end integration. Each has its own spec → plan → implementation cycle. |
| 2 | App registration mechanism | In-process Rust API on `DaemonHandle` (definition + async handler callback). No IPC / manifest-CLI track this stage. |
| 3 | Protocol shape | New dedicated Envelope messages: `AppToolsUpdate` (full-snapshot advertising) + `AppToolRequest`/`AppToolResponse` (JSON in/out invocation). No reuse of `JobRequest`. |
| 4 | Discovery path for agent-pi | Direct hub control-plane API. Hub caches the per-device tool catalog; team9 DB is not involved. team9 remains the permissions SOT for device ownership only. |
| 5 | Approval model | Reuse session modes (`INACTIVE`/`STRICT`/`TRUST`/`AUTO_ACCEPT`) plus a per-tool `requires_approval` flag with tighten-only semantics (forces approval even in `TRUST`/`AUTO_ACCEPT`). |
| 6 | First consumer | Coffice (includes its baseline aHand integration: embedded `ahandd`, device identity, hub tenancy, session binding). |

## Architecture

```text
┌─ Coffice Desktop (Tauri) ───────────────────┐
│  app logic ──register_app_tool()──┐         │
│                            ahandd (in-proc) │
└───────────────────────────────┬─────────────┘
                   WS+protobuf  │  AppToolsUpdate ↑ / AppToolRequest ↓
                        ┌───────┴───────┐
                        │   ahand-hub   │  per-device tool catalog cache
                        └───────┬───────┘  GET /api/devices/:id/app-tools
                                │          POST /api/control/app-tool
                    CloudClient │
               ┌────────────────┴──────────────┐
               │  agent-pi DeviceToolsProvider │ → IToolProvider → TieredTool
               └───────────────────────────────┘
```

One `ahandd` instance is embedded in exactly one host application, so device ↔ app is 1:1 and
tool names are flat per device (no app namespace). This is an explicit assumption of this stage.

## Sub-project ①: aHand Full Chain (Detailed)

### Proto

New file `proto/ahand/v1/app_tool.proto`, imported by `envelope.proto`. Envelope `oneof payload`
gains tags **35/36/37** (tag 32 is permanently skipped per the existing wire-compat comment;
33/34 are taken by `FileRequest`/`FileResponse`):

```proto
message AppToolDescriptor {
  string name               = 1; // ^[a-z0-9_-]{1,64}$
  string description        = 2;
  string input_schema_json  = 3; // JSON Schema, serialized JSON object
  bool   requires_approval  = 4; // tighten-only: forces approval in every session mode
}

message AppToolsUpdate {            // daemon -> hub, Envelope tag 35
  uint64 revision = 1;              // monotonic per daemon process
  repeated AppToolDescriptor tools = 2; // FULL snapshot, replaces previous catalog
}

message AppToolRequest {            // hub -> daemon, Envelope tag 36
  string tool_call_id = 1;
  string name         = 2;
  string args_json    = 3;          // JSON object matching input_schema_json
  uint32 timeout_ms   = 4;          // daemon clamps to [1_000, 300_000]; 0 => default 60_000
}

message AppToolError {
  string code    = 1;               // see Error Codes below
  string message = 2;
}

message AppToolResponse {           // daemon -> hub, Envelope tag 37
  string tool_call_id = 1;
  oneof result {
    string       result_json = 2;
    AppToolError error       = 3;
  }
}
```

Snapshot semantics: every register/unregister sends the full list with a bumped `revision`; the
daemon also re-sends the snapshot after every successful Hello handshake (reconnect). The hub
replaces its cached catalog wholesale and ignores updates whose `revision` is not greater than
the cached one for the same connection epoch. Single response per invocation — no streaming in
this stage; long-running work is bounded by `timeout_ms`.

### ahandd: `app_tool_registry` module

New module `crates/ahandd/src/app_tool_registry/`, **parallel to** `plugin_runtime` — managed
runtimes (first-party plugins) and app tools are different concepts and stay separate.

Public API on the existing `DaemonHandle` (`crates/ahandd/src/public_api.rs`):

```rust
pub struct AppToolDef {
    pub name: String,                    // validated: ^[a-z0-9_-]{1,64}$
    pub description: String,
    pub input_schema: serde_json::Value, // must be a JSON object
    pub requires_approval: bool,
}

pub struct AppToolError { pub code: String, pub message: String }

pub type AppToolHandler = Arc<
    dyn Fn(serde_json::Value) -> BoxFuture<'static, Result<serde_json::Value, AppToolError>>
        + Send + Sync,
>;

impl DaemonHandle {
    /// Errors on invalid name/schema or duplicate name.
    pub fn register_app_tool(&self, def: AppToolDef, handler: AppToolHandler) -> anyhow::Result<()>;
    /// Returns true if the tool existed.
    pub fn unregister_app_tool(&self, name: &str) -> anyhow::Result<bool>;
}
```

Registration is dynamic (any time after `spawn()`); each change pushes an `AppToolsUpdate`
snapshot through the existing outbox so ordering and reconnect replay follow the established
delivery path.

### Invocation path and approval gating

`AppToolRequest` dispatch reuses the job-handling skeleton in order:

1. **Idempotency** check by `msg_id` (same mechanism as jobs).
2. **Session-mode gate**:
   - `INACTIVE` → reject with `SESSION_INACTIVE`.
   - `STRICT` → emit `ApprovalRequest` carrying the tool name and an args preview, wait for
     `ApprovalResponse` (existing `approval_timeout` applies; timeout/deny →
     `APPROVAL_TIMEOUT`/`APPROVAL_DENIED`).
   - `TRUST` / `AUTO_ACCEPT` → pass, **unless** the tool was registered with
     `requires_approval = true`, which forces the `STRICT` approval flow in every mode
     (tighten-only; there is no flag that relaxes gating below the session mode).
3. **Lookup** in the registry → `TOOL_NOT_FOUND` if absent. `args_json` must parse as a JSON
   object → otherwise `INVALID_ARGS` (full schema validation is the handler's responsibility;
   the daemon only guarantees well-formed JSON).
4. **Execute** the handler on a dedicated tokio task with `catch_unwind`-equivalent isolation
   (a panicking handler yields `HANDLER_PANIC`, never kills the daemon), under the clamped
   timeout (`EXECUTION_TIMEOUT`), within an app-tool-specific concurrency limit (default 4,
   independent of `max_concurrent_jobs`; excess → `CONCURRENCY_LIMIT`).
5. **Respond** with `AppToolResponse` (`result_json` or `error`).

### hub: catalog cache, control plane, webhook, audit

- **Catalog cache** (hub-store, memory + Redis): keyed by device id, replaced wholesale on each
  `AppToolsUpdate`. Marked `stale` the moment the device's WS disconnects; cleared of `stale`
  only when a fresh snapshot arrives after reconnect. Queries always return the `stale` flag —
  a stale catalog is readable but not trustworthy.
- **Control plane**:
  - `GET /api/devices/{device_id}/app-tools` → `{ revision, stale, tools: [...] }`.
  - `POST /api/control/app-tool` with `{ device_id, name, args, timeout_ms? }` →
    synchronous await of `AppToolResponse` (aligned with existing control-endpoint timeout
    budget). Device offline → fast-fail `DEVICE_OFFLINE` without waiting for the timeout.
- **Webhook**: new event `device.app_tools.updated { device_id, revision }`, following the
  `device.heartbeat` pattern, so consumers can invalidate caches.
- **Audit**: every invocation attempt (including approval denials and rejections) is written to
  the existing audit log with device id, tool name, caller principal, and outcome code.

### SDK (`@ahandai/sdk` + `@ahandai/proto`)

- Regenerate `@ahandai/proto` from the new proto files; bump both package versions.
- `CloudClient.listAppTools(deviceId)` → `{ revision, stale, tools }`.
- `CloudClient.invokeAppTool(deviceId, name, args, { timeoutMs }?)` → parsed result JSON;
  failures throw a typed error carrying the `AppToolError.code`.

### Error codes

`TOOL_NOT_FOUND`, `INVALID_ARGS`, `SESSION_INACTIVE`, `APPROVAL_DENIED`, `APPROVAL_TIMEOUT`,
`EXECUTION_TIMEOUT`, `HANDLER_PANIC`, `HANDLER_ERROR`, `CONCURRENCY_LIMIT` (daemon-side);
`DEVICE_OFFLINE` (hub-side). When a handler returns `Err(AppToolError)` its `code` passes
through verbatim; `HANDLER_ERROR` is only the fallback when the handler supplies no code. All
errors carry a human-readable, host-neutral remediation hint in `message`, consistent with the
plugin-runtime capability hints.

## Sub-project ②: agent-pi DeviceToolsProvider (Architecture)

Lives in team9-agent-pi as an extension of the existing `AhandIntegration` component
(`packages/claw-hive/src/components/ahand/integration.ts`), which already holds the
`CloudClient` and the session's bound device list (it registers one `AhandBackend` per device).

- On `onInitialize`, register an `IToolProvider` (`id: "device-app-tools"`) into the
  `tool-tier` dependency — same pattern as `Team9CapabilityHubComponent`
  (`packages/team9-components/src/components/team9-capability-hub/component.ts`).
- **Auto-discovery**: for each session-bound online device, call `CloudClient.listAppTools()`
  and register each tool as a `TieredTool` at the **listed** tier (discoverable via
  `search_tools`/`load_tools`, no context bloat).
- **Execution**: `TieredTool.execute()` proxies to `CloudClient.invokeAppTool()`; result JSON
  becomes the tool result. Errors map to `isError` tool results with remediation hints
  (approval denied / timeout / device offline) — never thrown into the agent loop.
- **Naming**: plain tool name when the session has a single device (the common case);
  `{name}@{deviceLabel}` qualification only on collision in multi-device sessions.
- **Session recovery**: provider-sourced tools are recovered by `providerKey` (existing
  mechanism in `agent-tool-component.ts`); `resolve()` must reconstruct tools after recovery,
  same contract as capability-hub.
- **Cache invalidation**: prefer the `device.app_tools.updated` webhook; deployments without
  webhook delivery fall back to a short TTL.
- Device binding comes from the session-level `AhandIntegration` config, **not** the team9 DB,
  so non-team9 consumers (Coffice) work unchanged.

Sub-project ② gets its own spec in team9-agent-pi referencing this document.

## Sub-project ③: Coffice Integration (Architecture)

Coffice has no aHand dependency today, so ③ includes its baseline integration. It mirrors the
team9 pattern: **embed the `ahandd` library in-process** (Tauri Rust side), per user
confirmation.

1. **Desktop (Tauri)**: `ahandd::spawn(DaemonConfig)` at app start;
   `load_or_create_identity` for device identity; default session mode `STRICT`; minimal
   status panel (online state, device id) in settings.
2. **Coffice API**: its own hub service token (Coffice is a separate tenant — it does not
   borrow team9's); device-JWT minting endpoint; device pairs to the user account on login.
3. **Session binding**: when creating an agent run, Coffice API writes the user's online
   device id into the agent-pi session config, which activates
   `AhandIntegration`/`DeviceToolsProvider`.
4. **First tools** (candidates, finalized during ③ planning): `list_workspace_documents`
   (read-only), `read_document` (read-only), `insert_content` (`requires_approval = true`
   showcase). Registered from Tauri Rust via `register_app_tool`, bridging to app state.
5. **Approval UX**: `ApprovalRequest` surfaces as a native Tauri dialog. The exact in-process
   hook into the `approval` module is verified against `dev`-branch code during ③ planning.

Sub-project ③ gets its own spec in the Coffice repo referencing this document.

### Acceptance (definition of done for the whole initiative)

In a Coffice conversation: the agent finds Coffice-registered tools via `search_tools` →
invokes one → `STRICT` mode pops a native approval dialog → the result returns to the
conversation; `insert_content` still pops approval in `TRUST` mode (tighten-only flag works);
every invocation is visible in the hub audit log.

## Error Handling Summary

- **Daemon**: handler panics are isolated (error response, daemon survives); timeouts clamped
  to [1s, 300s] with a 60s default; unknown tool / inactive session / denied approval each map
  to a distinct error code; idempotency via `msg_id`; daemon shutdown mid-call surfaces as a
  hub-side timeout.
- **Hub**: offline device fails fast (`DEVICE_OFFLINE`), no timeout wait; catalog goes `stale`
  on disconnect and recovers only via a fresh snapshot; every attempt (including denials) is
  audited.
- **agent-pi**: all invocation failures become `isError` tool results with hints; recovery
  failures degrade the tool gracefully instead of breaking the session.
- **Coffice**: `ahandd` spawn failure leaves the app fully functional — the agent simply sees
  no device tools, and the settings panel shows why.

## Testing Strategy

- **ahandd (Rust)**: unit tests for registry (validation, duplicates, snapshot/revision),
  gating matrix (4 session modes × `requires_approval` true/false), timeout clamping, panic
  isolation, concurrency limit, idempotency. Proto roundtrip tests for the three new messages.
- **hub**: integration tests — catalog replace, stale-on-disconnect/recover-on-snapshot,
  invoke happy path, offline fast-fail, webhook emission, audit entries.
- **e2e (this repo)**: a test harness embeds `ahandd`, registers a test tool, and drives the
  full loop through the hub control API (extends the existing `e2e/` patterns).
- **Wire compat**: Envelope tags 35–37 only; `cargo check --workspace` after every shared
  proto/type change (per workspace convention).
- **SDK (TS, vitest)**: unit tests for both new `CloudClient` methods (success, timeout,
  offline, approval-denied).
- **agent-pi / Coffice**: per their own specs; agent-pi provider tests follow the existing
  `integration.test.ts` patterns.
- Coverage: 100% on new code where the repo has coverage infrastructure; every sub-project
  ends with a multi-round review loop until no Critical/Important findings remain.

## Non-Goals

- Streaming tool results (single response per invocation this stage).
- Out-of-process registration (IPC socket or manifest-CLI track) — in-process embedding only.
- MCP bridging (tracked separately on the remote-control roadmap; the proto shape here maps
  cleanly onto MCP tool semantics if/when that lands).
- Per-tool authorization tables (which agent/workspace may see which tool) — team9 permission
  granularity stays at the device level this stage.
- Multiple host apps sharing one daemon.

## Risks and Open Questions

- **Stale-catalog race**: if the hub restarts and repopulates from Redis while a device is
  reconnecting, an old catalog could be served as fresh. Mitigation: catalogs persist with the
  connection epoch and are born `stale` after hub restart until the next snapshot.
- **Blocking handlers**: a deadlocked app handler can pin the app-tool concurrency slots until
  timeouts fire; the independent limit keeps `run_command`/file/browser traffic unaffected.
- **Coffice tenancy**: the hub's service-token model currently serves team9 only. If hub auth
  assumes a single service principal anywhere, ③ needs a second-tenant extension — to be
  confirmed early in ③ planning.
- **Open**: exact in-process approval hook for embedded daemons (③); whether tools of an
  offline device stay visible (degraded) or are removed from the agent's listed tier (②).
