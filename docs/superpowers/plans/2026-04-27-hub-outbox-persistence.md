# Hub Outbox Persistence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers-extended-cc:subagent-driven-development (recommended) or superpowers-extended-cc:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the hub's per-device outbox state survive process restarts via Redis Streams, with multi-replica safety via a Redis-side fenced lock, and auto-recover currently-wedged devices on first reconnect.

**Architecture:** Introduce `OutboxStore` trait in `ahand-hub-core`. Two implementations: `InMemoryOutboxStore` (used by `StoreConfig::Memory` and unit tests) and `RedisOutboxStore` (used by `StoreConfig::Persistent`). Refactor `ConnectionRegistry` to delegate all outbox state to the trait. Each WS connection owns a `session_id` (uuid) used as a fencing token in every Redis script. Lock acquisition uses `SET NX EX 30`; handoff is accelerated via a Pub/Sub `kick:{device_id}` channel; correctness comes from the fenced `INCR`/`XADD` scripts that always check ownership.

**Tech Stack:** Rust 2021 (workspace edition), `redis = 0.24` async (already in workspace), `tokio` watch channels, `async-trait`, `prost` for protobuf encoding, `testcontainers` for integration.

**Spec:** `docs/superpowers/specs/2026-04-27-hub-outbox-persistence-design.md`

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `crates/ahand-hub-core/src/traits.rs` | Modify | Add `OutboxStore` trait + `KickSubscription` + `OutboxSendOutcome` types |
| `crates/ahand-hub/src/ws/in_memory_outbox.rs` | Create | `InMemoryOutboxStore` impl for `StoreConfig::Memory` and tests |
| `crates/ahand-hub/src/ws/mod.rs` | Modify | `pub mod in_memory_outbox;` |
| `crates/ahand-hub-store/src/outbox_lua.rs` | Create | Lua source strings + cached SHA1 + `EVALSHA`-with-fallback wrapper |
| `crates/ahand-hub-store/src/outbox_store.rs` | Create | `RedisOutboxStore` impl |
| `crates/ahand-hub-store/src/lib.rs` | Modify | `pub mod outbox_lua;` `pub mod outbox_store;` |
| `crates/ahand-hub/src/ws/device_gateway.rs` | Modify | Refactor `ConnectionRegistry` to use `OutboxStore`; add lease renewer + kick subscriber tasks |
| `crates/ahand-hub/src/state.rs` | Modify | Wire `Arc<dyn OutboxStore>` based on `StoreConfig`, pass into `ConnectionRegistry::new` |
| `crates/ahand-hub-store/tests/store_roundtrip.rs` | Modify | Add `RedisOutboxStore` integration tests (uses existing `TestStack` Redis container) |
| `crates/ahand-hub/tests/outbox_persistence.rs` | Create | End-to-end regression: replay-after-restart, lock takeover, bootstrap path |

`HubError` already has `InvalidPeerAck`, `Internal`, `Unauthorized`. We add `OutboxLockContention` for the rare "kick failed to dislodge previous owner" case.

---

## Task Ordering and Dependencies

```
Task 0 (trait) ─┬─▶ Task 1 (InMemory impl) ────────────────────────┐
                │                                                  │
                └─▶ Task 2 (Lua module) ─▶ Task 3 (Redis lock) ─▶ Task 4 (Redis send) ─┤
                                                                                       │
                                                            ┌──────────────────────────┘
                                                            ▼
                                                       Task 5 (Registry refactor)
                                                            │
                                                            ▼
                                                       Task 6 (AppState wiring)
                                                            │
                                                            ▼
                                                       Task 7 (E2E regression tests)
```

Tasks 1 and 2 can be done in parallel after Task 0 ships. Tasks 3 and 4 are sequential.

---

## Task 0: `OutboxStore` trait and supporting types

**Goal:** Add the `OutboxStore` trait, `KickSubscription`, and `OutboxLockContention` error variant. No implementations yet.

**Files:**
- Modify: `crates/ahand-hub-core/src/traits.rs` (append new trait at end)
- Modify: `crates/ahand-hub-core/src/error.rs` (add variant)
- Modify: `crates/ahand-hub-core/Cargo.toml` (add `tokio` workspace dep with `sync` feature, `uuid` workspace dep — verify already present)

**Acceptance Criteria:**
- [ ] `OutboxStore` trait compiles with `#[async_trait]` and is `Send + Sync + 'static` capable.
- [ ] `KickSubscription` is documented and droppable (drop releases Pub/Sub resources).
- [ ] `HubError::OutboxLockContention` exists and renders sensibly.
- [ ] `cargo check -p ahand-hub-core` passes; existing tests still green.

**Verify:** `cargo check -p ahand-hub-core && cargo test -p ahand-hub-core` → all green.

**Steps:**

- [ ] **Step 1: Add `OutboxLockContention` to `HubError`**

Edit `crates/ahand-hub-core/src/error.rs`:

```rust
#[derive(Debug, Error, PartialEq, Eq)]
pub enum HubError {
    // ... existing variants unchanged ...
    #[error("invalid peer ack {ack}, max issued seq is {max}")]
    InvalidPeerAck { ack: u64, max: u64 },
    #[error("outbox lock contention for device {0}")]
    OutboxLockContention(String),
    #[error("internal: {0}")]
    Internal(String),
}
```

- [ ] **Step 2: Verify `tokio` and `uuid` are workspace deps in `ahand-hub-core/Cargo.toml`**

```bash
grep -E "^(tokio|uuid|async-trait)" crates/ahand-hub-core/Cargo.toml
```

Expected: at least `async-trait` and `tokio` present. If `uuid` is missing, add:

```toml
uuid = { workspace = true, features = ["v4"] }
```

(Check root `Cargo.toml` for `[workspace.dependencies] uuid` — should already exist since `ahand-hub-store` and `ahand-hub` use it.)

- [ ] **Step 3: Add the trait at the bottom of `traits.rs`**

```rust
// ── Outbox persistence (hub→device durable buffer + multi-replica fencing) ──

/// Subscription handle returned by [`OutboxStore::subscribe_kick`]. The
/// receiver fires with `()` whenever a kick is published on the device's
/// channel. Drop releases the underlying Pub/Sub connection and aborts the
/// background reader task.
pub struct KickSubscription {
    pub recv: tokio::sync::watch::Receiver<u64>,
    pub _drop_guard: tokio::task::JoinHandle<()>,
}

/// Per-device durable outbox. Implementations:
///
/// * `RedisOutboxStore` (production) — Redis Streams + Lua scripts + Pub/Sub.
/// * `InMemoryOutboxStore` (tests, `StoreConfig::Memory`) — in-process state.
///
/// All methods are `&self` and async; implementations are expected to be
/// `Arc`-shared across tasks. `session_id` is a UUID generated per WS
/// connection and acts as the fencing token: every fenced operation aborts
/// with [`HubError::Unauthorized`] if the lock value does not match.
#[async_trait]
pub trait OutboxStore: Send + Sync + 'static {
    /// Atomically `SET lock:device:{id} {session_id} NX EX <ttl>`. Returns
    /// `true` on success, `false` if another session already holds the lock.
    async fn try_acquire_lock(&self, device_id: &str, session_id: &str) -> Result<bool>;

    /// Best-effort `PUBLISH kick:{device_id} <new_session_id>`. Failures are
    /// logged but not propagated; the lease will eventually expire.
    async fn kick(&self, device_id: &str, new_session_id: &str) -> Result<()>;

    /// Subscribe to `kick:{device_id}`. The returned watch receiver ticks
    /// (value increments) each time a kick arrives.
    async fn subscribe_kick(&self, device_id: &str) -> Result<KickSubscription>;

    /// Renew lease: Lua-checked `EXPIRE` — only renews if the current value
    /// equals `session_id`. Returns `false` if the lock was lost.
    async fn renew_lock(&self, device_id: &str, session_id: &str) -> Result<bool>;

    /// Release lock: Lua-checked `DEL` — only deletes if value matches.
    /// Idempotent.
    async fn release_lock(&self, device_id: &str, session_id: &str) -> Result<()>;

    /// Reconcile the per-device seq counter against the device's
    /// `Hello.last_ack`:
    ///
    /// * If `seq:{id} > last_ack`: trim acked entries with
    ///   `XTRIM outbox:{id} MINID 0-{last_ack+1}` and return `current_seq`.
    /// * If `seq:{id} <= last_ack`: **bootstrap path** — server lost state
    ///   (e.g., process restart wiped Redis is impossible since Redis is
    ///   the durable layer; this branch fires when the device's `last_ack`
    ///   exceeds anything the store has ever seen, which is exactly the
    ///   wedged-after-restart case for fresh deploys carrying this code).
    ///   Set `seq:{id} = last_ack`, `DEL outbox:{id}`, return `last_ack`.
    ///
    /// Both branches call `EXPIRE` to keep the keys alive for 30d.
    /// The fence is checked via the lock script; callers must hold the lock.
    async fn reconcile_on_hello(
        &self,
        device_id: &str,
        session_id: &str,
        last_ack: u64,
    ) -> Result<u64>;

    /// Read all unacked frames for replay: `XRANGE outbox:{id} (0-{last_ack} +`.
    async fn unacked_frames(&self, device_id: &str, last_ack: u64) -> Result<Vec<Vec<u8>>>;

    /// Reserve the next seq atomically: fence + `INCR seq:{id}`. Returns the
    /// assigned seq. Callers then mutate `envelope.seq`, encode, and call
    /// [`Self::xadd_frame`].
    async fn fenced_incr_seq(&self, device_id: &str, session_id: &str) -> Result<u64>;

    /// Append the encoded frame to the stream: fence + `XADD outbox:{id}
    /// 0-{seq} frame <bytes>` + `MAXLEN ~ 10000` + `EXPIRE 30d`.
    async fn xadd_frame(
        &self,
        device_id: &str,
        session_id: &str,
        seq: u64,
        frame: Vec<u8>,
    ) -> Result<()>;

    /// Trim acked frames: `XTRIM outbox:{id} MINID 0-{ack+1}`. Fire-and-forget
    /// from the caller's perspective; failure is logged but not surfaced.
    async fn observe_ack(&self, device_id: &str, ack: u64) -> Result<()>;
}
```

- [ ] **Step 4: Run cargo check + tests**

```bash
cargo check -p ahand-hub-core
cargo test -p ahand-hub-core
```

Both expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ahand-hub-core/src/traits.rs crates/ahand-hub-core/src/error.rs crates/ahand-hub-core/Cargo.toml
git commit -m "$(cat <<'EOF'
feat(hub-core): add OutboxStore trait + KickSubscription

Trait surface for hub→device outbox persistence, used by both the new
RedisOutboxStore (production) and InMemoryOutboxStore (tests, Memory
StoreConfig). Adds HubError::OutboxLockContention for the kick-failed path.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 1: `InMemoryOutboxStore` implementation

**Goal:** A complete in-process implementation of `OutboxStore` that mirrors the Redis semantics. Used by `StoreConfig::Memory` and as the test fixture for Task 5+.

**Files:**
- Create: `crates/ahand-hub/src/ws/in_memory_outbox.rs`
- Modify: `crates/ahand-hub/src/ws/mod.rs` (add `pub mod in_memory_outbox;`)
- Test: same file (`#[cfg(test)] mod tests`)

