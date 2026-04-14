//! File operations REST endpoints.
//!
//! Exposes `POST /api/devices/{device_id}/files` which accepts a raw
//! protobuf-encoded `FileRequest` (content-type `application/x-protobuf`),
//! forwards it to the connected device via the websocket gateway, and waits
//! for the matching `FileResponse` to come back (correlated by `request_id`).
//! The response body is a raw protobuf-encoded `FileResponse` with the same
//! content type.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use prost::Message;
use tokio::sync::oneshot;

use ahand_protocol::{envelope, FileError, FileErrorCode, FileRequest, FileResponse};

use crate::auth::AuthContextExt;
use crate::http::api_error::{ApiError, ApiResult};
use crate::state::AppState;

const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;
const PROTOBUF_CONTENT_TYPE: &str = "application/x-protobuf";
/// Hard cap on simultaneously in-flight file requests across all devices.
/// Picked high enough to never bite a dashboard user under normal load, low
/// enough to stop a malicious client from leaking 30-second waiters.
const MAX_PENDING_FILE_REQUESTS: usize = 1024;

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

/// Tracks in-flight file requests so the device_gateway can resolve them when a
/// FileResponse arrives. Keyed by `(device_id, request_id)` to prevent cross
/// contamination between devices that happen to pick colliding request IDs.
///
/// Concurrency: admission control is enforced via an `AtomicUsize` counter
/// that is incremented BEFORE inserting and decremented on resolve / cancel.
/// This makes the cap atomic in the face of concurrent registers — a naive
/// `len() >= capacity` check followed by `insert()` could let multiple
/// callers race past the cap. Duplicate keys are rejected via
/// `DashMap::entry`, so retries that reuse a still-pending request_id get an
/// explicit `Duplicate` error instead of silently clobbering the previous
/// waiter.
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
        //    reservation back. This makes the cap race-free under concurrent
        //    callers.
        let new_count = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        if new_count > self.capacity {
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            return Err(PendingFileRequestsError::AtCapacity);
        }

        // 2. Try to take the (device_id, request_id) slot. Reject duplicates
        //    explicitly — silently overwriting would orphan the prior waiter
        //    in the closed-channel error path.
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

    #[cfg(test)]
    pub fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::SeqCst)
    }
}

impl Default for PendingFileRequests {
    fn default() -> Self {
        Self::new(MAX_PENDING_FILE_REQUESTS)
    }
}

