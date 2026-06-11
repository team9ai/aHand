//! Application-defined tool registry. Host apps embedding ahandd register
//! tools (definition + async handler); the daemon advertises full snapshots
//! to the hub and executes invocations under session-mode gating.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use ahand_protocol::{AppToolDescriptor, AppToolsUpdate};
use futures_util::future::BoxFuture;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore, watch};

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
    running: Mutex<HashSet<String>>,
    completed: Mutex<VecDeque<(String, CompletedAppToolCall)>>,
}

impl std::fmt::Debug for AppToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppToolRegistry").finish_non_exhaustive()
    }
}

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
            revision: Mutex::new(0),
            revision_tx,
            semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_APP_TOOLS)),
            running: Mutex::new(HashSet::new()),
            completed: Mutex::new(VecDeque::new()),
        }
    }

    /// Subscribe to revision changes. The receiver holds the latest revision.
    pub fn subscribe_revision(&self) -> watch::Receiver<u64> {
        self.revision_tx.subscribe()
    }

    /// Register a tool with its definition and handler.
    /// Returns an error if the name is invalid, the schema is not a JSON
    /// object, or a tool with that name is already registered.
    pub async fn register(&self, def: AppToolDef, handler: AppToolHandler) -> anyhow::Result<()> {
        if !valid_name(&def.name) {
            anyhow::bail!(
                "invalid tool name {:?}: must match ^[a-z0-9_-]{{1,64}}$",
                def.name
            );
        }
        if !def.input_schema.is_object() {
            anyhow::bail!("input_schema for tool {:?} must be a JSON object", def.name);
        }

        {
            let mut tools = self.tools.lock().await;
            if tools.contains_key(&def.name) {
                anyhow::bail!("tool {:?} is already registered", def.name);
            }
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

        self.bump_revision().await;
        Ok(())
    }

    /// Unregister a tool by name. Returns `true` if the tool existed.
    pub async fn unregister(&self, name: &str) -> bool {
        let existed = {
            let mut tools = self.tools.lock().await;
            tools.remove(name).is_some()
        };
        if existed {
            self.bump_revision().await;
        }
        existed
    }

    async fn bump_revision(&self) {
        let new_rev = {
            let mut rev = self.revision.lock().await;
            *rev += 1;
            *rev
        };
        // Ignore send errors (no subscribers is fine).
        let _ = self.revision_tx.send(new_rev);
    }

    /// Return a snapshot of all registered tools, sorted by name.
    pub async fn snapshot(&self) -> AppToolsUpdate {
        let tools = self.tools.lock().await;
        let revision = *self.revision.lock().await;

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

    /// Try to acquire a concurrency permit (fail-fast — CONCURRENCY_LIMIT).
    /// Returns `None` if all 4 permits are already held.
    pub async fn acquire_permit(&self) -> Option<OwnedSemaphorePermit> {
        // Fail-fast is intentional: we don't queue invocations; the hub
        // should retry or surface backpressure to the caller.
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
    pub async fn mark_running(&self, tool_call_id: &str) {
        let mut running = self.running.lock().await;
        running.insert(tool_call_id.to_owned());
    }

    /// Mark a tool call as completed. Evicts oldest entries past 256.
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

    /// Clamp a caller-supplied timeout to [MIN_TIMEOUT_MS, MAX_TIMEOUT_MS].
    /// A value of 0 maps to DEFAULT_TIMEOUT_MS (60 000 ms).
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
        Arc::new(|_args| Box::pin(async move { Ok(json!({"ok": true})) }))
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

    // ── eviction past 256 entries ─────────────────────────────────────────

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

        let p1 = reg.acquire_permit().await;
        let p2 = reg.acquire_permit().await;
        let p3 = reg.acquire_permit().await;
        let p4 = reg.acquire_permit().await;

        assert!(p1.is_some());
        assert!(p2.is_some());
        assert!(p3.is_some());
        assert!(p4.is_some());

        // 5th attempt should fail (fail-fast)
        assert!(
            reg.acquire_permit().await.is_none(),
            "5th permit should return None"
        );

        // Drop one permit, then acquire should succeed
        drop(p1);
        assert!(
            reg.acquire_permit().await.is_some(),
            "permit should be available after drop"
        );
    }

    // ── clamp_timeout ─────────────────────────────────────────────────────

    #[test]
    fn clamp_timeout_values() {
        assert_eq!(
            AppToolRegistry::clamp_timeout(0),
            60_000,
            "0 → default 60000"
        );
        assert_eq!(
            AppToolRegistry::clamp_timeout(500),
            1_000,
            "500 → clamped to min 1000"
        );
        assert_eq!(
            AppToolRegistry::clamp_timeout(400_000),
            300_000,
            "400000 → clamped to max 300000"
        );
        assert_eq!(
            AppToolRegistry::clamp_timeout(30_000),
            30_000,
            "30000 unchanged"
        );
    }
}
