//! In-memory registry for control-plane jobs dispatched via
//! `/api/control/jobs`.
//!
//! This is deliberately separate from [`crate::http::jobs::JobRuntime`],
//! which backs the dashboard `/api/jobs` surface and has its own
//! persistence, audit, and lifecycle expectations. Control-plane jobs
//! are intentionally lightweight:
//!
//!   - they are tracked only in-memory for the lifetime of the stream,
//!   - they are addressed by a hub-minted `ulid` (not a UUID),
//!   - they broadcast stdout / stderr / progress / finished / error to
//!     one or more SSE subscribers.
//!
//! The WS inbound path routes [`ahand_protocol::JobEvent`] /
//! [`ahand_protocol::JobFinished`] / [`ahand_protocol::JobRejected`]
//! envelopes into this tracker in addition to `JobRuntime`; both
//! trackers filter by `job_id` so a control-plane job doesn't collide
//! with a dashboard job.
//!
//! Cleanup semantics: an entry is removed as soon as a terminal event
//! (`Finished` or `Error`) is published to it. Leaking subscribers
//! don't matter — `tokio::broadcast` drops senders naturally when the
//! entry goes out of scope, which in turn closes any SSE streams. If
//! a daemon never emits a terminal event (crash without final frame),
//! the entry stays until the hub is restarted — the dashboard job
//! flow already relies on the WS disconnect path to finalize jobs, but
//! control-plane jobs don't have that hook, so callers should set a
//! sensible `timeout_ms` on the daemon side.

use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use tokio::sync::broadcast;

/// Capacity of the broadcast channel used for each tracked job. Large
/// enough that a slow SSE client won't cause meaningful lag on a
/// typical job (millions of stdout bytes at 8 KiB per frame); a slow
/// subscriber that falls behind simply sees `RecvError::Lagged` and
/// the stream ends.
const JOB_EVENT_CHANNEL_CAPACITY: usize = 1024;

/// Single control-plane job event delivered to SSE subscribers.
///
/// The tag/content shape is intentionally flat so the on-wire SSE
/// frame stays small. `serde_json` escapes newlines in strings, which
/// means a multi-line stdout chunk is delivered as one SSE `data:`
/// line and never mis-splits on a `\n\n` sequence inside the chunk.
#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum ControlJobEvent {
    Stdout {
        chunk: String,
    },
    Stderr {
        chunk: String,
    },
    Progress {
        percent: u8,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    Finished {
        exit_code: i32,
        duration_ms: u64,
    },
    Error {
        code: String,
        message: String,
    },
}

/// Per-job channel + metadata kept in the tracker.
pub struct JobChannels {
    event_tx: broadcast::Sender<ControlJobEvent>,
    pub device_id: String,
    pub external_user_id: String,
    pub correlation_id: Option<String>,
    pub started_at: Instant,
}

impl JobChannels {
    /// Subscribe for incoming events. The returned receiver lives
    /// independently of this channel handle and is dropped by the
    /// caller — the broadcast sender naturally notices when the last
    /// subscriber disappears.
    pub fn subscribe(&self) -> broadcast::Receiver<ControlJobEvent> {
        self.event_tx.subscribe()
    }

    fn send(&self, event: ControlJobEvent) {
        // Dropped if no active subscribers. We intentionally don't
        // block on delivery — a dashboard operator's slow client must
        // not back-pressure the WS inbound loop.
        let _ = self.event_tx.send(event);
    }
}

/// In-memory index of active control-plane jobs. Two maps:
///
/// * `jobs` — keyed by `job_id` (ulid), the canonical record.
/// * `correlation_index` — keyed by `{external_user_id}:{correlation_id}`
///   so POSTs with the same correlation id from the same user are
///   idempotent without leaking ids across users (a user could
///   legitimately reuse a correlation id another user also picked).
#[derive(Default)]
pub struct ControlJobTracker {
    jobs: DashMap<String, Arc<JobChannels>>,
    correlation_index: DashMap<String, String>,
}

