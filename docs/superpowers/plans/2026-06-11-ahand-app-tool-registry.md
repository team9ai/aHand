# AHand App Tool Registry Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers-extended-cc:subagent-driven-development (recommended) or superpowers-extended-cc:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let host applications that embed `ahandd` register app-defined tools (definition + async handler) that cloud callers can discover and invoke through ahand-hub.

**Architecture:** New `app_tool.proto` messages ride the existing Envelope (tags 35/36/37). The daemon gains an `app_tool_registry` module + `DaemonHandle::register_app_tool` API; invocation reuses the job skeleton (idempotency → session-mode gate via synthetic `JobRequest` → isolated execution). The hub caches a per-device catalog (Redis + stale flag), exposes `GET .../app-tools` and `POST /api/control/app-tool` (oneshot pending-map like browser), emits a webhook, audits invocations. SDK adds `listAppTools`/`invokeAppTool`.

**Tech Stack:** Rust (prost, tokio, axum, dashmap), TypeScript (ts-proto/buf, vitest), pnpm + turbo + cargo workspace.

**Spec:** `docs/superpowers/specs/2026-06-11-ahand-app-tool-registry-design.md`
**Branch:** `feat/app-tool-registry` (off `origin/dev`), worktree `/Users/winrey/Projects/weightwave/ahand.worktrees/app-tool-registry`

**Conventions that apply to every task:**
- Work in the worktree directory above; never touch the main checkout.
- After ANY change to `proto/` or shared crate types: `cargo check --workspace`.
- Commit after each task with the message given in the task.
- TDD: write the failing test first wherever the task structure allows.

---

### Task 1: Proto messages + Rust generation

**Goal:** Define `AppToolDescriptor`/`AppToolsUpdate`/`AppToolRequest`/`AppToolError`/`AppToolResponse`, wire them into Envelope tags 35/36/37, and prove Rust round-trips.

**Files:**
- Create: `proto/ahand/v1/app_tool.proto`
- Modify: `proto/ahand/v1/envelope.proto` (import block at top; oneof tail after tag 34)
- Modify: `crates/ahand-protocol/build.rs`
- Test: `crates/ahand-protocol/tests/app_tool_roundtrip.rs`

**Acceptance Criteria:**
- [ ] `cargo test -p ahand-protocol` passes including the new roundtrip test
- [ ] `cargo check --workspace` passes (no other crate breaks)
- [ ] Envelope tags 35/36/37 used; tag 32 untouched (see wire-compat comment in envelope.proto)

**Verify:** `cargo test -p ahand-protocol && cargo check --workspace` → all green

**Steps:**

- [ ] **Step 1: Create `proto/ahand/v1/app_tool.proto`**

```proto
syntax = "proto3";

package ahand.v1;

// AppToolDescriptor - one application-defined tool advertised by the host app
// embedding ahandd. Names are flat per device (device <-> app is 1:1).
message AppToolDescriptor {
  string name              = 1; // ^[a-z0-9_-]{1,64}$
  string description       = 2;
  string input_schema_json = 3; // JSON Schema, serialized JSON object
  bool   requires_approval = 4; // tighten-only: forces approval in every session mode
}

// AppToolsUpdate - daemon -> hub. FULL snapshot of registered tools.
// Sent on every register/unregister and re-sent after each Hello handshake.
message AppToolsUpdate {
  uint64 revision = 1; // monotonic per daemon process
  repeated AppToolDescriptor tools = 2;
}

// AppToolRequest - hub -> daemon. Invoke one app tool.
message AppToolRequest {
  string tool_call_id = 1;
  string name         = 2;
  string args_json    = 3; // JSON object matching input_schema_json
  uint32 timeout_ms   = 4; // daemon clamps to [1_000, 300_000]; 0 => 60_000
}

message AppToolError {
  string code    = 1; // TOOL_NOT_FOUND | INVALID_ARGS | SESSION_INACTIVE | APPROVAL_DENIED
                      // | APPROVAL_TIMEOUT | EXECUTION_TIMEOUT | HANDLER_PANIC
                      // | HANDLER_ERROR | CONCURRENCY_LIMIT
  string message = 2; // human-readable, host-neutral remediation hint
}

// AppToolResponse - daemon -> hub. Single response per invocation (no streaming).
message AppToolResponse {
  string tool_call_id = 1;
  oneof result {
    string       result_json = 2;
    AppToolError error       = 3;
  }
}
```

- [ ] **Step 2: Wire into `proto/ahand/v1/envelope.proto`**

Add to the import block (after `import "ahand/v1/file_ops.proto";`):

```proto
import "ahand/v1/app_tool.proto";
```

Append inside `oneof payload`, directly after `FileResponse file_response = 34;` (do NOT touch the tag-32 comment):

```proto
    AppToolsUpdate   app_tools_update  = 35;
    AppToolRequest   app_tool_request  = 36;
    AppToolResponse  app_tool_response = 37;
```

- [ ] **Step 3: Update `crates/ahand-protocol/build.rs`**

Add after the file_ops rerun line:

```rust
    println!("cargo:rerun-if-changed=../../proto/ahand/v1/app_tool.proto");
```

Add `"../../proto/ahand/v1/app_tool.proto",` to the `compile_protos` array (after file_ops.proto). The `lib.rs` glob re-export (`pub use ahand::v1::*;`) picks up the new types automatically — no lib.rs change.

- [ ] **Step 4: Write the roundtrip test `crates/ahand-protocol/tests/app_tool_roundtrip.rs`**

```rust
//! Wire round-trip tests for app tool messages (Envelope tags 35/36/37).

use ahand_protocol::{
    envelope, AppToolDescriptor, AppToolError, AppToolRequest, AppToolResponse, AppToolsUpdate,
    Envelope,
};
use prost::Message;

fn roundtrip(payload: envelope::Payload) -> envelope::Payload {
    let env = Envelope {
        device_id: "dev-1".into(),
        msg_id: "m-1".into(),
        payload: Some(payload),
        ..Default::default()
    };
    let bytes = env.encode_to_vec();
    Envelope::decode(bytes.as_slice()).unwrap().payload.unwrap()
}

#[test]
fn app_tools_update_roundtrips() {
    let update = AppToolsUpdate {
        revision: 7,
        tools: vec![AppToolDescriptor {
            name: "list_documents".into(),
            description: "List open documents".into(),
            input_schema_json: r#"{"type":"object","properties":{}}"#.into(),
            requires_approval: true,
        }],
    };
    match roundtrip(envelope::Payload::AppToolsUpdate(update.clone())) {
        envelope::Payload::AppToolsUpdate(got) => assert_eq!(got, update),
        other => panic!("wrong payload variant: {other:?}"),
    }
}

#[test]
fn app_tool_request_roundtrips() {
    let req = AppToolRequest {
        tool_call_id: "call-1".into(),
        name: "list_documents".into(),
        args_json: r#"{"limit":5}"#.into(),
        timeout_ms: 30_000,
    };
    match roundtrip(envelope::Payload::AppToolRequest(req.clone())) {
        envelope::Payload::AppToolRequest(got) => assert_eq!(got, req),
        other => panic!("wrong payload variant: {other:?}"),
    }
}

#[test]
fn app_tool_response_error_roundtrips() {
    let resp = AppToolResponse {
        tool_call_id: "call-1".into(),
        result: Some(ahand_protocol::app_tool_response::Result::Error(AppToolError {
            code: "TOOL_NOT_FOUND".into(),
            message: "no such tool".into(),
        })),
    };
    match roundtrip(envelope::Payload::AppToolResponse(resp.clone())) {
        envelope::Payload::AppToolResponse(got) => assert_eq!(got, resp),
        other => panic!("wrong payload variant: {other:?}"),
    }
}
```

- [ ] **Step 5: Run and verify**

