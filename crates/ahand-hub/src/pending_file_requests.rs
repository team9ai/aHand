//! In-flight file-request correlation table.
//!
//! `PendingFileRequests` lets the HTTP `POST /api/devices/{id}/files`
//! handler park a `oneshot::Receiver<FileResponse>` while waiting for the
//! device to send the matching response back over the WebSocket gateway.
//! Both layers — `crate::http::files` (the HTTP handler) and
//! `crate::http::jobs::JobRuntime::handle_device_frame` (the WS frame
//! handler that resolves a slot when a `FileResponse` arrives) — depend on
//! this type, so it lives in a transport-neutral module instead of the
//! HTTP module that it used to share with `file_operation`.
//!
//! Concurrency model:
//! - Admission control is enforced via an `AtomicUsize` counter that is
//!   incremented BEFORE inserting and decremented on `resolve` / `cancel`.
//!   This makes the cap race-free under concurrent registers — a naive
//!   `len() >= capacity` check followed by `insert()` could let multiple
//!   callers race past the cap.
//! - Duplicate keys are rejected via `DashMap::entry`, so retries that
//!   reuse a still-pending `request_id` get an explicit `Duplicate` error
//!   instead of silently clobbering the previous waiter (which would then
//!   trip the closed-channel error path with a 500).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ahand_protocol::FileResponse;
use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use tokio::sync::oneshot;

/// Hard cap on simultaneously in-flight file requests across all devices.
/// Picked high enough to never bite a dashboard user under normal load,
/// low enough to stop a malicious client from leaking 30-second waiters.
pub const MAX_PENDING_FILE_REQUESTS: usize = 1024;

/// Error returned by `PendingFileRequests::register`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingFileRequestsError {
    /// The pending-request table is at its configured capacity.
    AtCapacity,
    /// A waiter for the same `(device_id, request_id)` is already pending.
    /// The caller should pick a different request id (or wait for the
    /// existing one to complete).
    Duplicate,
}

/// Tracks in-flight file requests so the device gateway can resolve them
/// when a `FileResponse` arrives. Keyed by `(device_id, request_id)` to
/// prevent cross contamination between devices that happen to pick
/// colliding request IDs.
pub struct PendingFileRequests {
    pending: DashMap<(String, String), oneshot::Sender<FileResponse>>,
    capacity: usize,
    in_flight: AtomicUsize,
}

impl PendingFileRequests {
    pub fn new(capacity: usize) -> Self {
        Self {
            pending: DashMap::new(),
            capacity,
            in_flight: AtomicUsize::new(0),
        }
    }

    pub fn register(
        &self,
        device_id: &str,
        request_id: &str,
    ) -> Result<oneshot::Receiver<FileResponse>, PendingFileRequestsError> {
        // 1. Atomically reserve a slot. If we exceed capacity we hand the
        //    reservation back. This makes the cap race-free under
        //    concurrent callers.
        let new_count = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        if new_count > self.capacity {
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            return Err(PendingFileRequestsError::AtCapacity);
        }

        // 2. Try to take the (device_id, request_id) slot. Reject
        //    duplicates explicitly — silently overwriting would orphan the
        //    prior waiter in the closed-channel error path.
        let key = (device_id.to_string(), request_id.to_string());
        match self.pending.entry(key) {
            Entry::Occupied(_) => {
                self.in_flight.fetch_sub(1, Ordering::SeqCst);
                Err(PendingFileRequestsError::Duplicate)
            }
            Entry::Vacant(slot) => {
                let (tx, rx) = oneshot::channel();
                slot.insert(tx);
                Ok(rx)
            }
        }
    }

    pub fn resolve(&self, device_id: &str, request_id: &str, response: FileResponse) {
        if let Some((_, tx)) = self
            .pending
            .remove(&(device_id.to_string(), request_id.to_string()))
        {
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            let _ = tx.send(response);
        }
    }

    pub fn cancel(&self, device_id: &str, request_id: &str) {
        if self
            .pending
            .remove(&(device_id.to_string(), request_id.to_string()))
            .is_some()
        {
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// Current number of registered slots. Useful for tests and metrics —
    /// production code should not branch on this (the value is racy with
    /// concurrent register/resolve/cancel).
    pub fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::SeqCst)
    }
}

impl Default for PendingFileRequests {
    fn default() -> Self {
        Self::new(MAX_PENDING_FILE_REQUESTS)
    }
}

