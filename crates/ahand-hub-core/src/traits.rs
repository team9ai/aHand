use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::Result;
use crate::audit::{AuditEntry, AuditFilter};
use crate::device::{Device, NewDevice};
use crate::job::{Job, JobFilter, JobStatus, NewJob};

#[async_trait]
pub trait DeviceStore: Send + Sync {
    async fn insert(&self, device: NewDevice) -> Result<Device>;
    async fn get(&self, device_id: &str) -> Result<Option<Device>>;
    async fn list(&self) -> Result<Vec<Device>>;
    async fn delete(&self, device_id: &str) -> Result<()>;
}

/// Admin-plane operations on a device store. Implemented by the same
/// backends that implement [`DeviceStore`], but split out into its own
/// trait so callers that only need read/write ops (e.g. the dispatcher)
/// don't need to know about the admin surface. The trait is intentionally
/// additive — existing [`DeviceStore`] consumers are untouched.
#[async_trait]
pub trait DeviceAdminStore: Send + Sync {
    /// Idempotent pre-register:
    /// - if no row exists, insert `(device_id, public_key, external_user_id)`
    /// - if a row exists with matching `external_user_id` AND matching
    ///   `public_key`, return the existing row unchanged
    /// - if a row exists with a different `external_user_id`, return
    ///   [`crate::HubError::DeviceOwnedByDifferentUser`]
    ///
    /// Returns `(Device, registered_at)` where `registered_at` is the stable
    /// timestamp from the DB row (i.e. the first time the device was inserted,
    /// not `Utc::now()` on each call).
    async fn pre_register(
        &self,
        device_id: &str,
        public_key: &[u8],
        external_user_id: &str,
    ) -> Result<(Device, DateTime<Utc>)>;

    async fn find_by_id(&self, device_id: &str) -> Result<Option<Device>>;

    /// Delete returns true if a row was removed, false if it didn't
    /// exist. Distinguishes "idempotent no-op" from "something changed"
    /// for the admin API's 404 path.
    async fn delete_device(&self, device_id: &str) -> Result<bool>;

    async fn list_by_external_user(&self, external_user_id: &str) -> Result<Vec<Device>>;
}

#[async_trait]
pub trait JobStore: Send + Sync {
    async fn insert(&self, job: NewJob) -> Result<Job>;
    async fn get(&self, job_id: &str) -> Result<Option<Job>>;
    async fn list(&self, filter: JobFilter) -> Result<Vec<Job>>;
    async fn transition_status(&self, job_id: &str, status: JobStatus)
    -> Result<Option<JobStatus>>;
    async fn update_status(&self, job_id: &str, status: JobStatus) -> Result<()>;
    async fn update_terminal(
        &self,
        job_id: &str,
        exit_code: i32,
        error: &str,
        output_summary: &str,
    ) -> Result<()>;
}

#[async_trait]
pub trait AuditStore: Send + Sync {
    async fn append(&self, entries: &[AuditEntry]) -> Result<()>;
    async fn query(&self, filter: AuditFilter) -> Result<Vec<AuditEntry>>;

    async fn prune_before(&self, _cutoff: DateTime<Utc>) -> Result<u64> {
        Ok(0)
    }
}

// ── Outbox persistence (hub→device durable buffer + multi-replica fencing) ──

/// Wrapper around a [`tokio::task::JoinHandle`] that aborts the underlying
/// task when dropped. Used by [`KickSubscription`] so downstream impls
/// don't each have to remember to abort their background reader task.
pub struct AbortOnDropHandle(tokio::task::JoinHandle<()>);

impl AbortOnDropHandle {
    pub fn new(handle: tokio::task::JoinHandle<()>) -> Self {
        Self(handle)
    }
}

impl Drop for AbortOnDropHandle {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Subscription handle returned by [`OutboxStore::subscribe_kick`]. The
/// receiver value increments whenever a kick is published on the device's
/// channel. Drop releases the underlying Pub/Sub connection and aborts the
/// background reader task.
pub struct KickSubscription {
    pub recv: tokio::sync::watch::Receiver<u64>,
    pub _drop_guard: AbortOnDropHandle,
}

/// Per-device durable outbox.
///
/// Implementations:
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

    /// Best-effort `PUBLISH kick:{device_id} <new_session_id>`. Failures
    /// are logged but not propagated; the lease will eventually expire.
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
    /// * If `seq:{id} >= last_ack`: trim acked entries with
    ///   `XTRIM outbox:{id} MINID 0-{last_ack+1}` and return `current_seq`.
    /// * If `seq:{id} < last_ack`: **bootstrap path** — the device has a
    ///   higher last_ack than anything the store has ever seen, which is
    ///   exactly the wedged-after-restart case for fresh deploys carrying
    ///   this code. Set `seq:{id} = last_ack`, `DEL outbox:{id}`, return
    ///   `last_ack`.
    ///
    /// Both branches keep the keys alive for 30d via `EXPIRE`. The fence
    /// is checked via the lock script; callers must hold the lock.
    async fn reconcile_on_hello(
        &self,
        device_id: &str,
        session_id: &str,
        last_ack: u64,
    ) -> Result<u64>;

    /// Read all unacked frames for replay: `XRANGE outbox:{id} (0-{last_ack} +`.
    async fn unacked_frames(&self, device_id: &str, last_ack: u64) -> Result<Vec<Vec<u8>>>;

    /// Reserve the next seq atomically: fence + `INCR seq:{id}`. Returns
    /// the assigned seq. Callers then mutate `envelope.seq`, encode, and
    /// call [`Self::xadd_frame`].
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