Run: `cargo test -p ahand-protocol && cargo check --workspace`
Expected: all tests pass; workspace compiles.

- [ ] **Step 6: Commit**

```bash
git add proto/ crates/ahand-protocol/
git commit -m "feat(proto): add app tool messages (Envelope tags 35-37)"
```

---

### Task 2: TypeScript proto generation + exports + changelog

**Goal:** Regenerate `@ahandai/proto` with the new messages and publish-prep version 0.3.0.

**Files:**
- Modify: `packages/proto-ts/src/index.ts`
- Modify: `packages/proto-ts/package.json` (version)
- Modify: `packages/proto-ts/CHANGELOG.md`
- Generated: `packages/proto-ts/src/generated/ahand/v1/app_tool.ts` (+ regenerated envelope.ts)

**Acceptance Criteria:**
- [ ] `pnpm --filter @ahandai/proto build` and `lint` pass
- [ ] `AppToolsUpdate`/`AppToolRequest`/`AppToolResponse`/`AppToolDescriptor`/`AppToolError` importable from package root

**Verify:** `pnpm --filter @ahandai/proto build && pnpm --filter @ahandai/proto lint` → exit 0

**Steps:**

- [ ] **Step 1: Generate**

Run: `cd packages/proto-ts && pnpm generate`
Expected: `src/generated/ahand/v1/app_tool.ts` created; `envelope.ts` regenerated with the three new oneof cases.

- [ ] **Step 2: Re-export in `packages/proto-ts/src/index.ts`** (follow the existing file_ops export block style):

```typescript
export {
  AppToolDescriptor,
  AppToolError,
  AppToolRequest,
  AppToolResponse,
  AppToolsUpdate,
} from "./generated/ahand/v1/app_tool.js";
```

- [ ] **Step 3: Version + changelog**

Set `"version": "0.3.0"` in `packages/proto-ts/package.json`. Prepend to `packages/proto-ts/CHANGELOG.md` (match existing format):

```markdown
## 0.3.0 — 2026-06-11

### Added

- **`AppToolsUpdate` / `AppToolRequest` / `AppToolResponse`** wire types
  (Envelope tags 35–37) for application-defined tools registered by host
  apps embedding `ahandd`. Includes `AppToolDescriptor` (name, description,
  JSON Schema, `requires_approval`) and `AppToolError`.
```

- [ ] **Step 4: Verify and commit**

Run: `pnpm --filter @ahandai/proto build && pnpm --filter @ahandai/proto lint`

```bash
git add packages/proto-ts/
git commit -m "feat(proto-ts): generate app tool types, bump to 0.3.0"
```

---

### Task 3: ahandd — `app_tool_registry` core module

**Goal:** A self-contained registry holding tool defs + handlers with validation, snapshot/revision, change notification, concurrency permits, and a completed-call cache — fully unit-tested, no daemon wiring yet.

**Files:**
- Create: `crates/ahandd/src/app_tool_registry/mod.rs`
- Modify: `crates/ahandd/src/lib.rs` (add `pub mod app_tool_registry;` to the module list)

**Acceptance Criteria:**
- [ ] Name validation enforces `^[a-z0-9_-]{1,64}$` (no regex crate — char check)
- [ ] `input_schema` must be a JSON object; duplicates rejected; unregister returns whether it existed
- [ ] Every mutation bumps `revision` and notifies a `tokio::sync::watch` channel
- [ ] `snapshot()` returns an `ahand_protocol::AppToolsUpdate` with current revision + descriptors
- [ ] Completed-call cache mirrors `JobRegistry`'s bounded `VecDeque` pattern (`crates/ahandd/src/registry.rs:117-142`)

**Verify:** `cargo test -p ahandd app_tool_registry` → all unit tests pass

**Steps:**

- [ ] **Step 1: Write the module with tests** (`crates/ahandd/src/app_tool_registry/mod.rs`)

```rust
//! Application-defined tool registry. Host apps embedding ahandd register
//! tools (definition + async handler); the daemon advertises full snapshots
//! to the hub and executes invocations under session-mode gating.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use ahand_protocol::{AppToolDescriptor, AppToolsUpdate};
use futures_util::future::BoxFuture;
use tokio::sync::{watch, Mutex, OwnedSemaphorePermit, Semaphore};

pub const DEFAULT_TIMEOUT_MS: u32 = 60_000;
pub const MIN_TIMEOUT_MS: u32 = 1_000;
pub const MAX_TIMEOUT_MS: u32 = 300_000;
const MAX_CONCURRENT_APP_TOOLS: usize = 4;
const MAX_COMPLETED_CALLS: usize = 256;

#[derive(Debug, Clone)]
pub struct AppToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub requires_approval: bool,
}

#[derive(Debug, Clone)]
pub struct AppToolError {
    pub code: String,
    pub message: String,
}

pub type AppToolHandler = Arc<
    dyn Fn(serde_json::Value) -> BoxFuture<'static, Result<serde_json::Value, AppToolError>>
        + Send
        + Sync,
>;

#[derive(Debug, Clone)]
pub struct CompletedAppToolCall {
    pub result_json: Option<String>,
    pub error: Option<(String, String)>, // (code, message)
}

pub enum CallState {
    Running,
    Completed(CompletedAppToolCall),
    Unknown,
}

struct Registered {
    descriptor: AppToolDescriptor,
    handler: AppToolHandler,
}

pub struct AppToolRegistry {
    tools: Mutex<HashMap<String, Registered>>,
    revision: Mutex<u64>,
    revision_tx: watch::Sender<u64>,
    semaphore: Arc<Semaphore>,
    running: Mutex<std::collections::HashSet<String>>,
    completed: Mutex<VecDeque<(String, CompletedAppToolCall)>>,
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

impl AppToolRegistry {
    pub fn new() -> Self {
        let (revision_tx, _) = watch::channel(0u64);
        Self {
            tools: Mutex::new(HashMap::new()),
            revision: Mutex::new(0),
            revision_tx,
            semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_APP_TOOLS)),
            running: Mutex::new(std::collections::HashSet::new()),
            completed: Mutex::new(VecDeque::new()),
        }
    }

    pub fn subscribe_revision(&self) -> watch::Receiver<u64> {
        self.revision_tx.subscribe()
    }

    pub async fn register(&self, def: AppToolDef, handler: AppToolHandler) -> anyhow::Result<()> {
        if !valid_name(&def.name) {
            anyhow::bail!("invalid tool name {:?}: must match ^[a-z0-9_-]{{1,64}}$", def.name);
        }
        if !def.input_schema.is_object() {
            anyhow::bail!("input_schema for {:?} must be a JSON object", def.name);
        }
        let mut tools = self.tools.lock().await;
        if tools.contains_key(&def.name) {
            anyhow::bail!("tool {:?} already registered", def.name);
        }
        tools.insert(
            def.name.clone(),
            Registered {
                descriptor: AppToolDescriptor {
                    name: def.name,
                    description: def.description,
                    input_schema_json: def.input_schema.to_string(),
                    requires_approval: def.requires_approval,
                },
                handler,
            },
        );
        drop(tools);
        self.bump_revision().await;
        Ok(())
    }

    pub async fn unregister(&self, name: &str) -> bool {
        let existed = self.tools.lock().await.remove(name).is_some();
        if existed {
            self.bump_revision().await;
        }
        existed
    }

    async fn bump_revision(&self) {
        let mut rev = self.revision.lock().await;
        *rev += 1;
        let _ = self.revision_tx.send(*rev);
    }

    pub async fn snapshot(&self) -> AppToolsUpdate {
        let tools = self.tools.lock().await;
        let mut descriptors: Vec<AppToolDescriptor> =
            tools.values().map(|r| r.descriptor.clone()).collect();
        descriptors.sort_by(|a, b| a.name.cmp(&b.name));
        AppToolsUpdate { revision: *self.revision.lock().await, tools: descriptors }
    }

    /// Returns (descriptor, handler) for dispatch.
    pub async fn lookup(&self, name: &str) -> Option<(AppToolDescriptor, AppToolHandler)> {
        self.tools
            .lock()
            .await
            .get(name)
            .map(|r| (r.descriptor.clone(), Arc::clone(&r.handler)))
    }

    pub async fn acquire_permit(&self) -> Option<OwnedSemaphorePermit> {
        // try_acquire: excess invocations fail fast with CONCURRENCY_LIMIT
        // rather than queueing into their own timeout.
        Arc::clone(&self.semaphore).try_acquire_owned().ok()
    }
}
```