**Acceptance Criteria:**
- [ ] All 10 trait methods implemented; semantics match the spec.
- [ ] Lock acquisition is atomic (one of N concurrent acquirers wins, others see `false`).
- [ ] Bootstrap path inside `reconcile_on_hello` works: `seq` set to `last_ack`, frames cleared, returned value equals `last_ack`.
- [ ] Fence rejects writes from a non-owning session.
- [ ] Kick subscription delivers a `()` tick when `kick` is called.
- [ ] Unit tests cover: lock NX, kick handoff, fence rejection, bootstrap path, replay across simulated reconnect, ack-trim.

**Verify:** `cargo test -p ahand-hub --lib ws::in_memory_outbox::tests` → all pass.

**Steps:**

- [ ] **Step 1: Add the module declaration**

Edit `crates/ahand-hub/src/ws/mod.rs` — add at the top of the file (or alongside existing `pub mod`):

```rust
pub mod in_memory_outbox;
```

- [ ] **Step 2: Write the failing tests first**

Create `crates/ahand-hub/src/ws/in_memory_outbox.rs` and add the test module before any implementation. We'll fail-compile first, then build the impl until tests pass.

```rust
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use ahand_hub_core::traits::{KickSubscription, OutboxStore};
use ahand_hub_core::{HubError, Result};
use async_trait::async_trait;
use tokio::sync::{Mutex, watch};

#[derive(Default)]
struct DeviceState {
    lock: Option<String>,
    seq: u64,
    buffer: VecDeque<(u64, Vec<u8>)>,
    kick_tx: Option<watch::Sender<u64>>,
    kick_count: u64,
}

const MAX_BUFFER: usize = 10_000;

/// Process-local OutboxStore for `StoreConfig::Memory` and unit tests.
/// Mirrors the Redis semantics method-for-method.
#[derive(Default, Clone)]
pub struct InMemoryOutboxStore {
    inner: Arc<Mutex<HashMap<String, DeviceState>>>,
}

impl InMemoryOutboxStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> InMemoryOutboxStore {
        InMemoryOutboxStore::new()
    }

    #[tokio::test]
    async fn try_acquire_lock_first_succeeds_second_fails() {
        let s = store();
        assert!(s.try_acquire_lock("dev", "sess-a").await.unwrap());
        assert!(!s.try_acquire_lock("dev", "sess-b").await.unwrap());
    }

    #[tokio::test]
    async fn release_then_reacquire_succeeds() {
        let s = store();
        assert!(s.try_acquire_lock("dev", "sess-a").await.unwrap());
        s.release_lock("dev", "sess-a").await.unwrap();
        assert!(s.try_acquire_lock("dev", "sess-b").await.unwrap());
    }

    #[tokio::test]
    async fn release_with_wrong_session_is_noop() {
        let s = store();
        assert!(s.try_acquire_lock("dev", "sess-a").await.unwrap());
        s.release_lock("dev", "sess-other").await.unwrap();
        assert!(!s.try_acquire_lock("dev", "sess-b").await.unwrap());
    }

    #[tokio::test]
    async fn renew_lock_succeeds_for_owner_fails_for_other() {
        let s = store();
        s.try_acquire_lock("dev", "sess-a").await.unwrap();
        assert!(s.renew_lock("dev", "sess-a").await.unwrap());
        assert!(!s.renew_lock("dev", "sess-b").await.unwrap());
    }

    #[tokio::test]
    async fn fenced_incr_seq_increments_per_owner() {
        let s = store();
        s.try_acquire_lock("dev", "sess-a").await.unwrap();
        assert_eq!(s.fenced_incr_seq("dev", "sess-a").await.unwrap(), 1);
        assert_eq!(s.fenced_incr_seq("dev", "sess-a").await.unwrap(), 2);
    }

    #[tokio::test]
    async fn fenced_incr_seq_rejects_non_owner() {
        let s = store();
        s.try_acquire_lock("dev", "sess-a").await.unwrap();
        let err = s.fenced_incr_seq("dev", "sess-b").await.unwrap_err();
        assert!(matches!(err, HubError::Unauthorized));
    }

    #[tokio::test]
    async fn xadd_frame_stores_for_replay() {
        let s = store();
        s.try_acquire_lock("dev", "sess-a").await.unwrap();
        let seq = s.fenced_incr_seq("dev", "sess-a").await.unwrap();
        s.xadd_frame("dev", "sess-a", seq, b"hello".to_vec()).await.unwrap();
        let frames = s.unacked_frames("dev", 0).await.unwrap();
        assert_eq!(frames, vec![b"hello".to_vec()]);
    }

    #[tokio::test]
    async fn unacked_frames_returns_only_after_last_ack() {
        let s = store();
        s.try_acquire_lock("dev", "sess-a").await.unwrap();
        for i in 1..=3 {
            let seq = s.fenced_incr_seq("dev", "sess-a").await.unwrap();
            s.xadd_frame("dev", "sess-a", seq, vec![i as u8]).await.unwrap();
        }
        let frames = s.unacked_frames("dev", 1).await.unwrap();
        assert_eq!(frames, vec![vec![2], vec![3]]);
    }

    #[tokio::test]
    async fn observe_ack_trims_buffer() {
        let s = store();
        s.try_acquire_lock("dev", "sess-a").await.unwrap();
        for i in 1..=3 {
            let seq = s.fenced_incr_seq("dev", "sess-a").await.unwrap();
            s.xadd_frame("dev", "sess-a", seq, vec![i as u8]).await.unwrap();
        }
        s.observe_ack("dev", 2).await.unwrap();
        let frames = s.unacked_frames("dev", 0).await.unwrap();
        assert_eq!(frames, vec![vec![3]]);
    }

    #[tokio::test]
    async fn reconcile_normal_path_returns_current_seq() {
        let s = store();
        s.try_acquire_lock("dev", "sess-a").await.unwrap();
        for _ in 0..5 {
            let seq = s.fenced_incr_seq("dev", "sess-a").await.unwrap();
            s.xadd_frame("dev", "sess-a", seq, vec![]).await.unwrap();
        }
        let current = s.reconcile_on_hello("dev", "sess-a", 3).await.unwrap();
        assert_eq!(current, 5);
        // 1..=3 trimmed; 4..=5 remain
        let frames = s.unacked_frames("dev", 0).await.unwrap();
        assert_eq!(frames.len(), 2);
    }

    #[tokio::test]
    async fn reconcile_bootstrap_path_seeds_seq_and_clears_buffer() {
        let s = store();
        s.try_acquire_lock("dev", "sess-a").await.unwrap();
        // Fresh store, last_ack=9 (the wedged-device case)
        let returned = s.reconcile_on_hello("dev", "sess-a", 9).await.unwrap();
        assert_eq!(returned, 9);
        // Next incr should produce 10
        assert_eq!(s.fenced_incr_seq("dev", "sess-a").await.unwrap(), 10);
        // No frames to replay
        assert!(s.unacked_frames("dev", 9).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn kick_subscriber_fires_on_publish() {
        let s = store();
        let mut sub = s.subscribe_kick("dev").await.unwrap();
        assert!(sub.recv.borrow_and_update() == &0);
        s.kick("dev", "new-sess").await.unwrap();
        // The watch::Receiver should observe a change.
        sub.recv.changed().await.unwrap();
        assert!(*sub.recv.borrow() >= 1);
    }

    #[tokio::test]
    async fn maxlen_caps_buffer() {
        let s = store();
        s.try_acquire_lock("dev", "sess-a").await.unwrap();
        for _ in 0..(MAX_BUFFER + 5) {
            let seq = s.fenced_incr_seq("dev", "sess-a").await.unwrap();
            s.xadd_frame("dev", "sess-a", seq, vec![]).await.unwrap();
        }
        let frames = s.unacked_frames("dev", 0).await.unwrap();
        assert_eq!(frames.len(), MAX_BUFFER);
    }
}
```

- [ ] **Step 3: Run tests, expect compile-time error (no impl)**

```bash
cargo test -p ahand-hub --lib ws::in_memory_outbox::tests
```

Expected: compile error — `InMemoryOutboxStore` does not implement `OutboxStore`.

- [ ] **Step 4: Implement the trait**

Add below the struct definition (still inside `in_memory_outbox.rs`):