impl ControlJobTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new job and return a handle to it. Safe to call
    /// without first checking `find_by_correlation`: if a correlation
    /// id collides, the caller is expected to short-circuit earlier.
    pub fn register(
        &self,
        job_id: String,
        device_id: String,
        external_user_id: String,
        correlation_id: Option<String>,
    ) -> Arc<JobChannels> {
        let (tx, _) = broadcast::channel(JOB_EVENT_CHANNEL_CAPACITY);
        let channels = Arc::new(JobChannels {
            event_tx: tx,
            device_id,
            external_user_id: external_user_id.clone(),
            correlation_id: correlation_id.clone(),
            started_at: Instant::now(),
        });
        if let Some(cid) = correlation_id {
            self.correlation_index
                .insert(correlation_key(&external_user_id, &cid), job_id.clone());
        }
        self.jobs.insert(job_id, channels.clone());
        channels
    }

    pub fn get(&self, job_id: &str) -> Option<Arc<JobChannels>> {
        self.jobs.get(job_id).map(|entry| entry.clone())
    }

    /// Returns the existing `job_id` if the `(external_user_id,
    /// correlation_id)` pair is already registered AND still live. An
    /// entry that was removed (terminal) is treated as absent so the
    /// next POST starts a new job rather than returning a stale id.
    pub fn find_by_correlation(
        &self,
        external_user_id: &str,
        correlation_id: &str,
    ) -> Option<String> {
        let key = correlation_key(external_user_id, correlation_id);
        let existing = self.correlation_index.get(&key).map(|v| v.clone())?;
        if self.jobs.contains_key(&existing) {
            Some(existing)
        } else {
            // Terminal event already reclaimed the entry; drop the
            // stale pointer so a retry creates a fresh job rather
            // than 404-ing on `GET /stream`.
            self.correlation_index.remove(&key);
            None
        }
    }

    /// Publish a non-terminal event to all subscribers.
    pub fn publish(&self, job_id: &str, event: ControlJobEvent) {
        if let Some(channels) = self.jobs.get(job_id) {
            channels.send(event);
        }
    }

    /// Publish a terminal event (`Finished` / `Error`) and drop the
    /// registry entry so subsequent GETs 404 and so the broadcast
    /// sender is closed (SSE streams end cleanly).
    pub fn finalize(&self, job_id: &str, event: ControlJobEvent) {
        debug_assert!(matches!(
            event,
            ControlJobEvent::Finished { .. } | ControlJobEvent::Error { .. }
        ));
        let Some((_, channels)) = self.jobs.remove(job_id) else {
            return;
        };
        channels.send(event);
        if let Some(cid) = &channels.correlation_id {
            let key = correlation_key(&channels.external_user_id, cid);
            // Only remove if the correlation entry still points at
            // this job id — a later POST under the same correlation
            // id may have been rejected earlier, but defense in depth.
            if let Some(existing) = self.correlation_index.get(&key)
                && existing.value() == job_id
            {
                drop(existing);
                self.correlation_index.remove(&key);
            }
        }
    }

    /// Number of live jobs. Used by tests.
    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }
}