/// Handle a client-initiated file operation.
///
/// The request body is a raw protobuf `FileRequest`. The response body is a
/// raw protobuf `FileResponse`. Content-type on both is
/// `application/x-protobuf`.
///
/// Flow:
/// 1. Decode the protobuf body.
/// 2. Assign a request_id if the client didn't supply one.
/// 3. Register a pending-slot in `PendingFileRequests` *before* sending so we
///    don't race a fast device.
/// 4. Wrap the request in an `Envelope` and send it to the device via the
///    connection registry. On send failure we cancel the pending slot and map
///    `DeviceOffline` to 409.
/// 5. Wait for the response or a 30s timeout. On timeout we cancel the pending
///    slot and return 504.
/// 6. Encode the response back to protobuf bytes and return it with the same
///    content type.
pub async fn file_operation(
    auth: AuthContextExt,
    State(state): State<AppState>,
    Path(device_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    auth.require_dashboard_access()?;

    // Enforce content-type: missing / empty is allowed for backwards
    // compatibility with older clients and test helpers, but any explicit
    // content-type other than `application/x-protobuf` is rejected with
    // 415 so schema confusion (e.g. a client sending JSON by mistake) is
    // surfaced loudly instead of silently decoding random bytes.
    if let Some(ct) = headers.get(header::CONTENT_TYPE) {
        let ct_str = ct.to_str().unwrap_or("");
        // Strip any `; charset=...` / parameter suffix.
        let base = ct_str.split(';').next().unwrap_or("").trim();
        if !base.is_empty() && base != PROTOBUF_CONTENT_TYPE {
            return Err(ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "UNSUPPORTED_MEDIA_TYPE",
                format!(
                    "expected content-type {}, got {}",
                    PROTOBUF_CONTENT_TYPE, ct_str
                ),
            ));
        }
    }

    if body.is_empty() {
        return Err(ApiError::validation("request body is empty"));
    }

    let mut request: FileRequest = FileRequest::decode(body.as_ref())
        .map_err(|e| ApiError::validation(format!("failed to decode FileRequest: {e}")))?;

    if request.request_id.is_empty() {
        request.request_id = uuid::Uuid::new_v4().to_string();
    }

    let rx = state
        .pending_file_requests
        .register(&device_id, &request.request_id)
        .map_err(|err| match err {
            PendingFileRequestsError::AtCapacity => ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "FILE_REQUESTS_SATURATED",
                "hub pending file-request table is at capacity; retry shortly",
            ),
            PendingFileRequestsError::Duplicate => ApiError::new(
                StatusCode::CONFLICT,
                "FILE_REQUEST_DUPLICATE",
                format!(
                    "request_id {} is already in flight for device {}",
                    request.request_id, device_id
                ),
            ),
        })?;

    let request_id = request.request_id.clone();
    let envelope = ahand_protocol::Envelope {
        device_id: device_id.clone(),
        msg_id: format!("file-{}", request_id),
        ts_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
        payload: Some(envelope::Payload::FileRequest(request)),
        ..Default::default()
    };

    if let Err(err) = state.connections.send(&device_id, envelope).await {
        state
            .pending_file_requests
            .cancel(&device_id, &request_id);
        return Err(err.into());
    }

    let timeout_secs = DEFAULT_REQUEST_TIMEOUT_SECS;
    let response = match tokio::time::timeout(Duration::from_secs(timeout_secs), rx).await {
        Ok(Ok(resp)) => resp,
        Ok(Err(_)) => {
            state
                .pending_file_requests
                .cancel(&device_id, &request_id);
            return Err(ApiError::internal(
                "file response channel closed unexpectedly",
            ));
        }
        Err(_) => {
            state
                .pending_file_requests
                .cancel(&device_id, &request_id);
            return Err(ApiError::new(
                StatusCode::GATEWAY_TIMEOUT,
                "DEVICE_TIMEOUT",
                format!("device {device_id} did not respond within {timeout_secs}s"),
            ));
        }
    };

    let encoded = response.encode_to_vec();
    Ok((
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static(PROTOBUF_CONTENT_TYPE),
        )],
        encoded,
    )
        .into_response())
}

/// Helper used by `state.rs` to expose a shared Arc<PendingFileRequests>.
pub fn new_pending_requests() -> Arc<PendingFileRequests> {
    Arc::new(PendingFileRequests::default())
}

/// Build a synthetic FileError response for internal error paths.
pub fn error_response(request_id: String, message: impl Into<String>) -> FileResponse {
    FileResponse {
        request_id,
        result: Some(ahand_protocol::file_response::Result::Error(FileError {
            code: FileErrorCode::Unspecified as i32,
            message: message.into(),
            path: String::new(),
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ahand_protocol::{file_response, FileErrorCode as ProtoFileErrorCode};

    fn ok_response(request_id: &str) -> FileResponse {
        FileResponse {
            request_id: request_id.to_string(),
            result: Some(file_response::Result::Error(FileError {
                code: ProtoFileErrorCode::Unspecified as i32,
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

// ── S3 large-file transfer ────────────────────────────────────────────────
//
// The `POST /files/upload-url` endpoint used to live here. It was removed
// during Round 1 review because the full large-file transfer flow (hub
// downloads from S3 before forwarding writes, hub uploads large responses
// before returning reads) was only half-wired: the endpoint produced valid
// presigned PUT URLs but the daemon rejected any FileRequest carrying
// `FullWrite.s3_object_key`. Exposing a half-working API surface is worse
// than not exposing it at all, so the route was dropped until the entire
// path is implemented.
//
// The underlying plumbing is intentionally kept:
// - `s3::S3Client` (generate_upload_url/generate_download_url/
//   upload_bytes/download_bytes)
// - `config::S3Config`
// - `AppState.s3_client`
// - proto field `FullWrite.s3_object_key`
//
// A follow-up PR can wire the full flow inside `file_operation` (S3 fetch
// before forwarding writes, S3 push after large reads) without having to
// re-establish any of the skeleton.