```rust
#[async_trait]
impl OutboxStore for InMemoryOutboxStore {
    async fn try_acquire_lock(&self, device_id: &str, session_id: &str) -> Result<bool> {
        let mut g = self.inner.lock().await;
        let entry = g.entry(device_id.to_string()).or_default();
        if entry.lock.is_some() {
            return Ok(false);
        }
        entry.lock = Some(session_id.to_string());
        Ok(true)
    }

    async fn kick(&self, device_id: &str, _new_session_id: &str) -> Result<()> {
        let mut g = self.inner.lock().await;
        let entry = g.entry(device_id.to_string()).or_default();
        entry.kick_count = entry.kick_count.wrapping_add(1);
        if let Some(tx) = &entry.kick_tx {
            let _ = tx.send(entry.kick_count);
        }
        Ok(())
    }

    async fn subscribe_kick(&self, device_id: &str) -> Result<KickSubscription> {
        let mut g = self.inner.lock().await;
        let entry = g.entry(device_id.to_string()).or_default();
        let tx = match &entry.kick_tx {
            Some(tx) => tx.clone(),
            None => {
                let (tx, _rx) = watch::channel(0u64);
                entry.kick_tx = Some(tx.clone());
                tx
            }
        };
        let recv = tx.subscribe();
        // Memory impl does not need a background task; return a no-op handle.
        let _drop_guard = tokio::spawn(async {});
        Ok(KickSubscription { recv, _drop_guard })
    }

    async fn renew_lock(&self, device_id: &str, session_id: &str) -> Result<bool> {
        let g = self.inner.lock().await;
        Ok(g.get(device_id)
            .and_then(|e| e.lock.as_ref())
            .map(|owner| owner == session_id)
            .unwrap_or(false))
    }

    async fn release_lock(&self, device_id: &str, session_id: &str) -> Result<()> {
        let mut g = self.inner.lock().await;
        if let Some(entry) = g.get_mut(device_id) {
            if entry.lock.as_deref() == Some(session_id) {
                entry.lock = None;
            }
        }
        Ok(())
    }

    async fn reconcile_on_hello(
        &self,
        device_id: &str,
        session_id: &str,
        last_ack: u64,
    ) -> Result<u64> {
        let mut g = self.inner.lock().await;
        let entry = g.get_mut(device_id).ok_or_else(|| HubError::Unauthorized)?;
        if entry.lock.as_deref() != Some(session_id) {
            return Err(HubError::Unauthorized);
        }
        if last_ack > entry.seq {
            // Bootstrap path
            entry.seq = last_ack;
            entry.buffer.clear();
            return Ok(last_ack);
        }
        // Normal path: trim acked frames
        while let Some((seq, _)) = entry.buffer.front() {
            if *seq <= last_ack {
                entry.buffer.pop_front();
            } else {
                break;
            }
        }
        Ok(entry.seq)
    }

    async fn unacked_frames(&self, device_id: &str, last_ack: u64) -> Result<Vec<Vec<u8>>> {
        let g = self.inner.lock().await;
        Ok(g.get(device_id)
            .map(|e| {
                e.buffer
                    .iter()
                    .filter(|(seq, _)| *seq > last_ack)
                    .map(|(_, bytes)| bytes.clone())
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn fenced_incr_seq(&self, device_id: &str, session_id: &str) -> Result<u64> {
        let mut g = self.inner.lock().await;
        let entry = g.get_mut(device_id).ok_or_else(|| HubError::Unauthorized)?;
        if entry.lock.as_deref() != Some(session_id) {
            return Err(HubError::Unauthorized);
        }
        entry.seq += 1;
        Ok(entry.seq)
    }

    async fn xadd_frame(
        &self,
        device_id: &str,
        session_id: &str,
        seq: u64,
        frame: Vec<u8>,
    ) -> Result<()> {
        let mut g = self.inner.lock().await;
        let entry = g.get_mut(device_id).ok_or_else(|| HubError::Unauthorized)?;
        if entry.lock.as_deref() != Some(session_id) {
            return Err(HubError::Unauthorized);
        }
        entry.buffer.push_back((seq, frame));
        while entry.buffer.len() > MAX_BUFFER {
            entry.buffer.pop_front();
        }
        Ok(())
    }

    async fn observe_ack(&self, device_id: &str, ack: u64) -> Result<()> {
        let mut g = self.inner.lock().await;
        if let Some(entry) = g.get_mut(device_id) {
            while let Some((seq, _)) = entry.buffer.front() {
                if *seq <= ack {
                    entry.buffer.pop_front();
                } else {
                    break;
                }
            }
        }
        Ok(())
    }
}
```

- [ ] **Step 5: Run tests**

```bash
cargo test -p ahand-hub --lib ws::in_memory_outbox::tests
```

Expected: all 12 tests pass.

- [ ] **Step 6: Run the full hub test suite to confirm no regressions from the new module**

```bash
cargo test -p ahand-hub
```

Expected: pass.

- [ ] **Step 7: Commit**

```bash
git add crates/ahand-hub/src/ws/in_memory_outbox.rs crates/ahand-hub/src/ws/mod.rs
git commit -m "$(cat <<'EOF'
feat(hub): in-memory OutboxStore implementation

Implements OutboxStore trait with process-local state. Used by
StoreConfig::Memory and as the test fixture for the gateway refactor in
later tasks. Mirrors RedisOutboxStore semantics method-for-method
including the bootstrap branch in reconcile_on_hello.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Lua scripts module + EVALSHA wrapper

**Goal:** Centralize Lua script source strings and the `EVALSHA`-with-`NOSCRIPT`-fallback machinery so `RedisOutboxStore` can stay focused on protocol logic.

**Files:**
- Create: `crates/ahand-hub-store/src/outbox_lua.rs`
- Modify: `crates/ahand-hub-store/src/lib.rs` (add `pub mod outbox_lua;`)

**Acceptance Criteria:**
- [ ] All four scripts defined as `pub const &str`: `ACQUIRE_LOCK`, `RENEW_LOCK`, `RELEASE_LOCK`, `RECONCILE_ON_HELLO`, `FENCED_INCR_SEQ`, `FENCED_XADD`.
- [ ] `Scripts` struct caches SHA1s on construction (lazily computed via `redis::Script::get_hash`).
- [ ] `Scripts::eval` invokes `EVALSHA`; on `NOSCRIPT`, transparently falls back to `EVAL` and re-caches.
- [ ] Unit-testable against testcontainer Redis (verified by Task 3 tests).

**Verify:** `cargo check -p ahand-hub-store` → compiles. Behavior verified in Task 3.

**Steps:**

- [ ] **Step 1: Add module to `lib.rs`**

Edit `crates/ahand-hub-store/src/lib.rs` — add at the appropriate place alongside other `pub mod`:

```rust
pub mod outbox_lua;
```

- [ ] **Step 2: Create the Lua module**

Create `crates/ahand-hub-store/src/outbox_lua.rs`:

```rust
//! Lua scripts for the per-device outbox protocol on the hub side.
//!
//! All scripts are loaded once at construction (`SCRIPT LOAD`) and invoked
//! via `EVALSHA`. On `NOSCRIPT` (Redis restart, FLUSHALL), the wrapper
//! transparently falls back to `EVAL` and re-caches the SHA. Callers do
//! not need to handle script loading or SHA management.
//!
//! The scripts are designed to be the unit of atomicity. The Rust caller
//! is responsible for ordering the two-step send (`fenced_incr_seq` →
//! encode envelope with assigned seq → `fenced_xadd`); see the design doc
//! for the rationale.

use redis::aio::ConnectionManager;
use redis::{FromRedisValue, Script, ToRedisArgs};

pub const ACQUIRE_LOCK: &str = r#"
-- KEYS[1] = lock:device:{id}
-- ARGV[1] = session_id
-- ARGV[2] = ttl_secs
local ok = redis.call('SET', KEYS[1], ARGV[1], 'NX', 'EX', ARGV[2])
if ok then return 1 else return 0 end
"#;

pub const RENEW_LOCK: &str = r#"
-- KEYS[1] = lock:device:{id}
-- ARGV[1] = session_id
-- ARGV[2] = ttl_secs
if redis.call('GET', KEYS[1]) == ARGV[1] then
  redis.call('EXPIRE', KEYS[1], ARGV[2])
  return 1
end
return 0
"#;

pub const RELEASE_LOCK: &str = r#"
-- KEYS[1] = lock:device:{id}
-- ARGV[1] = session_id
if redis.call('GET', KEYS[1]) == ARGV[1] then
  return redis.call('DEL', KEYS[1])
end
return 0
"#;

pub const RECONCILE_ON_HELLO: &str = r#"
-- KEYS[1] = lock:device:{id}
-- KEYS[2] = seq:{id}
-- KEYS[3] = outbox:{id}
-- ARGV[1] = session_id
-- ARGV[2] = last_ack (decimal)
-- ARGV[3] = retention_secs
if redis.call('GET', KEYS[1]) ~= ARGV[1] then
  return redis.error_reply('NOT_OWNER')
end
local current = tonumber(redis.call('GET', KEYS[2])) or 0
local last_ack = tonumber(ARGV[2])
if last_ack > current then
  redis.call('SET', KEYS[2], last_ack)
  redis.call('DEL', KEYS[3])
  redis.call('EXPIRE', KEYS[2], ARGV[3])
  return last_ack
end
if last_ack > 0 then
  redis.call('XTRIM', KEYS[3], 'MINID', '0-' .. (last_ack + 1))
end
return current
"#;

pub const FENCED_INCR_SEQ: &str = r#"
-- KEYS[1] = lock:device:{id}
-- KEYS[2] = seq:{id}
-- ARGV[1] = session_id
-- ARGV[2] = retention_secs
if redis.call('GET', KEYS[1]) ~= ARGV[1] then
  return redis.error_reply('NOT_OWNER')
end
local seq = redis.call('INCR', KEYS[2])
redis.call('EXPIRE', KEYS[2], ARGV[2])
return seq
"#;

pub const FENCED_XADD: &str = r#"
-- KEYS[1] = lock:device:{id}
-- KEYS[2] = outbox:{id}
-- ARGV[1] = session_id
-- ARGV[2] = seq (decimal)
-- ARGV[3] = frame (binary)
-- ARGV[4] = max_buffer (decimal)
-- ARGV[5] = retention_secs
if redis.call('GET', KEYS[1]) ~= ARGV[1] then
  return redis.error_reply('NOT_OWNER')
end
local id = '0-' .. ARGV[2]
redis.call('XADD', KEYS[2], 'MAXLEN', '~', ARGV[4], id, 'frame', ARGV[3])
redis.call('EXPIRE', KEYS[2], ARGV[5])
return 1
"#;

/// Pre-built [`redis::Script`] handles. `redis::Script` itself caches the
/// SHA1 internally and uses `EVALSHA`-then-`EVAL` automatically, so this
/// type is mostly a named bundle so callers do not have to repeat the raw
/// strings everywhere.
pub struct OutboxScripts {
    pub acquire_lock: Script,
    pub renew_lock: Script,
    pub release_lock: Script,
    pub reconcile_on_hello: Script,
    pub fenced_incr_seq: Script,
    pub fenced_xadd: Script,
}

impl OutboxScripts {
    pub fn load() -> Self {
        Self {
            acquire_lock: Script::new(ACQUIRE_LOCK),
            renew_lock: Script::new(RENEW_LOCK),
            release_lock: Script::new(RELEASE_LOCK),
            reconcile_on_hello: Script::new(RECONCILE_ON_HELLO),
            fenced_incr_seq: Script::new(FENCED_INCR_SEQ),
            fenced_xadd: Script::new(FENCED_XADD),
        }
    }
}

/// Convenience wrapper used in this crate's tests; production code prefers
/// `Script::invoke_async` directly via `OutboxScripts`.
#[cfg(test)]
pub async fn eval_script<T: FromRedisValue>(
    script: &Script,
    conn: &mut ConnectionManager,
    keys: &[&str],
    args: &[&dyn ToRedisArgs],
) -> redis::RedisResult<T> {
    let mut invoke = script.prepare_invoke();
    for k in keys {
        invoke.key(*k);
    }
    for a in args {
        invoke.arg(*a);
    }
    invoke.invoke_async(conn).await
}
```

- [ ] **Step 3: Build and verify**

```bash
cargo check -p ahand-hub-store
```

Expected: compile clean.

- [ ] **Step 4: Commit**

```bash
git add crates/ahand-hub-store/src/outbox_lua.rs crates/ahand-hub-store/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(hub-store): outbox Lua scripts module