/// Helper used by `state.rs` to expose a shared `Arc<PendingFileRequests>`.
pub fn new_pending_requests() -> Arc<PendingFileRequests> {
    Arc::new(PendingFileRequests::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ahand_protocol::{FileError, FileErrorCode, file_response};

    fn ok_response(request_id: &str) -> FileResponse {
        FileResponse {
            request_id: request_id.to_string(),
            result: Some(file_response::Result::Error(FileError {
                code: FileErrorCode::Unspecified as i32,
                message: "stub".into(),
                path: String::new(),
            })),
        }
    }

    #[tokio::test]
    async fn pending_file_requests_cross_device_collision_does_not_resolve_other_waiter() {
        // T12 regression: two devices share the same caller-chosen
        // request_id. Resolving (device_b, "req-1") must NOT deliver to
        // the (device_a, "req-1") waiter.
        let table = PendingFileRequests::default();
        let rx_a = table.register("device-a", "req-1").unwrap();
        let _rx_b = table.register("device-b", "req-1").unwrap();

        table.resolve("device-b", "req-1", ok_response("req-1"));

        // device-a waiter still pending.
        let poll = tokio::time::timeout(std::time::Duration::from_millis(25), rx_a).await;
        assert!(
            poll.is_err(),
            "device-a waiter should NOT have been resolved by device-b's response"
        );
        assert_eq!(table.in_flight(), 1);
    }

    #[tokio::test]
    async fn pending_file_requests_resolves_correct_device() {
        let table = PendingFileRequests::default();
        let rx_a = table.register("device-a", "req-1").unwrap();
        let _rx_b = table.register("device-b", "req-1").unwrap();

        table.resolve("device-a", "req-1", ok_response("req-1"));

        let resp = tokio::time::timeout(std::time::Duration::from_millis(100), rx_a)
            .await
            .expect("device-a waiter must resolve")
            .expect("oneshot must deliver");
        assert_eq!(resp.request_id, "req-1");
    }

    #[tokio::test]
    async fn pending_file_requests_admission_control_rejects_over_capacity() {
        // T13 regression: register returns AtCapacity once the table is
        // full. We use a tiny capacity=2 so the test doesn't need 1024
        // waiters.
        let table = PendingFileRequests::new(2);
        let _rx1 = table.register("device-1", "r1").unwrap();
        let _rx2 = table.register("device-1", "r2").unwrap();
        let third = table.register("device-1", "r3");
        assert_eq!(third.err(), Some(PendingFileRequestsError::AtCapacity));
    }

    #[tokio::test]
    async fn pending_file_requests_rejects_duplicate_keys_explicitly() {
        // R1 regression: registering the same (device_id, request_id) twice
        // must NOT silently overwrite the prior waiter. The second register
        // returns Duplicate so the caller can pick a fresh id (or wait for
        // the existing one to complete).
        let table = PendingFileRequests::default();
        let _rx1 = table.register("device-a", "req-1").unwrap();
        let second = table.register("device-a", "req-1");
        assert_eq!(second.err(), Some(PendingFileRequestsError::Duplicate));
        // The first waiter is still alive — `in_flight` reflects only one slot.
        assert_eq!(table.in_flight(), 1);
    }

    #[tokio::test]
    async fn pending_file_requests_capacity_is_atomic_under_concurrent_registers() {
        // R1 regression: capacity must not be over-subscribed under
        // concurrent admission. Spawn 32 tasks that each try to grab a slot
        // in a 4-slot table; exactly 4 must succeed, 28 must fail with
        // AtCapacity, and `in_flight` lands at 4 after the dust settles.
        let table = std::sync::Arc::new(PendingFileRequests::new(4));
        let mut handles = Vec::new();
        for i in 0..32 {
            let t = table.clone();
            handles.push(tokio::spawn(async move {
                t.register("device-1", &format!("r{i}"))
            }));
        }
        let mut accepted = 0usize;
        let mut rejected = 0usize;
        let mut keepalives = Vec::new();
        for h in handles {
            match h.await.unwrap() {
                Ok(rx) => {
                    accepted += 1;
                    keepalives.push(rx);
                }
                Err(PendingFileRequestsError::AtCapacity) => rejected += 1,
                Err(other) => panic!("unexpected error: {other:?}"),
            }
        }
        assert_eq!(accepted, 4, "exactly 4 registrations should be accepted");
        assert_eq!(rejected, 28, "the rest should be AtCapacity");
        assert_eq!(table.in_flight(), 4);
    }

    #[tokio::test]
    async fn pending_file_requests_admission_control_accepts_after_cancel() {
        let table = PendingFileRequests::new(2);
        let _rx1 = table.register("device-1", "r1").unwrap();
        let _rx2 = table.register("device-1", "r2").unwrap();
        table.cancel("device-1", "r1");
        // After cancelling one, there's room again.
        let _rx3 = table
            .register("device-1", "r3")
            .expect("room available after cancel");
        assert_eq!(table.in_flight(), 2);
    }
}