- [ ] **Step 2: Add the idempotency cache methods** (same impl block):

```rust
impl AppToolRegistry {
    pub async fn call_state(&self, tool_call_id: &str) -> CallState {
        if self.running.lock().await.contains(tool_call_id) {
            return CallState::Running;
        }
        let completed = self.completed.lock().await;
        for (id, result) in completed.iter() {
            if id == tool_call_id {
                return CallState::Completed(result.clone());
            }
        }
        CallState::Unknown
    }

    pub async fn mark_running(&self, tool_call_id: &str) {
        self.running.lock().await.insert(tool_call_id.to_string());
    }

    pub async fn mark_completed(&self, tool_call_id: String, result: CompletedAppToolCall) {
        self.running.lock().await.remove(&tool_call_id);
        let mut completed = self.completed.lock().await;
        completed.push_back((tool_call_id, result));
        while completed.len() > MAX_COMPLETED_CALLS {
            completed.pop_front();
        }
    }

    pub fn clamp_timeout(timeout_ms: u32) -> u32 {
        if timeout_ms == 0 {
            DEFAULT_TIMEOUT_MS
        } else {
            timeout_ms.clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS)
        }
    }
}
```

- [ ] **Step 3: Unit tests** (in-module `#[cfg(test)] mod tests`, the repo's standard pattern). Cover, at minimum:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn noop_handler() -> AppToolHandler {
        Arc::new(|_args| Box::pin(async { Ok(serde_json::json!({"ok": true})) }))
    }

    fn def(name: &str) -> AppToolDef {
        AppToolDef {
            name: name.into(),
            description: "d".into(),
            input_schema: serde_json::json!({"type": "object"}),
            requires_approval: false,
        }
    }

    #[tokio::test]
    async fn register_validates_names() {
        let reg = AppToolRegistry::new();
        for bad in ["", "UPPER", "has space", "emoji✨", &"x".repeat(65)] {
            assert!(reg.register(def(bad), noop_handler()).await.is_err(), "{bad:?}");
        }
        assert!(reg.register(def("ok_tool-1"), noop_handler()).await.is_ok());
    }

    #[tokio::test]
    async fn schema_must_be_object() {
        let reg = AppToolRegistry::new();
        let mut d = def("t");
        d.input_schema = serde_json::json!(["not", "an", "object"]);
        assert!(reg.register(d, noop_handler()).await.is_err());
    }

    #[tokio::test]
    async fn duplicate_rejected_and_unregister_roundtrip() {
        let reg = AppToolRegistry::new();
        reg.register(def("t"), noop_handler()).await.unwrap();
        assert!(reg.register(def("t"), noop_handler()).await.is_err());
        assert!(reg.unregister("t").await);
        assert!(!reg.unregister("t").await);
        assert!(reg.register(def("t"), noop_handler()).await.is_ok());
    }

    #[tokio::test]
    async fn snapshot_and_revision_track_mutations() {
        let reg = AppToolRegistry::new();
        let mut rx = reg.subscribe_revision();
        reg.register(def("b"), noop_handler()).await.unwrap();
        reg.register(def("a"), noop_handler()).await.unwrap();
        let snap = reg.snapshot().await;
        assert_eq!(snap.revision, 2);
        assert_eq!(snap.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), 2);
        reg.unregister("a").await;
        assert_eq!(reg.snapshot().await.revision, 3);
    }

    #[tokio::test]
    async fn idempotency_cache_and_permits() {
        let reg = AppToolRegistry::new();
        assert!(matches!(reg.call_state("c1").await, CallState::Unknown));
        reg.mark_running("c1").await;
        assert!(matches!(reg.call_state("c1").await, CallState::Running));
        reg.mark_completed("c1".into(), CompletedAppToolCall { result_json: Some("{}".into()), error: None }).await;
        assert!(matches!(reg.call_state("c1").await, CallState::Completed(_)));

        let p1 = reg.acquire_permit().await; // 4 permits available
        let _p2 = reg.acquire_permit().await;
        let _p3 = reg.acquire_permit().await;
        let _p4 = reg.acquire_permit().await;
        assert!(reg.acquire_permit().await.is_none(), "5th permit must fail fast");
        drop(p1);
        assert!(reg.acquire_permit().await.is_some());
    }

    #[test]
    fn timeout_clamping() {
        assert_eq!(AppToolRegistry::clamp_timeout(0), 60_000);
        assert_eq!(AppToolRegistry::clamp_timeout(500), 1_000);
        assert_eq!(AppToolRegistry::clamp_timeout(400_000), 300_000);
        assert_eq!(AppToolRegistry::clamp_timeout(30_000), 30_000);
    }
}
```

- [ ] **Step 4: Register the module** — add `pub mod app_tool_registry;` to `crates/ahandd/src/lib.rs` (alphabetical position, after `pub mod ahand_client;` / `pub mod approval;`). Check `futures_util` is already a dependency of ahandd (it is used by ahand_client); if the `BoxFuture` import fails, add `futures-util.workspace = true` to `crates/ahandd/Cargo.toml`.

- [ ] **Step 5: Run and commit**

Run: `cargo test -p ahandd app_tool_registry`
Expected: all new unit tests pass.

```bash
git add crates/ahandd/
git commit -m "feat(ahandd): add app tool registry core (validation, snapshot, permits)"
```

---

### Task 4: ahandd — DaemonHandle API + snapshot advertising

**Goal:** Apps call `handle.register_app_tool(...)`; the daemon pushes an `AppToolsUpdate` snapshot on every change and re-sends it after each Hello handshake.

**Files:**
- Modify: `crates/ahandd/src/public_api.rs` (DaemonHandle struct ~line 203, spawn() ~line 275)
- Modify: `crates/ahandd/src/ahand_client.rs` (run_with_reporter params; post-HelloAccepted hook ~line 385; watcher task)
- Modify: `crates/ahandd/src/main.rs` (CLI daemon passes a fresh registry — same param ripple as c9b8a4d did for FileManager)
- Test: `crates/ahandd/tests/app_tools.rs` (new, uses `tests/mock_hub/mod.rs` harness)
- Modify (if needed): `crates/ahandd/tests/mock_hub/mod.rs` (helper to receive/decode AppToolsUpdate)

**Acceptance Criteria:**
- [ ] `register_app_tool` before AND after connection both result in the hub receiving the correct snapshot
- [ ] Reconnect (drop WS from mock hub) → snapshot re-sent after new Hello
- [ ] `unregister_app_tool` pushes a snapshot without the tool, revision incremented
- [ ] CLI daemon (`main.rs`) still builds and runs (empty registry)

**Verify:** `cargo test -p ahandd --test app_tools && cargo check --workspace`

**Steps:**

- [ ] **Step 1: Wire registry into spawn() and DaemonHandle** (`public_api.rs`)

Add field + methods (re-export types for embedders):

```rust
pub use crate::app_tool_registry::{AppToolDef, AppToolError, AppToolHandler};