Six Lua scripts for the fenced outbox protocol: acquire/renew/release lock,
reconcile on hello (with bootstrap branch), fenced INCR seq, fenced XADD
frame. redis::Script handles EVALSHA→NOSCRIPT→EVAL transparently.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: `RedisOutboxStore` — lock primitives (acquire / kick / subscribe / renew / release)

**Goal:** Stand up `RedisOutboxStore` skeleton + the five lock-related methods. Does not include the send / reconcile / replay / observe_ack methods (those land in Task 4). Splitting at this seam keeps each task's diff focused and lets us run integration tests for the lock dance independently.

**Files:**
- Create: `crates/ahand-hub-store/src/outbox_store.rs`
- Modify: `crates/ahand-hub-store/src/lib.rs` (`pub mod outbox_store;`)
- Modify: `crates/ahand-hub-store/tests/store_roundtrip.rs` (add lock-flow tests reusing `TestStack`)

**Acceptance Criteria:**
- [ ] `RedisOutboxStore::new(redis_url) -> anyhow::Result<Self>` connects + caches scripts.
- [ ] `try_acquire_lock` round-trips `SET NX EX` correctly; concurrent acquirers see exactly one winner.
- [ ] `kick` publishes on `kick:{device_id}`.
- [ ] `subscribe_kick` returns a `KickSubscription` whose `recv` ticks on `PUBLISH`.
- [ ] `renew_lock` extends TTL only for the matching session_id.
- [ ] `release_lock` deletes only for the matching session_id.
- [ ] Integration tests pass against testcontainer Redis (Docker required).

**Verify:** `cargo test -p ahand-hub-store --features test-support --test store_roundtrip outbox_lock_` → all relevant tests pass.

**Steps:**

- [ ] **Step 1: Add module to `lib.rs`**

```rust
pub mod outbox_store;
```

- [ ] **Step 2: Skeleton + lock methods**

Create `crates/ahand-hub-store/src/outbox_store.rs`:

```rust
use std::sync::Arc;
use std::time::Duration;

use ahand_hub_core::traits::{KickSubscription, OutboxStore};
use ahand_hub_core::{HubError, Result};
use async_trait::async_trait;
use futures_util::StreamExt;
use redis::aio::ConnectionManager;
use redis::{AsyncCommands, Client};
use tokio::sync::{Mutex, watch};
use tokio::task::JoinHandle;

use crate::outbox_lua::OutboxScripts;

const LOCK_TTL_SECS: u64 = 30;
const RETENTION_SECS: u64 = 30 * 24 * 60 * 60; // 30 days
const STREAM_MAXLEN: u64 = 10_000;

#[derive(Clone)]
pub struct RedisOutboxStore {
    client: Client,
    conn: Arc<Mutex<ConnectionManager>>,
    scripts: Arc<OutboxScripts>,
}

impl RedisOutboxStore {
    pub async fn new(redis_url: &str) -> anyhow::Result<Self> {
        let client = Client::open(redis_url)?;
        let conn = crate::redis::connect_redis(redis_url).await?;
        Ok(Self {
            client,
            conn: Arc::new(Mutex::new(conn)),
            scripts: Arc::new(OutboxScripts::load()),
        })
    }

    fn lock_key(device_id: &str) -> String {
        format!("lock:device:{device_id}")
    }
    fn seq_key(device_id: &str) -> String {
        format!("seq:{device_id}")
    }
    fn outbox_key(device_id: &str) -> String {
        format!("outbox:{device_id}")
    }
    fn kick_channel(device_id: &str) -> String {
        format!("kick:{device_id}")
    }
}

fn redis_err(err: redis::RedisError) -> HubError {
    HubError::Internal(err.to_string())
}

#[async_trait]
impl OutboxStore for RedisOutboxStore {
    async fn try_acquire_lock(&self, device_id: &str, session_id: &str) -> Result<bool> {
        let mut conn = self.conn.lock().await;
        let lock_key = Self::lock_key(device_id);
        let result: i64 = self
            .scripts
            .acquire_lock
            .key(lock_key)
            .arg(session_id)
            .arg(LOCK_TTL_SECS)
            .invoke_async(&mut *conn)
            .await
            .map_err(redis_err)?;
        Ok(result == 1)
    }

    async fn kick(&self, device_id: &str, new_session_id: &str) -> Result<()> {
        let mut conn = self.conn.lock().await;
        let _: i64 = conn
            .publish(Self::kick_channel(device_id), new_session_id)
            .await
            .map_err(redis_err)?;
        Ok(())
    }

    async fn subscribe_kick(&self, device_id: &str) -> Result<KickSubscription> {
        let channel = Self::kick_channel(device_id);
        let (tx, rx) = watch::channel(0u64);
        let client = self.client.clone();

        // Long-lived background task: subscribes to the kick channel and bumps
        // the watch counter on each PUBLISH. Drops cleanly when the JoinHandle
        // is dropped from KickSubscription.
        let join: JoinHandle<()> = tokio::spawn(async move {
            let mut pubsub = match client.get_async_pubsub().await {
                Ok(p) => p,
                Err(_) => return,
            };
            if pubsub.subscribe(channel.as_str()).await.is_err() {
                return;
            }
            let mut stream = pubsub.on_message();
            let mut counter: u64 = 0;
            while let Some(_msg) = stream.next().await {
                counter = counter.wrapping_add(1);
                if tx.send(counter).is_err() {
                    break;
                }
            }
        });

        Ok(KickSubscription {
            recv: rx,
            _drop_guard: join,
        })
    }

    async fn renew_lock(&self, device_id: &str, session_id: &str) -> Result<bool> {
        let mut conn = self.conn.lock().await;
        let result: i64 = self
            .scripts
            .renew_lock
            .key(Self::lock_key(device_id))
            .arg(session_id)
            .arg(LOCK_TTL_SECS)
            .invoke_async(&mut *conn)
            .await
            .map_err(redis_err)?;
        Ok(result == 1)
    }

    async fn release_lock(&self, device_id: &str, session_id: &str) -> Result<()> {
        let mut conn = self.conn.lock().await;
        let _: i64 = self
            .scripts
            .release_lock
            .key(Self::lock_key(device_id))
            .arg(session_id)
            .invoke_async(&mut *conn)
            .await
            .map_err(redis_err)?;
        Ok(())
    }

    // ── stub implementations of the rest; filled in by Task 4 ──

    async fn reconcile_on_hello(
        &self,
        _device_id: &str,
        _session_id: &str,
        _last_ack: u64,
    ) -> Result<u64> {
        Err(HubError::Internal("reconcile_on_hello not implemented (Task 4)".into()))
    }

    async fn unacked_frames(&self, _device_id: &str, _last_ack: u64) -> Result<Vec<Vec<u8>>> {
        Err(HubError::Internal("unacked_frames not implemented (Task 4)".into()))
    }

    async fn fenced_incr_seq(&self, _device_id: &str, _session_id: &str) -> Result<u64> {
        Err(HubError::Internal("fenced_incr_seq not implemented (Task 4)".into()))
    }

    async fn xadd_frame(
        &self,
        _device_id: &str,
        _session_id: &str,
        _seq: u64,
        _frame: Vec<u8>,
    ) -> Result<()> {
        Err(HubError::Internal("xadd_frame not implemented (Task 4)".into()))
    }

    async fn observe_ack(&self, _device_id: &str, _ack: u64) -> Result<()> {
        Err(HubError::Internal("observe_ack not implemented (Task 4)".into()))
    }
}
```

- [ ] **Step 3: Wire `RedisOutboxStore` into the `TestStack`**

Edit `crates/ahand-hub-store/tests/support/mod.rs`:

```rust
// Add field:
pub struct TestStack {
    pub devices: ahand_hub_store::device_store::PgDeviceStore,
    pub jobs: ahand_hub_store::job_store::PgJobStore,
    pub audit: ahand_hub_store::audit_store::PgAuditStore,
    pub presence: ahand_hub_store::presence_store::RedisPresenceStore,
    pub outbox: ahand_hub_store::outbox_store::RedisOutboxStore,   // <— new
    database_url: String,
    redis_url: String,
    _postgres: ManagedContainer,
    _redis: ManagedContainer,
}

// In TestStack::start, after constructing presence:
let outbox = ahand_hub_store::outbox_store::RedisOutboxStore::new(&redis_url).await?;

// In the returned struct literal:
Ok(Self {
    // ... existing fields ...
    outbox,
    // ... existing fields ...
})
```

- [ ] **Step 4: Add lock-flow integration tests at the end of `store_roundtrip.rs`**

```rust
#[tokio::test]
async fn outbox_lock_acquire_and_release() -> anyhow::Result<()> {
    let stack = TestStack::start().await?;
    assert!(stack.outbox.try_acquire_lock("dev-lock-1", "sess-a").await?);
    assert!(!stack.outbox.try_acquire_lock("dev-lock-1", "sess-b").await?);
    stack.outbox.release_lock("dev-lock-1", "sess-a").await?;
    assert!(stack.outbox.try_acquire_lock("dev-lock-1", "sess-b").await?);
    Ok(())
}

#[tokio::test]
async fn outbox_lock_release_with_wrong_session_is_noop() -> anyhow::Result<()> {
    let stack = TestStack::start().await?;
    assert!(stack.outbox.try_acquire_lock("dev-lock-2", "sess-a").await?);
    stack.outbox.release_lock("dev-lock-2", "sess-other").await?;
    assert!(!stack.outbox.try_acquire_lock("dev-lock-2", "sess-b").await?);
    Ok(())
}

#[tokio::test]
async fn outbox_lock_renew_extends_ttl_for_owner_only() -> anyhow::Result<()> {
    let stack = TestStack::start().await?;
    stack.outbox.try_acquire_lock("dev-lock-3", "sess-a").await?;
    assert!(stack.outbox.renew_lock("dev-lock-3", "sess-a").await?);
    assert!(!stack.outbox.renew_lock("dev-lock-3", "sess-b").await?);
    Ok(())
}

#[tokio::test]
async fn outbox_kick_delivers_to_subscriber() -> anyhow::Result<()> {
    let stack = TestStack::start().await?;
    let mut sub = stack.outbox.subscribe_kick("dev-kick-1").await?;
    // Subscriber must be ready before publish; sleep briefly to let the
    // background task complete its SUBSCRIBE.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    stack.outbox.kick("dev-kick-1", "new-sess").await?;
    tokio::time::timeout(std::time::Duration::from_secs(2), sub.recv.changed())
        .await
        .expect("kick should arrive within 2s")?;
    assert!(*sub.recv.borrow() >= 1);
    Ok(())
}
```

