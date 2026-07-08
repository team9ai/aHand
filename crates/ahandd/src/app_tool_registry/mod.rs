//! Application-defined tool registry. Host apps embedding ahandd register
//! tools (definition + async handler); the daemon advertises full snapshots
//! to the hub and executes invocations under session-mode gating.
//!
//! # Revision invariant
//!
//! The revision lives in the `watch::Sender<u64>` (`revision_tx`); there is no
//! separate `revision: Mutex<u64>` field.  All mutations acquire the `tools`
//! lock first and publish the incremented revision via `send_modify` while the
//! `tools` guard is still held, so snapshot content and revision always move
//! atomically.  `snapshot()` reads the revision from `*revision_tx.borrow()`
//! while holding the same `tools` lock, so a snapshot is always consistent
//! with the revision that was current when the lock was acquired.
//!
//! Lock nesting is strictly `tools → (watch borrow)`; the reverse order is
//! never taken.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use ahand_protocol::{AppToolDescriptor, AppToolsUpdate};
use futures_util::future::BoxFuture;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore, watch};

pub const DEFAULT_TIMEOUT_MS: u32 = 60_000;
pub const MIN_TIMEOUT_MS: u32 = 1_000;
pub const MAX_TIMEOUT_MS: u32 = 300_000;
/// Maximum number of concurrent in-flight app tool calls.
const MAX_CONCURRENT_APP_TOOLS: usize = 4;
/// Maximum number of completed call results retained for idempotency replay.
const MAX_COMPLETED_CALLS: usize = 256;

// Bin target never constructs this directly; registration happens through
// the embedder-facing lib API (`DaemonHandle::register_app_tool`).
#[allow(dead_code)]
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

// Bin target never constructs this directly; AppToolInvocation is consumed by
// embedder-registered handlers through the lib API.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct AppToolInvocation {
    pub tool_call_id: String,
    pub name: String,
    pub args: serde_json::Value,
    /// Daemon-clamped execution budget delivered to the handler. For
    /// approval-gated calls this may be the remaining budget after approval
    /// wait, not the original request timeout.
    pub timeout_ms: u32,
    pub context: Option<serde_json::Value>,
}

#[allow(dead_code)]
pub type AppToolHandler = Arc<
    dyn Fn(AppToolInvocation) -> BoxFuture<'static, Result<serde_json::Value, AppToolError>>
        + Send
        + Sync,
>;

#[allow(dead_code)]
pub type AppToolArgsHandler = Arc<
    dyn Fn(serde_json::Value) -> BoxFuture<'static, Result<serde_json::Value, AppToolError>>
        + Send
        + Sync,
>;

#[allow(dead_code)]
pub fn args_only_handler(handler: AppToolArgsHandler) -> AppToolHandler {
    Arc::new(move |invocation: AppToolInvocation| handler(invocation.args))
}

#[derive(Debug, Clone)]
pub struct CompletedAppToolCall {
    pub result_json: Option<String>,
    pub error: Option<AppToolError>,
}

#[derive(Debug, Clone)]
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
    revision_tx: watch::Sender<u64>,
    semaphore: Arc<Semaphore>,
    running: Mutex<HashSet<String>>,
    completed: Mutex<VecDeque<(String, CompletedAppToolCall)>>,
}

impl std::fmt::Debug for AppToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppToolRegistry").finish_non_exhaustive()
    }
}

// Bin target never calls this directly; exercised via the lib registration
// path and unit tests.
#[allow(dead_code)]
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