pub struct DaemonHandle {
    shutdown_tx: Option<oneshot::Sender<()>>,
    join: JoinHandle<anyhow::Result<()>>,
    status_rx: watch::Receiver<DaemonStatus>,
    device_id: String,
    app_tools: Arc<crate::app_tool_registry::AppToolRegistry>, // NEW
}

impl DaemonHandle {
    pub async fn register_app_tool(
        &self,
        def: AppToolDef,
        handler: AppToolHandler,
    ) -> anyhow::Result<()> {
        self.app_tools.register(def, handler).await
    }

    pub async fn unregister_app_tool(&self, name: &str) -> bool {
        self.app_tools.unregister(name).await
    }
}
```

In `spawn()` (next to the other manager constructions around lines 298-313): create `let app_tools = Arc::new(AppToolRegistry::new());`, pass `Arc::clone(&app_tools)` into `run_with_reporter`, and store it on the returned `DaemonHandle`.

- [ ] **Step 2: Thread the registry through `run_with_reporter`** (`ahand_client.rs`)

Add `app_tools: Arc<AppToolRegistry>` to the parameter list (same ripple pattern as `file_mgr`). Update ALL call sites: `public_api.rs` and `main.rs` (CLI constructs `Arc::new(AppToolRegistry::new())`).

- [ ] **Step 3: Send snapshot after HelloAccepted + watch for changes**

After the unacked-replay block (~line 368) and the update_suggestion check (~line 385), add:

```rust
// Advertise app tools: initial snapshot after every successful handshake,
// then push a fresh snapshot whenever the registry changes.
send_app_tools_snapshot(&tx, device_id, &app_tools).await;
```

And spawn alongside the other connection-scoped tasks (shut down via the existing `close_rx` pattern):

```rust
let watcher_app_tools = Arc::clone(&app_tools);
let watcher_tx = tx.clone();
let watcher_device_id = device_id.to_string();
let mut watcher_close_rx = close_rx.clone();
let mut revision_rx = app_tools.subscribe_revision();
tokio::spawn(async move {
    loop {
        tokio::select! {
            _ = watcher_close_rx.changed() => break,
            changed = revision_rx.changed() => {
                if changed.is_err() { break; }
                send_app_tools_snapshot(&watcher_tx, &watcher_device_id, &watcher_app_tools).await;
            }
        }
    }
});
```

With the helper (place near the other envelope-builder helpers):

```rust
async fn send_app_tools_snapshot<T: crate::executor::EnvelopeSink>(
    tx: &T,
    device_id: &str,
    app_tools: &Arc<AppToolRegistry>,
) {
    let snapshot = app_tools.snapshot().await;
    let env = Envelope {
        device_id: device_id.to_string(),
        msg_id: new_msg_id(),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::AppToolsUpdate(snapshot)),
        ..Default::default()
    };
    let _ = tx.send(env);
}
```

(`tx` is the same outbound sink JobEvent/JobRejected use, so messages get outbox seq stamping for free. Match the exact sink type used by the surrounding code — if the watcher task cannot take `T: EnvelopeSink` generically, use the concrete mpsc sender type the file already uses for spawned tasks, e.g. what the approval-wait task captures.)

- [ ] **Step 4: Integration test** (`crates/ahandd/tests/app_tools.rs`)

Follow `lib_spawn.rs` setup (mock_hub::start_accepting, DaemonConfig builder, spawn). Add a mock-hub helper `recv_app_tools_update()` that reads binary frames until it decodes an Envelope whose payload is AppToolsUpdate, returning it. Test cases:

```rust
#[tokio::test]
async fn snapshot_sent_after_hello_and_on_register() {
    // spawn daemon against mock hub; expect initial empty snapshot (revision 0)
    // register tool "demo_echo" -> expect snapshot revision 1 containing it
}

#[tokio::test]
async fn unregister_pushes_snapshot_without_tool() { /* revision 2, empty list */ }

#[tokio::test]
async fn snapshot_resent_after_reconnect() {
    // register tool; force mock hub to drop the WS; daemon reconnects;
    // expect snapshot with same revision re-sent after new Hello
}
```

(Write the bodies with real harness calls — the existing helpers in `mock_hub/mod.rs` show how `start_accepting()`, `.ws_url()`, `.valid_jwt()` and frame reads work; extend the module rather than duplicating frame-decode logic.)

- [ ] **Step 5: Run and commit**

Run: `cargo test -p ahandd --test app_tools && cargo check --workspace`

```bash
git add crates/ahandd/
git commit -m "feat(ahandd): register_app_tool API + AppToolsUpdate advertising"
```

---

### Task 5: ahandd — AppToolRequest dispatch and isolated execution

**Goal:** Daemon handles `AppToolRequest`: idempotency, lookup, args validation, clamped timeout, panic isolation, fail-fast concurrency — and answers with `AppToolResponse`. (Session-mode gating lands in Task 6; this task executes directly so each concern is testable in isolation.)

**Files:**
- Modify: `crates/ahandd/src/ahand_client.rs` (new match arm after FileRequest ~line 587; new handler fn)
- Test: extend `crates/ahandd/tests/app_tools.rs`
- Modify: `crates/ahandd/tests/mock_hub/mod.rs` (helpers: `send_app_tool_request()`, `recv_app_tool_response()`)

**Acceptance Criteria:**
- [ ] Happy path: hub sends AppToolRequest → handler runs → AppToolResponse with result_json
- [ ] Unknown tool → `TOOL_NOT_FOUND`; non-object args_json → `INVALID_ARGS`
- [ ] Handler sleeping past clamped timeout → `EXECUTION_TIMEOUT`; panicking handler → `HANDLER_PANIC` (daemon stays alive — a follow-up call still works)
- [ ] 5 concurrent slow calls → 5th gets `CONCURRENCY_LIMIT` immediately
- [ ] Duplicate tool_call_id while running → ignored; after completion → cached response re-sent

**Verify:** `cargo test -p ahandd --test app_tools`

**Steps:**

- [ ] **Step 1: Add the dispatch arm** (after the FileRequest arm in the payload match):

```rust
Some(envelope::Payload::AppToolRequest(req)) => {
    handle_app_tool_request(
        device_id, caller_uid, req, &tx,
        session_mgr, approval_mgr, approval_broadcast_tx, app_tools,
    )
    .await;
}
```

- [ ] **Step 2: Implement the handler** (signature mirrors `handle_job_request`; in this task the session/approval params are accepted but unused — Task 6 fills them in):

```rust
fn app_tool_error_envelope(
    device_id: &str,
    tool_call_id: &str,
    code: &str,
    message: String,
) -> Envelope {
    Envelope {
        device_id: device_id.to_string(),
        msg_id: new_msg_id(),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::AppToolResponse(AppToolResponse {
            tool_call_id: tool_call_id.to_string(),
            result: Some(app_tool_response::Result::Error(AppToolError {
                code: code.to_string(),
                message,
            })),
        })),
        ..Default::default()
    }
}