- [ ] **Step 5: Run the lock tests**

```bash
cargo test -p ahand-hub-store --features test-support --test store_roundtrip outbox_lock outbox_kick
```

Expected: all 4 tests pass (Docker daemon required for testcontainer).

- [ ] **Step 6: Commit**

```bash
git add crates/ahand-hub-store/src/outbox_store.rs crates/ahand-hub-store/src/lib.rs crates/ahand-hub-store/tests/support/mod.rs crates/ahand-hub-store/tests/store_roundtrip.rs
git commit -m "$(cat <<'EOF'
feat(hub-store): RedisOutboxStore lock primitives

acquire/renew/release via Lua-checked SET NX EX. kick via PUBLISH;
subscribe_kick spawns a long-lived task that bumps a watch::Sender on
each message. Send/reconcile/replay/ack remain stubbed (return Internal
error) — landed in the next commit so this diff stays reviewable.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: `RedisOutboxStore` — send / reconcile / replay / observe_ack

**Goal:** Fill in the five remaining methods so `RedisOutboxStore` is complete.

**Files:**
- Modify: `crates/ahand-hub-store/src/outbox_store.rs`
- Modify: `crates/ahand-hub-store/tests/store_roundtrip.rs` (add data-path tests)

**Acceptance Criteria:**
- [ ] `fenced_incr_seq` returns monotonically increasing seqs for the lock owner; returns `Unauthorized` for non-owners.
- [ ] `xadd_frame` writes the frame at stream ID `0-{seq}`, applies `MAXLEN ~ 10000` and `EXPIRE`.
- [ ] `unacked_frames(device_id, last_ack)` returns frames in seq order, only entries with `seq > last_ack`.
- [ ] `observe_ack` trims via `XTRIM MINID 0-{ack+1}`.
- [ ] `reconcile_on_hello` normal path returns current seq, trims acked entries; bootstrap path (when `last_ack > current`) sets seq to `last_ack` and clears the stream, returns `last_ack`.
- [ ] All methods return `HubError::Unauthorized` when the fence rejects (i.e., `NOT_OWNER` from Lua).
- [ ] Integration tests pass.

**Verify:** `cargo test -p ahand-hub-store --features test-support --test store_roundtrip outbox_` → all pass.

**Steps:**

- [ ] **Step 1: Map `NOT_OWNER` Lua error to `HubError::Unauthorized`**

The `redis::error_reply('NOT_OWNER')` call in Lua surfaces in Rust as a `redis::RedisError` whose `code()` is `Some("NOT_OWNER")`. Add a helper at the top of `outbox_store.rs`:

```rust
fn map_redis_err(err: redis::RedisError) -> HubError {
    if err.code() == Some("NOT_OWNER") {
        HubError::Unauthorized
    } else {
        HubError::Internal(err.to_string())
    }
}
```

Replace existing `redis_err` calls in this file with `map_redis_err` so the Unauthorized mapping is uniform. Keep `redis_err` only where it's known the script can't return `NOT_OWNER` (e.g., the bare `PUBLISH` in `kick`).

- [ ] **Step 2: Implement `fenced_incr_seq`**

Replace the stub:

```rust
async fn fenced_incr_seq(&self, device_id: &str, session_id: &str) -> Result<u64> {
    let mut conn = self.conn.lock().await;
    let seq: u64 = self
        .scripts
        .fenced_incr_seq
        .key(Self::lock_key(device_id))
        .key(Self::seq_key(device_id))
        .arg(session_id)
        .arg(RETENTION_SECS)
        .invoke_async(&mut *conn)
        .await
        .map_err(map_redis_err)?;
    Ok(seq)
}
```

- [ ] **Step 3: Implement `xadd_frame`**

```rust
async fn xadd_frame(
    &self,
    device_id: &str,
    session_id: &str,
    seq: u64,
    frame: Vec<u8>,
) -> Result<()> {
    let mut conn = self.conn.lock().await;
    let _: i64 = self
        .scripts
        .fenced_xadd
        .key(Self::lock_key(device_id))
        .key(Self::outbox_key(device_id))
        .arg(session_id)
        .arg(seq.to_string())
        .arg(frame)
        .arg(STREAM_MAXLEN)
        .arg(RETENTION_SECS)
        .invoke_async(&mut *conn)
        .await
        .map_err(map_redis_err)?;
    Ok(())
}
```

- [ ] **Step 4: Implement `unacked_frames`**

`XRANGE outbox:{id} (0-{last_ack} +` reads all entries with stream ID strictly greater than `0-{last_ack}`. The redis crate's `xrange` exposes the entries as a `StreamRangeReply`.

```rust
async fn unacked_frames(&self, device_id: &str, last_ack: u64) -> Result<Vec<Vec<u8>>> {
    use redis::streams::StreamRangeReply;
    let mut conn = self.conn.lock().await;
    // XRANGE expects exclusive start prefixed with '('. last_ack=0 means
    // "everything"; encode that as start='-' so we don't synthesize 0-0.
    let start = if last_ack == 0 {
        "-".to_string()
    } else {
        format!("(0-{last_ack}")
    };
    let reply: StreamRangeReply = conn
        .xrange(Self::outbox_key(device_id), start, "+")
        .await
        .map_err(redis_err)?;
    let mut frames = Vec::with_capacity(reply.ids.len());
    for entry in reply.ids {
        // Each entry is a HashMap<String, redis::Value>; field key is "frame".
        if let Some(redis::Value::Data(bytes)) = entry.map.get("frame") {
            frames.push(bytes.clone());
        }
    }
    Ok(frames)
}
```

- [ ] **Step 5: Implement `observe_ack`**

```rust
async fn observe_ack(&self, device_id: &str, ack: u64) -> Result<()> {
    if ack == 0 {
        return Ok(());
    }
    let mut conn = self.conn.lock().await;
    // XTRIM outbox:{id} MINID 0-{ack+1}
    let minid = format!("0-{}", ack + 1);
    let _: i64 = redis::cmd("XTRIM")
        .arg(Self::outbox_key(device_id))
        .arg("MINID")
        .arg(minid)
        .query_async(&mut *conn)
        .await
        .map_err(redis_err)?;
    Ok(())
}
```

- [ ] **Step 6: Implement `reconcile_on_hello`**

```rust
async fn reconcile_on_hello(
    &self,
    device_id: &str,
    session_id: &str,
    last_ack: u64,
) -> Result<u64> {
    let mut conn = self.conn.lock().await;
    let returned: u64 = self
        .scripts
        .reconcile_on_hello
        .key(Self::lock_key(device_id))
        .key(Self::seq_key(device_id))
        .key(Self::outbox_key(device_id))
        .arg(session_id)
        .arg(last_ack.to_string())
        .arg(RETENTION_SECS)
        .invoke_async(&mut *conn)
        .await
        .map_err(map_redis_err)?;
    Ok(returned)
}
```

- [ ] **Step 7: Add data-path integration tests**

At the end of `store_roundtrip.rs`:

```rust
#[tokio::test]
async fn outbox_send_and_replay_roundtrip() -> anyhow::Result<()> {
    let stack = TestStack::start().await?;
    let dev = "dev-send-1";
    let sess = "sess-a";
    stack.outbox.try_acquire_lock(dev, sess).await?;

    let mut seqs = Vec::new();
    for i in 0..5u8 {
        let seq = stack.outbox.fenced_incr_seq(dev, sess).await?;
        stack.outbox.xadd_frame(dev, sess, seq, vec![i]).await?;
        seqs.push(seq);
    }
    assert_eq!(seqs, vec![1, 2, 3, 4, 5]);

    let frames = stack.outbox.unacked_frames(dev, 0).await?;
    assert_eq!(frames, vec![vec![0], vec![1], vec![2], vec![3], vec![4]]);

    let frames = stack.outbox.unacked_frames(dev, 2).await?;
    assert_eq!(frames, vec![vec![2], vec![3], vec![4]]);
    Ok(())
}

#[tokio::test]
async fn outbox_observe_ack_trims() -> anyhow::Result<()> {
    let stack = TestStack::start().await?;
    let dev = "dev-ack-1";
    let sess = "sess-a";
    stack.outbox.try_acquire_lock(dev, sess).await?;

    for i in 0..3u8 {
        let seq = stack.outbox.fenced_incr_seq(dev, sess).await?;
        stack.outbox.xadd_frame(dev, sess, seq, vec![i]).await?;
    }
    stack.outbox.observe_ack(dev, 2).await?;
    let frames = stack.outbox.unacked_frames(dev, 0).await?;
    assert_eq!(frames, vec![vec![2]]);
    Ok(())
}

#[tokio::test]
async fn outbox_reconcile_normal_path_returns_current_seq() -> anyhow::Result<()> {
    let stack = TestStack::start().await?;
    let dev = "dev-rec-normal";
    let sess = "sess-a";
    stack.outbox.try_acquire_lock(dev, sess).await?;
    for _ in 0..5 {
        let seq = stack.outbox.fenced_incr_seq(dev, sess).await?;
        stack.outbox.xadd_frame(dev, sess, seq, vec![]).await?;
    }
    let current = stack.outbox.reconcile_on_hello(dev, sess, 3).await?;
    assert_eq!(current, 5);
    let frames = stack.outbox.unacked_frames(dev, 0).await?;
    // 1..=3 trimmed; 4..=5 remain
    assert_eq!(frames.len(), 2);
    Ok(())
}

#[tokio::test]
async fn outbox_reconcile_bootstrap_path_seeds_and_clears() -> anyhow::Result<()> {
    let stack = TestStack::start().await?;
    let dev = "dev-rec-bootstrap";
    let sess = "sess-a";
    stack.outbox.try_acquire_lock(dev, sess).await?;
    // Fresh device, last_ack=9 — the wedged-after-restart case.
    let returned = stack.outbox.reconcile_on_hello(dev, sess, 9).await?;
    assert_eq!(returned, 9);
    let next = stack.outbox.fenced_incr_seq(dev, sess).await?;
    assert_eq!(next, 10);
    let frames = stack.outbox.unacked_frames(dev, 9).await?;
    assert!(frames.is_empty());
    Ok(())
}

#[tokio::test]
async fn outbox_send_rejects_non_owner() -> anyhow::Result<()> {
    let stack = TestStack::start().await?;
    let dev = "dev-fence";
    stack.outbox.try_acquire_lock(dev, "sess-a").await?;
    let err = stack.outbox.fenced_incr_seq(dev, "sess-b").await.unwrap_err();
    assert!(matches!(err, HubError::Unauthorized));
    Ok(())
}
```

(Add `use ahand_hub_core::HubError;` if not already present.)

- [ ] **Step 8: Run tests**

```bash
cargo test -p ahand-hub-store --features test-support --test store_roundtrip outbox_
```

Expected: all 9 outbox tests pass (4 lock + 5 data-path).

- [ ] **Step 9: Commit**

```bash
git add crates/ahand-hub-store/src/outbox_store.rs crates/ahand-hub-store/tests/store_roundtrip.rs
git commit -m "$(cat <<'EOF'
feat(hub-store): RedisOutboxStore send/reconcile/replay/observe_ack

Two-step send: fenced_incr_seq returns assigned seq, caller stamps the
envelope and re-encodes, then fenced_xadd writes the bytes. reconcile
on hello has the bootstrap branch — when device's last_ack exceeds
the store's current seq, seed the counter and clear the stream so
wedged devices auto-recover after this code ships.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Refactor `ConnectionRegistry` to use `OutboxStore`

**Goal:** Cut the in-memory `Outbox` out of `ConnectionRegistry`. Wire `Arc<dyn OutboxStore>` in, add session_id fencing, lease renewer task, kick subscriber task, and route the lock-contention path back as a clean error.

**Files:**
- Modify: `crates/ahand-hub/src/ws/device_gateway.rs`

**Acceptance Criteria:**
- [ ] `ConnectionRegistry::new(Arc<dyn OutboxStore>) -> Self` is the only constructor; `Default::default()` removed.
- [ ] `ConnectionEntry` no longer carries `outbox: Mutex<Outbox>`; carries `session_id: String` instead.
- [ ] `register(device_id, last_ack)` is now `async` and returns either success tuple or `HubError::OutboxLockContention` when the kick-then-retry cycle fails.
- [ ] `register` calls `OutboxStore::reconcile_on_hello` then `unacked_frames` and pushes replay frames into the per-connection mpsc.
- [ ] On accept, two background tasks are spawned per connection: lease renewer (every 10s) and kick subscriber. Either firing closes the WS via `close_tx`.
- [ ] `send` is fully fenced: `fenced_incr_seq` → mutate `envelope.seq` → encode → `xadd_frame`. On `Unauthorized` from either, close the connection and return `DeviceOffline`.
- [ ] `observe_ack` and `observe_inbound` delegate ack-trim to `OutboxStore::observe_ack` (fire-and-forget).
- [ ] `unregister` calls `OutboxStore::release_lock` (best-effort) and aborts both background tasks.
- [ ] Existing in-source tests (lines 903+ in `device_gateway.rs`) updated to use `InMemoryOutboxStore`.

**Verify:** `cargo test -p ahand-hub --lib ws::device_gateway::tests` → all pass.

**Steps:**

- [ ] **Step 1: Change struct definitions**

Replace the top of `device_gateway.rs` (the existing `ConnectionRegistry`/`ConnectionEntry`/`ActiveConnection` block) with:

```rust
use std::sync::Arc;

use ahand_hub_core::HubError;
use ahand_hub_core::traits::{KickSubscription, OutboxStore};
use ahand_hub_core::traits::DeviceStore;
use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use prost::Message;
use tokio::sync::{Mutex as AsyncMutex, mpsc, watch};
use tokio::task::JoinHandle;

use crate::state::AppState;

const LEASE_RENEW_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);
const LOCK_ACQUIRE_RETRIES: u32 = 5;
const LOCK_ACQUIRE_BACKOFF: std::time::Duration = std::time::Duration::from_millis(200);

pub struct ConnectionRegistry {
    senders: DashMap<String, ConnectionEntry>,
    outbox: Arc<dyn OutboxStore>,
}

struct ConnectionEntry {
    active: Option<ActiveConnection>,
}

#[derive(Clone)]
struct ActiveConnection {
    connection_id: uuid::Uuid,
    /// UUID acting as fencing token in every Redis script.
    session_id: String,
    sender: mpsc::UnboundedSender<OutboundFrame>,
    close_tx: watch::Sender<bool>,
    /// Aborted on close so the renewer stops attempting to renew.
    lease_task: Arc<AsyncMutex<Option<JoinHandle<()>>>>,
    /// Aborted on close so the kick subscriber stops listening.
    kick_task: Arc<AsyncMutex<Option<JoinHandle<()>>>>,
}

pub(crate) struct OutboundFrame {
    pub(crate) frame: Vec<u8>,
}

impl ConnectionRegistry {
    pub fn new(outbox: Arc<dyn OutboxStore>) -> Self {
        Self {
            senders: DashMap::new(),
            outbox,
        }
    }
}
```

- [ ] **Step 2: Rewrite `register` as async with lock dance**

```rust
impl ConnectionRegistry {
    pub(crate) async fn register(
        &self,
        device_id: String,
        last_ack: u64,
    ) -> Result<
        (
            uuid::Uuid,
            mpsc::UnboundedReceiver<OutboundFrame>,
            watch::Receiver<bool>,
        ),
        HubError,
    > {
        let session_id = uuid::Uuid::new_v4().to_string();

        // 1) Acquire lock with kick-then-retry.
        let mut acquired = self.outbox.try_acquire_lock(&device_id, &session_id).await?;
        if !acquired {
            self.outbox.kick(&device_id, &session_id).await?;
            for _ in 0..LOCK_ACQUIRE_RETRIES {
                tokio::time::sleep(LOCK_ACQUIRE_BACKOFF).await;
                acquired = self.outbox.try_acquire_lock(&device_id, &session_id).await?;
                if acquired {
                    break;
                }
            }
        }
        if !acquired {
            return Err(HubError::OutboxLockContention(device_id));
        }

        // 2) Reconcile + read replay frames BEFORE we wire the in-process state,
        //    so a failure during reconcile leaves the lock in a clean state via
        //    the release_lock in the error arm.
        if let Err(err) = self
            .outbox
            .reconcile_on_hello(&device_id, &session_id, last_ack)
            .await
        {
            let _ = self.outbox.release_lock(&device_id, &session_id).await;
            return Err(err);
        }
        let replay = self
            .outbox
            .unacked_frames(&device_id, last_ack)
            .await
            .unwrap_or_default();

        // 3) Build per-connection state.
        let (tx, rx) = mpsc::unbounded_channel();
        let connection_id = uuid::Uuid::new_v4();
        let (close_tx, close_rx) = watch::channel(false);

        // 4) Spawn lease renewer.
        let lease_task = {
            let outbox = self.outbox.clone();
            let device_id = device_id.clone();
            let session_id = session_id.clone();
            let close_tx = close_tx.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(LEASE_RENEW_INTERVAL);
                interval.tick().await; // first tick is immediate; skip
                loop {
                    interval.tick().await;
                    match outbox.renew_lock(&device_id, &session_id).await {
                        Ok(true) => continue,
                        Ok(false) | Err(_) => {
                            tracing::warn!(
                                device_id = %device_id,
                                session_id = %session_id,
                                "lease lost, signalling close"
                            );
                            let _ = close_tx.send(true);
                            break;
                        }
                    }
                }
            })
        };

        // 5) Spawn kick subscriber.
        let kick_task = {
            let outbox = self.outbox.clone();
            let device_id = device_id.clone();
            let close_tx = close_tx.clone();
            tokio::spawn(async move {
                let mut sub = match outbox.subscribe_kick(&device_id).await {
                    Ok(s) => s,
                    Err(err) => {
                        tracing::warn!(
                            device_id = %device_id,
                            error = %err,
                            "failed to subscribe to kick channel"
                        );
                        return;
                    }
                };
                if sub.recv.changed().await.is_ok() {
                    tracing::info!(device_id = %device_id, "received kick, signalling close");
                    let _ = close_tx.send(true);
                }
            })
        };

        let active = ActiveConnection {
            connection_id,
            session_id: session_id.clone(),
            sender: tx.clone(),
            close_tx: close_tx.clone(),
            lease_task: Arc::new(AsyncMutex::new(Some(lease_task))),
            kick_task: Arc::new(AsyncMutex::new(Some(kick_task))),
        };

        // 6) Replay first, then publish the active connection.
        for frame in replay {
            let _ = tx.send(OutboundFrame { frame });
        }
        match self.senders.entry(device_id) {
            dashmap::mapref::entry::Entry::Occupied(mut entry) => {
                let entry = entry.get_mut();
                if let Some(prev) = entry.active.replace(active) {
                    let _ = prev.close_tx.send(true);
                }
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                entry.insert(ConnectionEntry {
                    active: Some(active),
                });
            }
        }
        Ok((connection_id, rx, close_rx))
    }
}
```

- [ ] **Step 3: Rewrite `send`**

Replace the existing `send` (was around line 96–145):

```rust
impl ConnectionRegistry {
    pub(crate) async fn send(
        &self,
        device_id: &str,
        mut envelope: ahand_protocol::Envelope,
    ) -> anyhow::Result<()> {
        let (sender, session_id, connection_id) = {
            let entry = self
                .senders
                .get(device_id)
                .ok_or_else(|| HubError::DeviceOffline(device_id.into()))?;
            let active = entry
                .active
                .as_ref()
                .ok_or_else(|| HubError::DeviceOffline(device_id.into()))?;
            (
                active.sender.clone(),
                active.session_id.clone(),
                active.connection_id,
            )
        };

        let seq = match self.outbox.fenced_incr_seq(device_id, &session_id).await {
            Ok(s) => s,
            Err(HubError::Unauthorized) => {
                self.fail_connection(device_id, connection_id);
                return Err(HubError::DeviceOffline(device_id.into()).into());
            }
            Err(err) => return Err(err.into()),
        };
        envelope.seq = seq;
        let frame = envelope.encode_to_vec();

        if let Err(err) = self
            .outbox
            .xadd_frame(device_id, &session_id, seq, frame.clone())
            .await
        {
            if matches!(err, HubError::Unauthorized) {
                self.fail_connection(device_id, connection_id);
                return Err(HubError::DeviceOffline(device_id.into()).into());
            }
            return Err(err.into());
        }

        if sender.send(OutboundFrame { frame }).is_err() {
            // The WS IO task is gone; the message stays in the durable
            // stream and will replay on next reconnect.
            self.fail_connection(device_id, connection_id);
            return Err(HubError::DeviceOffline(device_id.into()).into());
        }
        Ok(())
    }

    fn fail_connection(&self, device_id: &str, connection_id: uuid::Uuid) {
        if let Some(mut entry) = self.senders.get_mut(device_id) {
            if entry
                .active
                .as_ref()
                .map(|a| a.connection_id == connection_id)
                .unwrap_or(false)
            {
                if let Some(active) = entry.active.take() {
                    let _ = active.close_tx.send(true);
                }
            }
        }
    }
}
```

- [ ] **Step 4: Rewrite `observe_ack` / `observe_inbound`**

The old code called `outbox.try_on_peer_ack(ack)` and could return `InvalidPeerAck`. With Redis as source of truth, the device's monotonic ack is just trimmed against the durable stream — there's no "ack from the future" failure mode anymore (the bootstrap branch handles the wedged-device case at register time). Rejecting an ack would also be hostile because the durable stream is the new authority.

```rust
impl ConnectionRegistry {
    pub(crate) async fn observe_ack(&self, device_id: &str, ack: u64) -> anyhow::Result<()> {
        if ack == 0 {
            return Ok(());
        }
        // Fire-and-forget; failure is logged inside the store impl, and the
        // next successful ack or MAXLEN trim will catch up.
        if let Err(err) = self.outbox.observe_ack(device_id, ack).await {
            tracing::warn!(device_id = %device_id, ack = ack, error = %err, "outbox observe_ack failed");
        }
        Ok(())
    }

    pub(crate) async fn observe_inbound(
        &self,
        device_id: &str,
        _seq: u64,
        ack: u64,
    ) -> anyhow::Result<()> {
        // Local-ack tracking ("have I already seen this seq from the device?")
        // is handled at the WS handler layer via `has_seen_inbound`; here we
        // only mirror peer acks into the outbox.
        self.observe_ack(device_id, ack).await
    }
}
```

`has_seen_inbound` was previously implemented against the in-memory `Outbox`. With the in-memory cache gone, dedup by seq must move into per-connection state if needed elsewhere. Audit call sites:

```bash
grep -rn "has_seen_inbound" crates/ahand-hub/src
```

If the only call site is inside the gateway loop, port to a `tokio::sync::Mutex<HashSet<u64>>` per connection. If used outside, replace it with a per-`ActiveConnection` ` last_inbound_seq: AtomicU64` and check `seq > last_inbound_seq.load()` before processing.

- [ ] **Step 5: Rewrite `unregister`**

```rust
impl ConnectionRegistry {
    pub(crate) async fn unregister(
        &self,
        device_id: &str,
        connection_id: uuid::Uuid,
    ) -> anyhow::Result<bool> {
        let session_to_release = if let Some(mut entry) = self.senders.get_mut(device_id) {
            if entry
                .active
                .as_ref()
                .map(|a| a.connection_id == connection_id)
                .unwrap_or(false)
            {
                let active = entry.active.take().expect("checked above");
                let _ = active.close_tx.send(true);
                if let Some(handle) = active.lease_task.lock().await.take() {
                    handle.abort();
                }
                if let Some(handle) = active.kick_task.lock().await.take() {
                    handle.abort();
                }
                Some(active.session_id)
            } else {
                None
            }
        } else {
            None
        };
        // Drop the DashMap guard before await on release_lock.
        drop(self.senders.get(device_id));
        if let Some(session_id) = session_to_release {
            if let Err(err) = self.outbox.release_lock(device_id, &session_id).await {
                tracing::warn!(device_id = %device_id, error = %err, "release_lock failed");
            }
            // No active = nothing to keep around in the local map.
            self.senders.remove(device_id);
            return Ok(true);
        }
        Ok(false)
    }
}
```

- [ ] **Step 6: Update existing in-source tests**

The existing tests (`mod tests` near the bottom of `device_gateway.rs`) need:
- Replace `ConnectionRegistry::default()` with `ConnectionRegistry::new(Arc::new(InMemoryOutboxStore::new()))`.
- Replace `registry.senders.get("device-1")` patterns with assertions on observable behavior (e.g., "was the frame delivered to the mpsc?") rather than peeking into the `outbox` field that no longer exists.
- Existing `register_replays_only_messages_after_last_ack` test needs adapting — instead of asserting on internal `Outbox` state, simulate: register session A, send 5 frames, drop session A's connection, register session A' with `last_ack=2`, drain the mpsc, assert it contains frames 3..5.

Skeleton replacement for that test:

```rust
#[tokio::test]
async fn register_replays_only_messages_after_last_ack() {
    let outbox = Arc::new(InMemoryOutboxStore::new());
    let registry = ConnectionRegistry::new(outbox);

    let (conn_a, mut rx_a, _close_a) = registry.register("dev-1".into(), 0).await.unwrap();
    for i in 0..5u8 {
        let envelope = ahand_protocol::Envelope {
            device_id: "dev-1".into(),
            ..Default::default()
        };
        registry.send("dev-1", envelope).await.unwrap();
        let _frame = rx_a.recv().await.expect("frame delivered");
        let _ = i;
    }
    // Simulate session A dropping; in-process, unregister releases the lock.
    registry.unregister("dev-1", conn_a).await.unwrap();

    // New session reconnects with last_ack=2 — only frames 3..5 should replay.
    let (_conn_b, mut rx_b, _close_b) = registry.register("dev-1".into(), 2).await.unwrap();
    let mut replayed = Vec::new();
    while let Ok(frame) = tokio::time::timeout(std::time::Duration::from_millis(50), rx_b.recv()).await {
        if let Some(f) = frame {
            replayed.push(f);
        } else {
            break;
        }
    }
    assert_eq!(replayed.len(), 3, "only seqs 3..=5 should replay");
}
```

- [ ] **Step 7: Update call sites**

The handler at `device_gateway.rs:480-482` becomes async-aware (it already is — that block lives in an `async` block):

```rust
let (connection_id, mut outbound_rx, close_rx) = state
    .connections
    .register(device_id.clone(), hello.last_ack)
    .await
    .map_err(|err| match err {
        HubError::OutboxLockContention(_) => {
            // Treat lock contention as a transient handshake failure;
            // the daemon's reconnect backoff will retry.
            anyhow::Error::from(err)
        }
        other => anyhow::Error::from(other),
    })?;
```

Also any call to `registry.observe_ack(...)` or `registry.observe_inbound(...)` becomes `.await`.

Run:

```bash
grep -rn "\.connections\.observe_ack\|\.connections\.observe_inbound\|\.connections\.send_envelope\|\.connections\.send(" crates/ahand-hub/src
```

Verify each call site is in an `async fn` and prefix with `.await`. (Most already are because the surrounding handlers are async.) `send_envelope` is the public wrapper around `send` — make it `pub async fn`.

- [ ] **Step 8: Run tests**

```bash
cargo check -p ahand-hub
cargo test -p ahand-hub --lib ws::device_gateway::tests
cargo test -p ahand-hub
```

Expected: all pass. Some unrelated tests in `ahand-hub` may need `.await` added at call sites — fix those mechanically as the compiler reports them.

- [ ] **Step 9: Commit**

```bash
git add crates/ahand-hub/src/ws/device_gateway.rs
git commit -m "$(cat <<'EOF'
refactor(hub/ws): wire ConnectionRegistry to OutboxStore

Per-connection session_id is now the fencing token across the registry
+ lease renewer + kick subscriber. send is fully fenced (fenced_incr_seq
→ encode → xadd_frame); on Unauthorized either side the connection is
torn down and the daemon's reconnect handles the rest. Lock contention
on register surfaces as HubError::OutboxLockContention.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Wire `OutboxStore` into `AppState`

**Goal:** Construct an `Arc<dyn OutboxStore>` based on `StoreConfig` and pass it to `ConnectionRegistry::new`.

**Files:**
- Modify: `crates/ahand-hub/src/state.rs`

**Acceptance Criteria:**
- [ ] `StoreConfig::Memory` builds an `Arc::new(InMemoryOutboxStore::new())`.
- [ ] `StoreConfig::Persistent { redis_url, .. }` builds an `Arc::new(RedisOutboxStore::new(redis_url).await?)`.
- [ ] `ConnectionRegistry::new(outbox.clone())` is the only constructor invocation.
- [ ] No call sites still reference `ConnectionRegistry::default()`.

**Verify:** `cargo check -p ahand-hub && cargo test -p ahand-hub` → green.

**Steps:**

- [ ] **Step 1: Update imports**

Add at the top of `state.rs`:

```rust
use ahand_hub_core::traits::OutboxStore;
use ahand_hub_store::outbox_store::RedisOutboxStore;
use crate::ws::in_memory_outbox::InMemoryOutboxStore;
```

- [ ] **Step 2: Construct outbox in `from_config`**

Inside `AppState::from_config`, replace the `match &config.store` block by extending the destructured tuple to include `outbox`:

```rust
let (
    devices,
    jobs_store,
    raw_audit_store,
    persistent_output,
    persistent_fanout,
    bootstrap_tokens,
    webhook_delivery_store,
    outbox,                                              // <— new
) = match &config.store {
    crate::config::StoreConfig::Memory => (
        Arc::new(MemoryDeviceStore::default()),
        Arc::new(MemoryJobStore::default()) as Arc<dyn JobStore>,
        Arc::new(MemoryAuditStore::default()) as Arc<dyn AuditStore>,
        None,
        None,
        crate::bootstrap::BootstrapCredentials::memory(),
        Arc::new(MemoryWebhookDeliveryStore::new()) as Arc<dyn WebhookDeliveryStore>,
        Arc::new(InMemoryOutboxStore::new()) as Arc<dyn OutboxStore>,
    ),
    crate::config::StoreConfig::Persistent {
        database_url,
        redis_url,
    } => {
        // ... existing setup unchanged ...
        let outbox_store = RedisOutboxStore::new(redis_url).await?;
        (
            // ... existing fields unchanged ...
            Arc::new(MemoryWebhookDeliveryStore::new()) as Arc<dyn WebhookDeliveryStore>,  // <-- replace with PgWebhookDeliveryStore as in current code
            Arc::new(outbox_store) as Arc<dyn OutboxStore>,
        )
    }
};
```

(Keep existing `PgWebhookDeliveryStore` usage in the persistent branch; the snippet above is just illustrating the shape.)

- [ ] **Step 3: Pass to `ConnectionRegistry`**

Change:

```rust
let connections = Arc::new(crate::ws::device_gateway::ConnectionRegistry::default());
```

to:

```rust
let connections = Arc::new(crate::ws::device_gateway::ConnectionRegistry::new(outbox));
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p ahand-hub
```

Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates/ahand-hub/src/state.rs
git commit -m "$(cat <<'EOF'
feat(hub): wire OutboxStore into AppState

Memory config uses InMemoryOutboxStore; Persistent uses RedisOutboxStore
via the existing REDIS_URL. ConnectionRegistry::new takes the store as
its only constructor arg; default() removed.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: End-to-end regression tests

**Goal:** Codify the original incident as a regression test plus two adjacent scenarios. These live in a new integration test file so they run independently of the unit tests.

**Files:**
- Create: `crates/ahand-hub/tests/outbox_persistence.rs`

**Acceptance Criteria:**
- [ ] **Replay-after-restart** test: send N frames via store A, drop A, build store B against the same Redis, register a new session with the original `last_ack`, verify all unsent frames replay.
- [ ] **Lock takeover** test: replica A holds the lock, replica B's `register` publishes kick → A's kick subscriber fires close → B acquires the lock within ≤ 5s.
- [ ] **Bootstrap-path** test: build a fresh store, register a session with `last_ack=9`, verify `register` succeeds, sends start at seq 10, no spurious replay.
- [ ] **Original incident regression** test (the keystone): two `RedisOutboxStore` instances against the same Redis simulating "before deploy" and "after deploy". Send 5 frames before; on after-deploy register with `last_ack=2`, expect frames 3..5 to replay (not Broken pipe).

**Verify:** `cargo test -p ahand-hub --test outbox_persistence` → all pass (Docker required).

**Steps:**

- [ ] **Step 1: Create the test file**

Create `crates/ahand-hub/tests/outbox_persistence.rs`:

```rust
//! End-to-end regression tests for hub outbox persistence.
//!
//! These tests boot a real Redis container via testcontainers, exercise
//! `RedisOutboxStore` and `ConnectionRegistry` together, and verify the
//! key invariants from the design spec.

use std::sync::Arc;
use std::time::Duration;

use ahand_hub::ws::device_gateway::ConnectionRegistry;
use ahand_hub_core::traits::OutboxStore;
use ahand_hub_store::outbox_store::RedisOutboxStore;
use testcontainers::{
    ContainerAsync, GenericImage, ImageExt,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
};

async fn redis_container() -> anyhow::Result<(ContainerAsync<GenericImage>, String)> {
    let container = GenericImage::new("redis", "7-alpine")
        .with_exposed_port(6379.tcp())
        .with_wait_for(WaitFor::message_on_stdout("Ready to accept connections"))
        .start()
        .await?;
    let port = container.get_host_port_ipv4(6379.tcp()).await?;
    Ok((container, format!("redis://127.0.0.1:{port}")))
}

#[tokio::test]
async fn replay_after_simulated_hub_restart() -> anyhow::Result<()> {
    let (_redis, url) = redis_container().await?;

    // Phase 1: hub instance A handles a session for dev-1 and sends 5 frames.
    let store_a = Arc::new(RedisOutboxStore::new(&url).await?);
    let registry_a = ConnectionRegistry::new(store_a.clone());
    let (_conn_a, mut rx_a, _close_a) = registry_a.register("dev-1".into(), 0).await?;
    for _ in 0..5 {
        let envelope = ahand_protocol::Envelope {
            device_id: "dev-1".into(),
            ..Default::default()
        };
        registry_a.send_envelope("dev-1", envelope).await?;
        let _ = rx_a.recv().await.expect("frame delivered");
    }
    // Device acked frames 1 and 2 before A "dies".
    registry_a.observe_ack("dev-1", 2).await?;

    // Drop A — simulates ECS task termination. Frames are still in Redis.
    drop(registry_a);
    drop(store_a);

    // Phase 2: hub instance B starts up and the device reconnects with last_ack=2.
    let store_b = Arc::new(RedisOutboxStore::new(&url).await?);
    let registry_b = ConnectionRegistry::new(store_b);
    let (_conn_b, mut rx_b, _close_b) = registry_b.register("dev-1".into(), 2).await?;

    // Frames 3..=5 should replay.
    let mut replayed = 0;
    while let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(200), rx_b.recv()).await {
        replayed += 1;
    }
    assert_eq!(replayed, 3, "expected 3 frames (seq 3..=5) to replay");
    Ok(())
}