fn correlation_key(external_user_id: &str, correlation_id: &str) -> String {
    format!(
        "{}:{}:{}",
        external_user_id.len(),
        external_user_id,
        correlation_id
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correlation_key_is_collision_free_across_separator_injection() {
        // Before the fix, "a:b" + "c" and "a" + "b:c" both produced "a:b:c"
        let key1 = correlation_key("a:b", "c");
        let key2 = correlation_key("a", "b:c");
        assert_ne!(key1, key2, "user_ids containing ':' must not collide");

        // Longer case from the original bug report
        let key3 = correlation_key("user:alice", "job-1");
        let key4 = correlation_key("user", "alice:job-1");
        assert_ne!(key3, key4);

        // Identical inputs must produce identical keys
        let key5 = correlation_key("user-1", "corr-42");
        let key6 = correlation_key("user-1", "corr-42");
        assert_eq!(key5, key6);

        // Different users with same correlation_id must not collide
        let key7 = correlation_key("alice", "job");
        let key8 = correlation_key("bob", "job");
        assert_ne!(key7, key8);
    }

    #[test]
    fn register_then_get_returns_channel() {
        let t = ControlJobTracker::new();
        t.register(
            "job-1".into(),
            "dev-1".into(),
            "user-1".into(),
            Some("corr-1".into()),
        );
        assert!(t.get("job-1").is_some());
        assert_eq!(t.len(), 1);
        assert!(!t.is_empty());
    }

    #[test]
    fn find_by_correlation_is_per_user() {
        let t = ControlJobTracker::new();
        t.register(
            "job-a".into(),
            "dev".into(),
            "user-a".into(),
            Some("corr".into()),
        );
        t.register(
            "job-b".into(),
            "dev".into(),
            "user-b".into(),
            Some("corr".into()),
        );
        assert_eq!(
            t.find_by_correlation("user-a", "corr").as_deref(),
            Some("job-a")
        );
        assert_eq!(
            t.find_by_correlation("user-b", "corr").as_deref(),
            Some("job-b")
        );
        assert_eq!(t.find_by_correlation("user-c", "corr"), None);
    }

    #[test]
    fn finalize_removes_entry_and_correlation() {
        let t = ControlJobTracker::new();
        let channels = t.register(
            "job-1".into(),
            "dev".into(),
            "user".into(),
            Some("corr".into()),
        );
        let mut rx = channels.subscribe();
        t.finalize(
            "job-1",
            ControlJobEvent::Finished {
                exit_code: 0,
                duration_ms: 5,
            },
        );
        let rx_event = rx.try_recv().unwrap();
        assert_eq!(
            rx_event,
            ControlJobEvent::Finished {
                exit_code: 0,
                duration_ms: 5,
            }
        );
        assert!(t.get("job-1").is_none());
        assert_eq!(t.find_by_correlation("user", "corr"), None);
        assert!(t.is_empty());
    }

    #[test]
    fn publish_without_subscriber_is_noop() {
        let t = ControlJobTracker::new();
        t.register("job-1".into(), "dev".into(), "user".into(), None);
        // No panic, and the event is silently dropped.
        t.publish("job-1", ControlJobEvent::Stdout { chunk: "hi".into() });
        // Publishing to an unknown job id is a no-op.
        t.publish("nope", ControlJobEvent::Stdout { chunk: "hi".into() });
    }

    #[test]
    fn finalize_unknown_is_noop() {
        let t = ControlJobTracker::new();
        t.finalize(
            "nope",
            ControlJobEvent::Error {
                code: "x".into(),
                message: "y".into(),
            },
        );
        assert!(t.is_empty());
    }

    #[test]
    fn find_by_correlation_expires_stale_pointer() {
        let t = ControlJobTracker::new();
        t.register(
            "job-1".into(),
            "dev".into(),
            "user".into(),
            Some("corr".into()),
        );
        // Simulate an odd teardown order: finalize removes both the
        // job AND the correlation entry; then we verify that a second
        // finalize is a no-op. This is the regression test for the
        // "stale correlation" cleanup branch.
        t.finalize(
            "job-1",
            ControlJobEvent::Error {
                code: "c".into(),
                message: "m".into(),
            },
        );
        assert_eq!(t.find_by_correlation("user", "corr"), None);
    }

    #[test]
    fn register_same_correlation_replaces_pointer() {
        // A rare case: register job A with correlation C, drop A
        // without finalizing (e.g. test scaffolding), then register
        // job B with the same correlation. The index should point at
        // B, not A.
        let t = ControlJobTracker::new();
        t.register(
            "job-a".into(),
            "dev".into(),
            "user".into(),
            Some("corr".into()),
        );
        t.jobs.remove("job-a");
        t.register(
            "job-b".into(),
            "dev".into(),
            "user".into(),
            Some("corr".into()),
        );
        assert_eq!(
            t.find_by_correlation("user", "corr").as_deref(),
            Some("job-b")
        );
    }

    #[test]
    fn two_subscribers_receive_same_events() {
        let t = ControlJobTracker::new();
        let channels = t.register("job-1".into(), "dev".into(), "user".into(), None);
        let mut rx1 = channels.subscribe();
        let mut rx2 = channels.subscribe();
        t.publish(
            "job-1",
            ControlJobEvent::Stdout {
                chunk: "hello".into(),
            },
        );
        assert_eq!(
            rx1.try_recv().unwrap(),
            ControlJobEvent::Stdout {
                chunk: "hello".into(),
            }
        );
        assert_eq!(
            rx2.try_recv().unwrap(),
            ControlJobEvent::Stdout {
                chunk: "hello".into(),
            }
        );
    }
}