async fn handle_app_tool_request<T>(
    device_id: &str,
    caller_uid: &str,
    req: ahand_protocol::AppToolRequest,
    tx: &T,
    session_mgr: &Arc<SessionManager>,
    approval_mgr: &Arc<ApprovalManager>,
    approval_broadcast_tx: &broadcast::Sender<Envelope>,
    app_tools: &Arc<AppToolRegistry>,
) where
    T: crate::executor::EnvelopeSink,
{
    // 1. Idempotency by tool_call_id (same semantics as JobRegistry::is_known).
    match app_tools.call_state(&req.tool_call_id).await {
        CallState::Running => {
            warn!(tool_call_id = %req.tool_call_id, "duplicate app tool call ignored (running)");
            return;
        }
        CallState::Completed(cached) => {
            let payload = match cached.error {
                Some((code, message)) => app_tool_error_envelope(device_id, &req.tool_call_id, &code, message),
                None => app_tool_result_envelope(device_id, &req.tool_call_id, cached.result_json.unwrap_or_default()),
            };
            let _ = tx.send(payload);
            return;
        }
        CallState::Unknown => {}
    }

    // 2. Session-mode gate — Task 6 inserts gating here.

    // 3. Lookup + args validation.
    let Some((_descriptor, handler)) = app_tools.lookup(&req.name).await else {
        let _ = tx.send(app_tool_error_envelope(
            device_id, &req.tool_call_id, "TOOL_NOT_FOUND",
            format!("no app tool named {:?} is registered on this device", req.name),
        ));
        return;
    };
    let args: serde_json::Value = match serde_json::from_str(&req.args_json) {
        Ok(v @ serde_json::Value::Object(_)) => v,
        Ok(_) => {
            let _ = tx.send(app_tool_error_envelope(
                device_id, &req.tool_call_id, "INVALID_ARGS",
                "args_json must be a JSON object".to_string(),
            ));
            return;
        }
        Err(err) => {
            let _ = tx.send(app_tool_error_envelope(
                device_id, &req.tool_call_id, "INVALID_ARGS",
                format!("args_json is not valid JSON: {err}"),
            ));
            return;
        }
    };

    // 4. Fail-fast concurrency.
    let Some(permit) = app_tools.acquire_permit().await else {
        let _ = tx.send(app_tool_error_envelope(
            device_id, &req.tool_call_id, "CONCURRENCY_LIMIT",
            "too many app tool calls in flight on this device; retry shortly".to_string(),
        ));
        return;
    };

    execute_app_tool(device_id, req, handler, args, permit, tx, app_tools).await;
}
```

- [ ] **Step 3: Isolated execution** (spawned task = panic isolation via JoinHandle; clamped timeout):

```rust
async fn execute_app_tool<T>(
    device_id: &str,
    req: ahand_protocol::AppToolRequest,
    handler: AppToolHandler,
    args: serde_json::Value,
    permit: tokio::sync::OwnedSemaphorePermit,
    tx: &T,
    app_tools: &Arc<AppToolRegistry>,
) where
    T: crate::executor::EnvelopeSink,
{
    app_tools.mark_running(&req.tool_call_id).await;
    let timeout = Duration::from_millis(AppToolRegistry::clamp_timeout(req.timeout_ms) as u64);
    let join = tokio::spawn(async move {
        let _permit = permit; // held for the call's lifetime
        handler(args).await
    });

    let (result_json, error) = match tokio::time::timeout(timeout, join).await {
        Ok(Ok(Ok(value))) => (Some(value.to_string()), None),
        Ok(Ok(Err(app_err))) => {
            let code = if app_err.code.is_empty() { "HANDLER_ERROR".to_string() } else { app_err.code };
            (None, Some((code, app_err.message)))
        }
        Ok(Err(join_err)) if join_err.is_panic() => (
            None,
            Some(("HANDLER_PANIC".to_string(), "app tool handler panicked; the app may be in a bad state".to_string())),
        ),
        Ok(Err(_)) => (None, Some(("HANDLER_ERROR".to_string(), "app tool task was cancelled".to_string()))),
        Err(_elapsed) => (
            None,
            Some(("EXECUTION_TIMEOUT".to_string(), format!("app tool did not finish within {}ms", timeout.as_millis()))),
        ),
    };

    let cached = CompletedAppToolCall { result_json: result_json.clone(), error: error.clone() };
    app_tools.mark_completed(req.tool_call_id.clone(), cached).await;

    let env = match error {
        Some((code, message)) => app_tool_error_envelope(device_id, &req.tool_call_id, &code, message),
        None => app_tool_result_envelope(device_id, &req.tool_call_id, result_json.unwrap_or_default()),
    };
    let _ = tx.send(env);
}
```

`app_tool_result_envelope` mirrors `app_tool_error_envelope` with `Result::ResultJson(result_json)`. Note: on timeout the spawned task keeps running to completion in the background but its permit is released only when it finishes — acceptable for this stage; do NOT abort() it (the handler may hold app state locks).

- [ ] **Step 4: Integration tests** (extend `tests/app_tools.rs`; daemon config uses `session_mode(SessionMode::AutoAccept)` so gating — not yet wired — stays out of the way). Cases per acceptance criteria: happy path, TOOL_NOT_FOUND, INVALID_ARGS (bad JSON + non-object), EXECUTION_TIMEOUT (handler sleeps 3s, timeout_ms=1000), HANDLER_PANIC then follow-up success, CONCURRENCY_LIMIT (4 slow + 1), idempotent replay (same tool_call_id after completion → identical cached response).

- [ ] **Step 5: Run and commit**

Run: `cargo test -p ahandd --test app_tools && cargo check --workspace`

```bash
git add crates/ahandd/
git commit -m "feat(ahandd): dispatch and execute AppToolRequest with isolation"
```

---

### Task 6: ahandd — session-mode gating + tighten-only `requires_approval`

**Goal:** AppToolRequest passes the same gate as jobs: INACTIVE rejects, STRICT requires approval, TRUST/AUTO_ACCEPT pass — except tools registered with `requires_approval=true`, which require approval in every mode. Zero changes to `session.rs`/`approval.rs`: a synthetic `JobRequest` rides the existing machinery.

**Files:**
- Modify: `crates/ahandd/src/ahand_client.rs` (fill the gate in `handle_app_tool_request`)
- Test: extend `crates/ahandd/tests/app_tools.rs`

**Acceptance Criteria (the full matrix):**
- [ ] INACTIVE → `SESSION_INACTIVE` error, no ApprovalRequest, handler never runs
- [ ] STRICT → ApprovalRequest emitted (job_id = tool_call_id, tool = `app:{name}`); approve → result; deny → `APPROVAL_DENIED`; no response → `APPROVAL_TIMEOUT` after `approval_timeout`
- [ ] TRUST and AUTO_ACCEPT with `requires_approval=false` → direct execution, no ApprovalRequest
- [ ] TRUST and AUTO_ACCEPT with `requires_approval=true` → ApprovalRequest required (tighten-only)

**Verify:** `cargo test -p ahandd --test app_tools` → matrix tests pass

**Steps:**

- [ ] **Step 1: Insert the gate** at the "Task 6 inserts gating here" marker, BEFORE lookup. Build the synthetic JobRequest and reuse `SessionManager::check` (`crates/ahandd/src/session.rs:83`):

```rust
// Synthetic JobRequest: lets SessionManager + ApprovalManager handle app
// tools without modification. job_id == tool_call_id so ApprovalResponse
// (keyed by job_id) resolves our pending entry; tool is namespaced "app:".
let mut args_preview = req.args_json.clone();
args_preview.truncate(512);
let synthetic = JobRequest {
    job_id: req.tool_call_id.clone(),
    tool: format!("app:{}", req.name),
    args: vec![args_preview],
    ..Default::default()
};

let requires_approval = app_tools
    .lookup(&req.name)
    .await
    .map(|(d, _)| d.requires_approval)
    .unwrap_or(false); // unknown tools fall through to TOOL_NOT_FOUND below

let decision = match session_mgr.check(&synthetic, caller_uid).await {
    SessionDecision::Deny(reason) => {
        let _ = tx.send(app_tool_error_envelope(
            device_id, &req.tool_call_id, "SESSION_INACTIVE",
            format!("session mode rejects app tool calls: {reason}"),
        ));
        return;
    }
    SessionDecision::Allow if requires_approval => {
        // Tighten-only escalation: TRUST/AUTO_ACCEPT still require approval.
        SessionDecision::NeedsApproval {
            reason: format!("app tool {:?} is registered with requires_approval", req.name),
            previous_refusals: session_mgr.get_refusals(&synthetic.tool).await,
        }
    }
    other => other,
};