#[tokio::test]
async fn lock_takeover_via_kick() -> anyhow::Result<()> {
    let (_redis, url) = redis_container().await?;
    let store_a = Arc::new(RedisOutboxStore::new(&url).await?);
    let store_b = Arc::new(RedisOutboxStore::new(&url).await?);
    let registry_a = ConnectionRegistry::new(store_a.clone());
    let registry_b = ConnectionRegistry::new(store_b.clone());

    let (_conn_a, _rx_a, mut close_a) = registry_a.register("dev-2".into(), 0).await?;
    // B's register should kick A and acquire within retry window.
    let started = tokio::time::Instant::now();
    let (_conn_b, _rx_b, _close_b) = registry_b.register("dev-2".into(), 0).await?;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "kick-and-acquire took {:?}, expected <5s",
        started.elapsed()
    );

    // A's close_rx should fire as a result of the kick.
    tokio::time::timeout(Duration::from_secs(2), close_a.changed())
        .await
        .expect("A's close_tx should fire after kick")?;
    assert!(*close_a.borrow_and_update());
    Ok(())
}

#[tokio::test]
async fn bootstrap_path_unblocks_wedged_device() -> anyhow::Result<()> {
    let (_redis, url) = redis_container().await?;

    // Fresh Redis (= just-deployed hub). Device sends Hello with last_ack=9
    // (the wedged-after-restart case from the original incident).
    let store = Arc::new(RedisOutboxStore::new(&url).await?);
    let registry = ConnectionRegistry::new(store.clone());
    let (_conn, mut rx, _close) = registry.register("dev-3".into(), 9).await?;

    // No frames to replay (nothing was in the stream).
    assert!(
        tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .is_err(),
        "no replay expected"
    );

    // Next send should produce seq=10.
    let envelope = ahand_protocol::Envelope {
        device_id: "dev-3".into(),
        ..Default::default()
    };
    registry.send_envelope("dev-3", envelope).await?;
    let frame = rx.recv().await.expect("frame delivered");
    let decoded = <ahand_protocol::Envelope as prost::Message>::decode(frame.as_slice())?;
    assert_eq!(decoded.seq, 10);
    Ok(())
}