impl Default for AppToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl AppToolRegistry {
    pub fn new() -> Self {
        let (revision_tx, _) = watch::channel(0u64);
        Self {
            tools: Mutex::new(HashMap::new()),
            revision_tx,
            semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_APP_TOOLS)),
            running: Mutex::new(HashSet::new()),
            completed: Mutex::new(VecDeque::new()),
        }
    }

    /// Subscribe to revision changes. The receiver holds the latest revision.
    ///
    /// The receiver is pre-marked changed so a watcher that loops on
    /// `changed()` will fire once immediately after subscribing. Consumers
    /// that send an explicit initial snapshot before entering their watch
    /// loop should call `borrow_and_update()` first to consume this initial
    /// notification (the Task 4 watcher in `ahand_client` does exactly this).
    pub fn subscribe_revision(&self) -> watch::Receiver<u64> {
        let mut rx = self.revision_tx.subscribe();
        rx.mark_changed();
        rx
    }

    /// Register a tool with its definition and handler.
    /// Returns an error if the name is invalid, the schema is not a JSON
    /// object, or a tool with that name is already registered.
    ///
    /// A failed registration (invalid name or duplicate) leaves the revision
    /// unchanged — no watch notification is sent.
    // Bin target never calls this directly; embedders register through
    // `DaemonHandle::register_app_tool` on the lib API.
    #[allow(dead_code)]
    pub async fn register(&self, def: AppToolDef, handler: AppToolHandler) -> anyhow::Result<()> {
        self.register_many(vec![(def, handler)]).await
    }

    /// Register a set of tools as one catalog mutation.
    ///
    /// The full batch is validated before any tool is inserted. A failed batch
    /// leaves the registry and revision unchanged.
    pub async fn register_many(
        &self,
        registrations: Vec<(AppToolDef, AppToolHandler)>,
    ) -> anyhow::Result<()> {
        if registrations.is_empty() {
            return Ok(());
        }

        let mut seen = HashSet::new();
        for (def, _) in &registrations {
            if !valid_name(&def.name) {
                anyhow::bail!(
                    "invalid tool name {:?}: must match ^[a-z0-9_-]{{1,64}}$",
                    def.name
                );
            }
            if !def.input_schema.is_object() {
                anyhow::bail!("input_schema for tool {:?} must be a JSON object", def.name);
            }
            if !seen.insert(def.name.clone()) {
                anyhow::bail!("tool {:?} is already registered", def.name);
            }
        }

        let mut tools = self.tools.lock().await;
        for (def, _) in &registrations {
            if tools.contains_key(&def.name) {
                anyhow::bail!("tool {:?} is already registered", def.name);
            }
        }

        for (def, handler) in registrations {
            let descriptor = AppToolDescriptor {
                name: def.name.clone(),
                description: def.description.clone(),
                input_schema_json: def.input_schema.to_string(),
                requires_approval: def.requires_approval,
            };
            tools.insert(
                def.name,
                Registered {
                    descriptor,
                    handler,
                },
            );
        }
        // Publish one revision for the complete batch while the tools lock is
        // still held, so snapshot content and revision move atomically.
        self.revision_tx.send_modify(|r| *r += 1);
        Ok(())
    }

    /// Unregister a tool by name. Returns `true` if the tool existed.
    // Bin target never calls this directly; embedders unregister through
    // `DaemonHandle::unregister_app_tool` on the lib API.
    #[allow(dead_code)]
    pub async fn unregister(&self, name: &str) -> bool {
        let mut tools = self.tools.lock().await;
        let existed = tools.remove(name).is_some();
        if existed {
            // Publish new revision while the tools lock is still held.
            self.revision_tx.send_modify(|r| *r += 1);
        }
        existed
    }

    /// Return a snapshot of all registered tools, sorted by name.
    ///
    /// Reads both the tool list and the current revision under the same lock
    /// so the snapshot is always consistent.
    pub async fn snapshot(&self) -> AppToolsUpdate {
        let tools = self.tools.lock().await;
        // Read revision while holding the tools lock for consistency.
        let revision = *self.revision_tx.borrow();

        let mut descriptors: Vec<AppToolDescriptor> =
            tools.values().map(|r| r.descriptor.clone()).collect();
        descriptors.sort_by(|a, b| a.name.cmp(&b.name));

        AppToolsUpdate {
            revision,
            tools: descriptors,
        }
    }

    /// Look up a tool's descriptor and handler by name.
    pub async fn lookup(&self, name: &str) -> Option<(AppToolDescriptor, AppToolHandler)> {
        let tools = self.tools.lock().await;
        tools
            .get(name)
            .map(|r| (r.descriptor.clone(), Arc::clone(&r.handler)))
    }

    /// Try to acquire a concurrency permit (fail-fast).
    ///
    /// Returns `None` immediately if all [`MAX_CONCURRENT_APP_TOOLS`] permits
    /// are already held. The hub should retry or surface backpressure to the
    /// caller rather than queueing invocations here.
    pub fn try_acquire_permit(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.semaphore).try_acquire_owned().ok()
    }

    /// Check the state of a tool call: Running, Completed, or Unknown.
    pub async fn call_state(&self, tool_call_id: &str) -> CallState {
        {
            let running = self.running.lock().await;
            if running.contains(tool_call_id) {
                return CallState::Running;
            }
        }
        {
            let completed = self.completed.lock().await;
            for (id, result) in completed.iter() {
                if id == tool_call_id {
                    return CallState::Completed(result.clone());
                }
            }
        }
        CallState::Unknown
    }

    /// Mark a tool call as running.
    ///
    /// **Every call to `mark_running` MUST reach `mark_completed` on all exit
    /// paths**, or the call-id stays `Running` and will shadow retries for that
    /// id. The invocation handler guarantees this invariant.
    pub async fn mark_running(&self, tool_call_id: &str) {
        let mut running = self.running.lock().await;
        running.insert(tool_call_id.to_owned());
    }

    /// Mark a tool call as completed. Evicts oldest entries past [`MAX_COMPLETED_CALLS`].
    pub async fn mark_completed(&self, tool_call_id: String, result: CompletedAppToolCall) {
        {
            let mut running = self.running.lock().await;
            running.remove(&tool_call_id);
        }
        let mut completed = self.completed.lock().await;
        completed.push_back((tool_call_id, result));
        while completed.len() > MAX_COMPLETED_CALLS {
            completed.pop_front();
        }
    }

    /// Clamp a caller-supplied timeout to [[`MIN_TIMEOUT_MS`], [`MAX_TIMEOUT_MS`]].
    /// A value of 0 maps to [`DEFAULT_TIMEOUT_MS`].
    pub fn clamp_timeout(timeout_ms: u32) -> u32 {
        if timeout_ms == 0 {
            DEFAULT_TIMEOUT_MS
        } else {
            timeout_ms.clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_handler() -> AppToolHandler {
        Arc::new(|_invocation| Box::pin(async move { Ok(json!({"ok": true})) }))
    }

    fn make_def(name: &str) -> AppToolDef {
        AppToolDef {
            name: name.to_owned(),
            description: "A test tool".to_owned(),
            input_schema: json!({"type": "object", "properties": {}}),
            requires_approval: false,
        }
    }

    // ── name validation ────────────────────────────────────────────────────

    #[test]
    fn name_validation_table() {
        // Invalid names
        assert!(!valid_name(""), "empty should be invalid");
        assert!(!valid_name("UPPER"), "uppercase should be invalid");
        assert!(!valid_name("has space"), "space should be invalid");
        assert!(!valid_name("emoji🙂"), "emoji should be invalid");
        assert!(
            !valid_name(&"a".repeat(65)),
            "65-char name should be invalid"
        );

        // Valid names
        assert!(valid_name("ok_tool-1"), "ok_tool-1 should be valid");
        assert!(valid_name("a"), "single char should be valid");
        assert!(valid_name(&"a".repeat(64)), "64-char name should be valid");
    }

    // ── schema + duplicate validation ─────────────────────────────────────

    #[tokio::test]
    async fn schema_not_object_rejected() {
        let reg = AppToolRegistry::new();
        let mut def = make_def("my_tool");
        def.input_schema = json!([1, 2, 3]);
        let result = reg.register(def, make_handler()).await;
        assert!(result.is_err(), "array schema should be rejected");
        assert!(result.unwrap_err().to_string().contains("JSON object"));
    }

    #[tokio::test]
    async fn duplicate_rejected_and_unregister_roundtrip() {
        let reg = AppToolRegistry::new();

        // First registration succeeds
        reg.register(make_def("my_tool"), make_handler())
            .await
            .expect("first register should succeed");

        // Duplicate fails
        let dup = reg.register(make_def("my_tool"), make_handler()).await;
        assert!(dup.is_err(), "duplicate registration should be rejected");
        assert!(dup.unwrap_err().to_string().contains("already registered"));

        // Unregister returns true
        assert!(
            reg.unregister("my_tool").await,
            "unregister should return true"
        );
        // Unregister again returns false
        assert!(
            !reg.unregister("my_tool").await,
            "second unregister should return false"
        );

        // Re-register after unregister succeeds
        reg.register(make_def("my_tool"), make_handler())
            .await
            .expect("re-register after unregister should succeed");
    }

    // ── snapshot + revision + watch ───────────────────────────────────────

    #[tokio::test]
    async fn snapshot_sorted_and_revision_tracks_mutations() {
        let reg = AppToolRegistry::new();
        let mut rx = reg.subscribe_revision();
        // Consume the pre-mark so we wait only for real mutations.
        rx.borrow_and_update();

        // Initial state
        let snap = reg.snapshot().await;
        assert_eq!(snap.revision, 0);
        assert!(snap.tools.is_empty());

        // Register two tools in reverse alphabetical order
        reg.register(make_def("zebra"), make_handler())
            .await
            .unwrap();
        let _ = rx.changed().await;
        assert_eq!(*rx.borrow(), 1);

        reg.register(make_def("alpha"), make_handler())
            .await
            .unwrap();
        let _ = rx.changed().await;
        assert_eq!(*rx.borrow(), 2);

        let snap = reg.snapshot().await;
        assert_eq!(snap.revision, 2);
        assert_eq!(snap.tools.len(), 2);
        assert_eq!(
            snap.tools[0].name, "alpha",
            "snapshot should be sorted by name"
        );
        assert_eq!(snap.tools[1].name, "zebra");

        // Unregister bumps revision
        reg.unregister("alpha").await;
        let _ = rx.changed().await;
        assert_eq!(*rx.borrow(), 3);

        let snap = reg.snapshot().await;
        assert_eq!(snap.revision, 3);
        assert_eq!(snap.tools.len(), 1);
    }

    // ── idempotency cache: Unknown → Running → Completed ─────────────────

    #[tokio::test]
    async fn call_state_transitions() {
        let reg = AppToolRegistry::new();
        let id = "call-abc";

        // Initially unknown
        assert!(matches!(reg.call_state(id).await, CallState::Unknown));

        // Mark running
        reg.mark_running(id).await;
        assert!(matches!(reg.call_state(id).await, CallState::Running));

        // Mark completed
        reg.mark_completed(
            id.to_owned(),
            CompletedAppToolCall {
                result_json: Some(r#"{"ok":true}"#.to_owned()),
                error: None,
            },
        )
        .await;

        match reg.call_state(id).await {
            CallState::Completed(c) => {
                assert_eq!(c.result_json.as_deref(), Some(r#"{"ok":true}"#));
                assert!(c.error.is_none());
            }
            _ => panic!("expected Completed"),
        }

        // No longer running
        assert!(!matches!(reg.call_state(id).await, CallState::Running));
    }

    // ── eviction past MAX_COMPLETED_CALLS entries ─────────────────────────

    #[tokio::test]
    async fn eviction_past_256() {
        let reg = AppToolRegistry::new();

        for i in 0u32..260 {
            let id = format!("call-{i:04}");
            reg.mark_running(&id).await;
            reg.mark_completed(
                id,
                CompletedAppToolCall {
                    result_json: Some(format!("{i}")),
                    error: None,
                },
            )
            .await;
        }

        // Oldest entries (0..3) should be evicted
        for i in 0u32..4 {
            let id = format!("call-{i:04}");
            assert!(
                matches!(reg.call_state(&id).await, CallState::Unknown),
                "call-{i:04} should have been evicted"
            );
        }

        // call-0004 (oldest survivor) must still be present
        let survivor = "call-0004";
        assert!(
            matches!(reg.call_state(survivor).await, CallState::Completed(_)),
            "call-0004 should be the oldest surviving entry"
        );

        // Most recent entry should still be present
        let last_id = "call-0259";
        assert!(
            matches!(reg.call_state(last_id).await, CallState::Completed(_)),
            "call-0259 should still be present"
        );
    }

    // ── permits ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn permits_4_ok_5th_none_drop_recover() {
        let reg = AppToolRegistry::new();

        let p1 = reg.try_acquire_permit();
        let p2 = reg.try_acquire_permit();
        let p3 = reg.try_acquire_permit();
        let p4 = reg.try_acquire_permit();

        assert!(p1.is_some());
        assert!(p2.is_some());
        assert!(p3.is_some());
        assert!(p4.is_some());

        // 5th attempt should fail (fail-fast)
        assert!(
            reg.try_acquire_permit().is_none(),
            "5th permit should return None"
        );

        // Drop one permit, then acquire should succeed
        drop(p1);
        assert!(
            reg.try_acquire_permit().is_some(),
            "permit should be available after drop"
        );
    }

    // ── clamp_timeout ─────────────────────────────────────────────────────

    #[test]
    fn clamp_timeout_values() {
        assert_eq!(
            AppToolRegistry::clamp_timeout(0),
            DEFAULT_TIMEOUT_MS,
            "0 → default DEFAULT_TIMEOUT_MS"
        );
        assert_eq!(
            AppToolRegistry::clamp_timeout(500),
            MIN_TIMEOUT_MS,
            "500 → clamped to MIN_TIMEOUT_MS"
        );
        assert_eq!(
            AppToolRegistry::clamp_timeout(400_000),
            MAX_TIMEOUT_MS,
            "400000 → clamped to MAX_TIMEOUT_MS"
        );
        assert_eq!(
            AppToolRegistry::clamp_timeout(30_000),
            30_000,
            "30000 unchanged"
        );
    }

    // ── invalid-name register via public API ──────────────────────────────

    #[tokio::test]
    async fn invalid_name_register_returns_err() {
        let reg = AppToolRegistry::new();
        let mut def = make_def("INVALID_NAME");
        def.name = "INVALID_NAME".to_owned();
        let result = reg.register(def, make_handler()).await;
        assert!(result.is_err(), "invalid name should be rejected");
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("invalid tool name"),
            "error should mention invalid tool name"
        );
    }

    // ── failed register leaves revision unbumped ──────────────────────────

    #[tokio::test]
    async fn failed_register_leaves_revision_unbumped() {
        let reg = AppToolRegistry::new();
        let mut rx = reg.subscribe_revision();
        rx.borrow_and_update(); // consume pre-mark

        // Register with invalid name — should fail
        let mut bad_def = make_def("ok");
        bad_def.name = "BAD NAME".to_owned();
        let _ = reg.register(bad_def, make_handler()).await;

        // Duplicate register after first success
        reg.register(make_def("ok"), make_handler())
            .await
            .expect("first ok");
        let snap_after_first = reg.snapshot().await;
        let rev_after_first = *rx.borrow_and_update();

        let _ = reg.register(make_def("ok"), make_handler()).await; // duplicate
        let snap_after_dup = reg.snapshot().await;
        let rev_after_dup = *rx.borrow();

        // After the duplicate failure, revision must NOT have changed.
        assert_eq!(
            snap_after_dup.revision, snap_after_first.revision,
            "duplicate register must not bump snapshot revision"
        );
        assert_eq!(
            rev_after_dup, rev_after_first,
            "duplicate register must not bump watch revision"
        );
    }

    // ── subscribe_revision fires immediately once ─────────────────────────

    #[tokio::test]
    async fn subscribe_revision_fires_immediately() {
        let reg = AppToolRegistry::new();

        // A fresh subscriber must fire immediately (pre-marked changed).
        let mut rx = reg.subscribe_revision();
        let fired = tokio::time::timeout(std::time::Duration::from_millis(100), rx.changed()).await;
        assert!(
            fired.is_ok(),
            "subscribe_revision receiver should fire immediately (mark_changed)"
        );

        // After borrow_and_update, a second changed() must NOT fire until a
        // real mutation happens.
        rx.borrow_and_update();
        let no_fire =
            tokio::time::timeout(std::time::Duration::from_millis(50), rx.changed()).await;
        assert!(
            no_fire.is_err(),
            "no second fire expected before a mutation"
        );

        // A real mutation should now fire.
        reg.register(make_def("tool_a"), make_handler())
            .await
            .unwrap();
        let fired_after_mutation =
            tokio::time::timeout(std::time::Duration::from_millis(100), rx.changed()).await;
        assert!(
            fired_after_mutation.is_ok(),
            "should fire after a real mutation"
        );
    }
}