if let SessionDecision::NeedsApproval { reason, previous_refusals } = decision {
    let (approval_req, approval_rx) = approval_mgr
        .submit(synthetic.clone(), caller_uid, reason, previous_refusals)
        .await;
    // Send + broadcast the ApprovalRequest exactly like handle_job_request
    // does (ahand_client.rs ~lines 1034-1044):
    let approval_env = Envelope {
        device_id: device_id.to_string(),
        msg_id: new_msg_id(),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::ApprovalRequest(approval_req)),
        ..Default::default()
    };
    let _ = tx.send(approval_env.clone());
    let _ = approval_broadcast_tx.send(approval_env);

    match tokio::time::timeout(approval_mgr.default_timeout(), approval_rx).await {
        Ok(Ok(resp)) if resp.approved => { /* fall through to lookup + execute */ }
        Ok(Ok(resp)) => {
            session_mgr.record_refusal(caller_uid, &synthetic.tool, &resp.reason).await;
            let _ = tx.send(app_tool_error_envelope(
                device_id, &req.tool_call_id, "APPROVAL_DENIED",
                format!("the user declined this call: {}", resp.reason),
            ));
            return;
        }
        _ => {
            approval_mgr.expire(&req.tool_call_id).await;
            let _ = tx.send(app_tool_error_envelope(
                device_id, &req.tool_call_id, "APPROVAL_TIMEOUT",
                "approval request expired without a user response".to_string(),
            ));
            return;
        }
    }
}
```

Implementation notes: (a) unlike the job path, await the approval inline rather than spawning a wait-task — the handler already runs per-message and execution follows immediately; if the surrounding read-loop must not block (check how `handle_file_request` handles long waits with `close_rx`), wrap the whole gate+execute in a `tokio::spawn` the way the job approval wait-task does. (b) `approval_mgr.default_timeout()` — if no such getter exists, add a one-line accessor to `ApprovalManager` (the field exists: `approval.rs:20`). (c) Match the exact `SessionDecision` variant shapes from `session.rs:9`.

- [ ] **Step 2: Matrix tests** (extend `tests/app_tools.rs`). The mock hub side: receive the ApprovalRequest envelope, reply with an ApprovalResponse envelope (approved / denied), or stay silent for the timeout case. Configure short `approval_timeout` via `DaemonConfig::builder(...).approval_timeout(Duration::from_millis(300))`. Eight cases per the acceptance matrix. Assert ApprovalRequest fields: `job_id == tool_call_id`, `tool == "app:demo_echo"`.

- [ ] **Step 3: Run and commit**

Run: `cargo test -p ahandd --test app_tools && cargo check --workspace`

```bash
git add crates/ahandd/
git commit -m "feat(ahandd): session-mode gate + tighten-only approval for app tools"
```

---

### Task 7: hub — catalog store + inbound `AppToolsUpdate`

**Goal:** Hub persists each device's tool catalog (Redis-backed, explicit `stale` flag) and updates it from inbound `AppToolsUpdate`, with revision ordering and stale-on-disconnect semantics.

**Files:**
- Create: `crates/ahand-hub-store/src/app_tool_store.rs` (follow `presence_store.rs` structure)
- Modify: `crates/ahand-hub-store/src/lib.rs` (export module)
- Modify: `crates/ahand-hub/src/state.rs` (AppState gains the store handle)
- Modify: `crates/ahand-hub/src/ws/device_gateway.rs` (new payload arm after the BrowserResponse arm ~line 856; stale marking in `unregister()` ~lines 398-449)
- Test: `crates/ahand-hub-store/` unit tests + `crates/ahand-hub/tests/` WS integration test

**Acceptance Criteria:**
- [ ] Inbound AppToolsUpdate replaces the catalog wholesale and clears `stale`
- [ ] An update with `revision <=` the stored revision for the same connection is ignored (out-of-order protection); after reconnect, the daemon's resent snapshot (same revision) IS accepted because disconnect marked the catalog stale
- [ ] WS disconnect marks the catalog `stale=true` (catalog content retained)
- [ ] Audit entry `device.app_tools.updated` written on accepted updates

**Verify:** `cargo test -p ahand-hub-store && cargo test -p ahand-hub app_tool`

**Steps:**

- [ ] **Step 1: Store** (`app_tool_store.rs`). Redis key `ahand:hub:app-tools:{device_id}` holding JSON:

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoredAppToolCatalog {
    pub revision: u64,
    pub stale: bool,
    pub tools: Vec<StoredAppTool>,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoredAppTool {
    pub name: String,
    pub description: String,
    pub input_schema_json: String,
    pub requires_approval: bool,
}
```