#[tokio::test]
async fn original_incident_regression() -> anyhow::Result<()> {
    // This is the test that, had it existed, would have caught the original
    // bug: hub restart wipes outbox state, device's last_ack is now > server's
    // max issued seq, server rejects with InvalidPeerAck, daemon sees Broken
    // pipe in a tight reconnect loop.
    let (_redis, url) = redis_container().await?;

    // Phase 1: device connects and receives some frames.
    let store_a = Arc::new(RedisOutboxStore::new(&url).await?);
    let registry_a = ConnectionRegistry::new(store_a.clone());
    let (conn_a, mut rx_a, _close_a) = registry_a.register("dev-incident".into(), 0).await?;
    for _ in 0..5 {
        registry_a
            .send_envelope(
                "dev-incident",
                ahand_protocol::Envelope {
                    device_id: "dev-incident".into(),
                    ..Default::default()
                },
            )
            .await?;
        rx_a.recv().await.expect("frame delivered");
    }
    // Device acked all 5.
    registry_a.observe_ack("dev-incident", 5).await?;
    registry_a.unregister("dev-incident", conn_a).await?;

    // Phase 2: simulate hub deploy — B is a fresh process holding a *different*
    // OutboxStore arc, but pointing at the same Redis (durable layer survives).
    let store_b = Arc::new(RedisOutboxStore::new(&url).await?);
    let registry_b = ConnectionRegistry::new(store_b);

    // Device reconnects with last_ack=5. With the fix, register succeeds; without
    // the fix (in-memory state), this would be `InvalidPeerAck`.
    let (_conn_b, _rx_b, _close_b) = registry_b
        .register("dev-incident".into(), 5)
        .await
        .expect("device should reconnect cleanly post-restart");
    Ok(())
}
```

- [ ] **Step 2: Run the regression suite**

```bash
cargo test -p ahand-hub --test outbox_persistence
```

Expected: all 4 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/ahand-hub/tests/outbox_persistence.rs
git commit -m "$(cat <<'EOF'
test(hub): end-to-end regression for outbox persistence

Four scenarios: replay-after-restart, lock takeover via kick, bootstrap
path for wedged devices, and the original incident regression
(simulates hub deploy with the device's last_ack > 0). The last test
is the keystone — had it existed before today's incident, the bug
would not have shipped.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Final Verification

After all tasks land:

```bash
cargo fmt --all
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

Expected: clean fmt, zero clippy warnings, all tests green. The CI workflow at `.github/workflows/hub-ci.yml` (or equivalent) will gate the same.

## Spec Coverage Map

| Spec section | Implemented in |
|---|---|
| Background / Goals / Non-Goals | Reflected in commit messages and task framing |
| High-Level Architecture | Tasks 0, 1, 3, 4, 5, 6 |
| Module Boundaries | File Structure table above |
| `OutboxStore` Trait | Task 0 |
| Redis Schema | Task 4 (key construction in `RedisOutboxStore`) |
| Lua Scripts (6) | Task 2 |
| Connection Lifecycle / register | Task 5 (Step 2) |
| Connection Lifecycle / send + Seq encoding | Task 5 (Step 3) |
| Connection Lifecycle / observe_ack | Task 5 (Step 4) |
| Connection Lifecycle / unregister | Task 5 (Step 5) |
| Bootstrap Path | Task 1 (memory) + Task 2 (Lua) + Task 4 (Redis) + Task 7 (regression test) |
| Failure Modes / Redis unreachable | Task 6 (`from_config` returns `?` on connect failure) — `register` itself returns `HubError::Internal` which the WS handler maps to a close frame; explicit close-frame text is left to the existing handler match |
| Failure Modes / NOSCRIPT | `redis::Script` handles automatically (Task 2 note) |
| Failure Modes / lease lost | Task 5 (Step 2 — lease task signals close_tx on `Ok(false)` or `Err`) |
| Failure Modes / lock contention | Task 5 (Step 2 — `OutboxLockContention` after retries) |
| Failure Modes / MAXLEN trim | Task 4 (Step 3 — `MAXLEN ~ 10000` in fenced_xadd) |
| Failure Modes / crash between scripts | Task 4 — seq gap is harmless; covered by replay test in Task 7 |
| Test Matrix | Tasks 1, 3, 4, 5, 7 |
| Deployment / Rollout | No code changes — operational, executed manually after merge |
| Backwards Compatibility | Preserved by design (no protocol changes); verified by existing in-source tests in Task 5 |