API (async, mirroring presence_store's connection handling): `put_catalog(device_id, catalog)`, `get_catalog(device_id) -> Option<StoredAppToolCatalog>`, `mark_stale(device_id) -> bool` (read-modify-write setting `stale=true`, no-op if absent). No TTL — catalogs survive reconnects; staleness is the explicit flag. Unit-test against the same Redis test infra `presence_store.rs` tests use (in-memory/`test-support` feature — copy whichever mechanism its tests use).

- [ ] **Step 2: Acceptance rule** (hub-core or store layer, unit-tested):

```rust
/// A snapshot is accepted when the stored catalog is absent, stale, or has a
/// lower revision. Equal revision on a fresh catalog is a duplicate -> ignore.
pub fn should_accept_update(existing: Option<&StoredAppToolCatalog>, incoming_revision: u64) -> bool {
    match existing {
        None => true,
        Some(c) if c.stale => true,
        Some(c) => incoming_revision > c.revision,
    }
}
```

- [ ] **Step 3: Gateway arm** (device_gateway.rs, new `else if` between the BrowserResponse arm and `dispatch_control_plane_event`):

```rust
} else if let Some(ahand_protocol::envelope::Payload::AppToolsUpdate(ref update)) = envelope.payload {
    state.connections.observe_inbound(&device_id, envelope.seq, envelope.ack).await?;
    queue_ack_only(&control_tx, &device_id, envelope.seq)?;
    let existing = state.app_tools.get_catalog(&device_id).await.ok().flatten();
    if should_accept_update(existing.as_ref(), update.revision) {
        let catalog = StoredAppToolCatalog {
            revision: update.revision,
            stale: false,
            tools: update.tools.iter().map(|t| StoredAppTool {
                name: t.name.clone(),
                description: t.description.clone(),
                input_schema_json: t.input_schema_json.clone(),
                requires_approval: t.requires_approval,
            }).collect(),
            updated_at_ms: now_ms(),
        };
        let tool_count = catalog.tools.len();
        if let Err(err) = state.app_tools.put_catalog(&device_id, catalog).await {
            tracing::warn!(device_id = %device_id, error = %err, "failed to store app tool catalog");
        } else {
            state.append_audit_entry(
                "device.app_tools.updated", "device", &device_id, &device_id,
                serde_json::json!({ "revision": update.revision, "toolCount": tool_count }),
            ).await;
            // Webhook enqueue added in Task 8.
        }
    }
}
```

- [ ] **Step 4: Stale on disconnect** — in `unregister()` (device_gateway.rs:398-449), next to presence cleanup: `let _ = state.app_tools.mark_stale(&device_id).await;`

- [ ] **Step 5: Integration test** (new `crates/ahand-hub/tests/app_tool_catalog.rs`, using the `spawn_test_server` + signed-hello helpers from `tests/support`): connect device → send AppToolsUpdate(rev 1, one tool) → assert store contents via state or the Task 8 endpoint; send rev 1 again → ignored; disconnect → stale; reconnect + resend rev 1 → accepted, stale cleared.

- [ ] **Step 6: Run and commit**

Run: `cargo test -p ahand-hub-store && cargo test -p ahand-hub && cargo check --workspace`

```bash
git add crates/ahand-hub-store/ crates/ahand-hub/
git commit -m "feat(hub): per-device app tool catalog with stale semantics"
```

### Review amendments (applied in post-review hardening pass)

- **Hello-time staleness**: catalog is now marked stale at Hello-accept time on every new connection, in addition to the existing disconnect-time mark. This self-heals hub-crash (cleanup never ran → catalog stays fresh forever) and daemon revision-counter resets after restart. The disconnect-time mark is retained and is connection-id-guarded via `unregister()`, so it cannot late-stale a catalog already accepted by a new connection.
- **`delete_catalog` addition**: `RedisAppToolStore` and `AppToolStore` now expose `delete_catalog`, called best-effort from the admin `delete_device` handler so catalog keys do not outlive their device row.
- **256 KiB size guard**: the gateway arm rejects (warns + ignores) any `AppToolsUpdate` whose combined tool payload exceeds 256 KiB, with `catalog_bytes` included in the accept log line.

---

### Task 8: hub — catalog read endpoint + webhook event

**Goal:** Consumers can query a device's tool catalog over the control plane, and subscribers get `device.app_tools.updated` webhooks.

**Files:**
- Modify: `crates/ahand-hub/src/http/control_plane.rs` (route + handler; router at lines 61-77)
- Modify: `crates/ahand-hub/src/webhook/mod.rs` (new enqueue helper, follow `enqueue_heartbeat` at lines 169-265)
- Modify: `crates/ahand-hub/src/ws/device_gateway.rs` (call the webhook from the Task 7 arm)
- Test: extend `crates/ahand-hub/tests/app_tool_catalog.rs` + webhook test alongside existing webhook tests

**Acceptance Criteria:**
- [ ] `GET /api/devices/{device_id}/app-tools` (control-plane JWT) returns `{ revision, stale, tools: [...] }` with camelCase JSON; 404 for unknown device
- [ ] `stale` in the response is `catalog.stale || !presence.is_online(device_id)`
- [ ] Accepted AppToolsUpdate enqueues webhook `device.app_tools.updated` with data `{"revision": N, "toolCount": M}`

**Verify:** `cargo test -p ahand-hub app_tool`

**Steps:**

- [ ] **Step 1: Route** — add to the control-plane router (inside the same `require_control_plane_jwt` layer):

```rust
.route("/api/devices/{device_id}/app-tools", get(get_device_app_tools))
```

Handler returns the stored catalog mapped to camelCase (`inputSchemaJson`, `requiresApproval`, `updatedAtMs`), with the OR-with-presence stale computation; `404` + the standard error envelope (`{"error":{"code":"DEVICE_NOT_FOUND",...}}` — match whatever shape `control_plane.rs`'s `ControlError` already produces) when the device doesn't exist in `state.devices`. Missing catalog for an existing device → `200` with `{ revision: 0, stale: true, tools: [] }`.

- [ ] **Step 2: Webhook helper** (webhook/mod.rs):

```rust
pub async fn enqueue_app_tools_updated(
    &self,
    device_id: &str,
    external_user_id: Option<&str>,
    revision: u64,
    tool_count: usize,
) -> anyhow::Result<()> {
    self.enqueue_typed(
        "device.app_tools.updated",
        device_id,
        external_user_id,
        serde_json::json!({ "revision": revision, "toolCount": tool_count }),
    )
    .await
}
```

Call it from the Task 7 gateway arm at the `// Webhook enqueue added in Task 8.` marker, guarded by `state.webhook.is_enabled()` exactly like the heartbeat call site (device_gateway.rs:830-845; reuse how that site obtains `external_user_id`).

- [ ] **Step 3: Tests** — endpoint happy path / unknown device / stale-by-offline; webhook enqueued on accepted update and NOT on ignored duplicate. Mirror the existing webhook assertion mechanism used by heartbeat tests.

- [ ] **Step 4: Run and commit**

Run: `cargo test -p ahand-hub && cargo check --workspace`

```bash
git add crates/ahand-hub/
git commit -m "feat(hub): app tool catalog endpoint + device.app_tools.updated webhook"
```

---

### Task 9: hub — `POST /api/control/app-tool` invocation

**Goal:** Cloud callers invoke a device app tool synchronously: oneshot pending-map (browser pattern), offline fast-fail, timeout, audit.

**Files:**
- Create: `crates/ahand-hub/src/app_tool_service.rs` (mirror `browser_service.rs:96-208`)
- Modify: `crates/ahand-hub/src/lib.rs` (module), `crates/ahand-hub/src/state.rs` (pending map)
- Modify: `crates/ahand-hub/src/http/control_plane.rs` (route + handler)
- Modify: `crates/ahand-hub/src/ws/device_gateway.rs` (AppToolResponse arm resolving the pending oneshot)
- Test: `crates/ahand-hub/tests/app_tool_invoke.rs`

**Acceptance Criteria:**
- [ ] Happy path: POST dispatches AppToolRequest to the device; device's AppToolResponse resolves the HTTP call with `200 { toolCallId, result }`
- [ ] Daemon-level errors pass through as `200 { toolCallId, error: { code, message } }` (same convention as `files()`: daemon errors live in the body)
- [ ] Offline device → `409` with code `DEVICE_OFFLINE` (no waiting); unknown device → `404`
- [ ] No response within timeout → `504` with code `TIMEOUT`; pending entry cleaned up (guard on drop)
- [ ] Audit entry `app_tool.invoked` with `{name, toolCallId, outcome}` for every attempt (success, daemon error, timeout, offline)

**Verify:** `cargo test -p ahand-hub --test app_tool_invoke`

**Steps:**

- [ ] **Step 1: State** — `state.rs`: add `pub app_tool_pending: Arc<DashMap<String, tokio::sync::oneshot::Sender<ahand_protocol::AppToolResponse>>>` (next to `browser_pending`, line ~59; initialize where browser_pending is initialized).

- [ ] **Step 2: Service** (`app_tool_service.rs`) — copy the browser_service skeleton: presence check first (`state.presence` / `state.devices` exactly as browser_service does → `DeviceOffline`/`DeviceNotFound` errors), generate `tool_call_id = uuid::Uuid::new_v4()`, insert oneshot + `PendingGuard` (lift the guard type or duplicate it locally), build Envelope `Payload::AppToolRequest` with `msg_id: format!("app-tool-{tool_call_id}")` and the caller's `timeout_ms` (default 60_000, floor 1_000, cap 300_000 — same constants as the daemon), `state.connections.send(&device_id, envelope).await?`, then `tokio::time::timeout(client_timeout + 2s grace, rx)`. Map outcomes to a service-level enum `AppToolServiceError { DeviceNotFound, DeviceOffline, Timeout, ChannelClosed, Dispatch(...) }`.

- [ ] **Step 3: Gateway arm** — in device_gateway.rs, alongside the BrowserResponse arm (~line 846):

```rust
} else if let Some(ahand_protocol::envelope::Payload::AppToolResponse(ref resp)) = envelope.payload {
    if let Some((_, sender)) = state.app_tool_pending.remove(&resp.tool_call_id) {
        let _ = sender.send(resp.clone());
    }
    state.connections.observe_inbound(&device_id, envelope.seq, envelope.ack).await?;
    queue_ack_only(&control_tx, &device_id, envelope.seq)?;
}
```

- [ ] **Step 4: Route + handler** — `POST /api/control/app-tool` in control_plane.rs. Request body (camelCase like create_job): `{ "deviceId": "...", "name": "...", "args": {...}, "timeoutMs": 30000 }` (`args` optional, default `{}`; serialized to `args_json`). Response mapping: result → `200 {"toolCallId": ..., "result": <parsed JSON or null>}`; daemon error → `200 {"toolCallId": ..., "error": {"code": ..., "message": ...}}`; DeviceOffline → 409 `DEVICE_OFFLINE`; DeviceNotFound → 404; Timeout → 504 `TIMEOUT` (reuse/extend the existing `ControlError` IntoResponse mapping at control_plane.rs:622-672). Write the audit entry in the handler after the outcome is known (`state.append_audit_entry("app_tool.invoked", "device", &device_id, <caller principal from the control-plane JWT claims, however create_job derives it>, json!({...}))`).

- [ ] **Step 5: Tests** (`tests/app_tool_invoke.rs`, modeled on `create_job_happy_path_dispatches_and_streams_events` in tests/control_plane.rs:181-300): happy path (attach device, POST, assert device receives AppToolRequest with serialized args, reply, assert HTTP body), daemon-error passthrough, offline 409 (no device attached but registered), unknown 404, timeout 504 with `timeoutMs: 1000` and a silent device, audit entries present for each outcome (query the audit store like existing audit assertions do).

- [ ] **Step 6: Run and commit**

Run: `cargo test -p ahand-hub --test app_tool_invoke && cargo check --workspace`

```bash
git add crates/ahand-hub/
git commit -m "feat(hub): POST /api/control/app-tool with offline fast-fail and audit"
```

---

### Task 10: SDK — `listAppTools` / `invokeAppTool`

**Goal:** `@ahandai/sdk` exposes both endpoints with the package's established auth/error/abort conventions; version 0.3.0.

**Files:**
- Modify: `packages/sdk/src/cloud-client.ts` (model on `files()`, lines 1388-1498)
- Modify: `packages/sdk/src/index.ts` (exports), `packages/sdk/package.json` (version 0.3.0), `packages/sdk/CHANGELOG.md`
- Test: `packages/sdk/src/cloud-client.test.ts` (mockFetch pattern, lines 15-28)

**Acceptance Criteria:**
- [ ] `listAppTools(deviceId, opts?)` → `{ revision, stale, tools: AppToolInfo[] }` (camelCase fields)
- [ ] `invokeAppTool(deviceId, name, args?, opts?)` returns the parsed result; daemon-level errors throw `CloudClientError` with code `"app_tool_error"` carrying the daemon code/message in `jobErrorCode`/`jobErrorMessage`
- [ ] 409 `DEVICE_OFFLINE` → `device_offline`; 504 → `timeout`; 401/403/404 → existing mappings (via `toTypedHttpError`)
- [ ] AbortSignal honored; `getAuthToken()` fetched lazily per call

**Verify:** `pnpm --filter @ahandai/sdk test && pnpm --filter @ahandai/sdk lint`

**Steps:**

- [ ] **Step 1: Types + methods** (cloud-client.ts):

```typescript
export interface AppToolInfo {
  name: string;
  description: string;
  inputSchemaJson: string;
  requiresApproval: boolean;
}

export interface AppToolCatalog {
  revision: number;
  stale: boolean;
  tools: AppToolInfo[];
}

export interface InvokeAppToolOptions {
  timeoutMs?: number;
  signal?: AbortSignal;
}
```

Add `"app_tool_error"` to the `CloudClientErrorCode` union (lines 421-435).

`listAppTools`: GET `${hubUrl}/api/devices/${encodeURIComponent(deviceId)}/app-tools` with bearer token; non-2xx → `throw toTypedHttpError(...)`; parse and return the camelCase body.

`invokeAppTool`: POST `${hubUrl}/api/control/app-tool` with body `{ deviceId, name, args: args ?? {}, timeoutMs }` (omit undefined fields, matching `files()`'s body construction); non-2xx → `toTypedHttpError` (409 DEVICE_OFFLINE and 504 already map correctly); 200 with `body.error` →

```typescript
throw new CloudClientError("app_tool_error", body.error.message ?? "app tool failed", {
  jobErrorCode: body.error.code,
  jobErrorMessage: body.error.message,
});
```

200 with result → `return body.result`.

- [ ] **Step 2: Tests** (cloud-client.test.ts, using `mockFetch`/`jsonResponse`): list happy path (assert URL + auth header + parsed body), list 404 → not_found; invoke happy path (assert body shape incl. args default `{}`), invoke daemon error `APPROVAL_DENIED` → app_tool_error with jobErrorCode, 409 → device_offline, 504 → timeout, abort before fetch → abort error with zero calls, getAuthToken throws → propagates.

- [ ] **Step 3: Version + changelog** — sdk `0.3.0`; CHANGELOG entry referencing `@ahandai/proto@0.3.0` (lockstep convention, see the 0.2.0 entry's phrasing):

```markdown
## 0.3.0 — 2026-06-11

Released alongside `@ahandai/proto@0.3.0`.

### Added

- **`listAppTools()` / `invokeAppTool()`** — discover and invoke
  application-defined tools registered by host apps embedding `ahandd`.
  Daemon-level failures throw `CloudClientError("app_tool_error")` with the
  daemon code in `jobErrorCode` (e.g. `APPROVAL_DENIED`, `EXECUTION_TIMEOUT`).
```

- [ ] **Step 4: Run and commit**

Run: `pnpm --filter @ahandai/proto build && pnpm --filter @ahandai/sdk build && pnpm --filter @ahandai/sdk test && pnpm --filter @ahandai/sdk lint`

```bash
git add packages/sdk/
git commit -m "feat(sdk): listAppTools + invokeAppTool (0.3.0)"
```

---

### Task 11: docs, roadmap, full validation sweep

**Goal:** Documentation reflects the new capability; the whole workspace passes the same checks CI runs.

**Files:**
- Modify: `docs/remote-control-roadmap.md` (add app tool registry under implemented capabilities)
- Modify: `README.md` (only if it lists capabilities — mirror how file ops/browser are mentioned)

**Acceptance Criteria:**
- [ ] Roadmap lists app tools as implemented (daemon registration → hub catalog → control-plane invoke), MCP bridge still TODO
- [ ] Full local validation passes (commands below — the same set hub-ci.yml runs)

**Verify (run all):**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
cargo llvm-cov --summary-only -p ahand-hub-core --all-features --fail-under-lines 99
cargo llvm-cov --summary-only -p ahand-protocol -p ahand-hub-core -p ahand-hub-store -p ahand-hub --fail-under-lines 85
pnpm install --frozen-lockfile
pnpm --filter @ahandai/proto build && pnpm --filter @ahandai/proto lint
pnpm --filter @ahandai/sdk build && pnpm --filter @ahandai/sdk test && pnpm --filter @ahandai/sdk lint
pnpm --filter @ahand/hub-dashboard lint && pnpm --filter @ahand/hub-dashboard test
```

**Steps:**

- [ ] **Step 1: Update roadmap + README**, run the full validation block, fix anything red. Coverage bar for THIS feature: new modules (`app_tool_registry`, `app_tool_store`, `app_tool_service`) target 100% line coverage (check with `cargo llvm-cov --summary-only -p ahandd -p ahand-hub -p ahand-hub-store` and inspect the new files' rows); the repo-wide thresholds (99/85) are the floor, not the goal.
- [ ] **Step 2: Commit**

```bash
git add docs/ README.md
git commit -m "docs: record app tool registry capability"
```

---

## Task Dependency Graph

```text
Task 1 (proto) ──┬── Task 2 (proto-ts) ──────────────┐
                 ├── Task 3 (registry core) → Task 4 → Task 5 → Task 6
                 └── Task 7 (hub catalog) → Task 8 → Task 9 ──┴→ Task 10 (SDK) → Task 11 (sweep)
```

Daemon track (3-6) and hub track (7-9) are independent after Task 1 and may be executed in parallel by separate workers. Task 10 needs Tasks 2 and 9. Task 11 needs everything.

## Out of Scope (per spec Non-Goals)

Streaming results, out-of-process registration, MCP bridging, per-tool authorization tables, multi-app daemons — do NOT add these even if adjacent code makes it tempting.
