//! File operations REST endpoints.
//!
//! Exposes `POST /api/devices/{device_id}/files` which accepts a raw
//! protobuf-encoded `FileRequest` (content-type `application/x-protobuf`),
//! forwards it to the connected device via the websocket gateway, and waits
//! for the matching `FileResponse` to come back (correlated by `request_id`).
//! The response body is a raw protobuf-encoded `FileResponse` with the same
//! content type.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use dashmap::DashMap;
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

/// Error returned when `PendingFileRequests::register` cannot accept a new
/// waiter because the table is at capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingFileRequestsAtCapacity;

/// Tracks in-flight file requests so the device_gateway can resolve them when a
/// FileResponse arrives. Keyed by `(device_id, request_id)` to prevent cross
/// contamination between devices that happen to pick colliding request IDs.
///
/// Admission control: `register` caps inserts at `MAX_PENDING_FILE_REQUESTS`
/// to prevent an authenticated client from retaining arbitrarily many
/// 30-second waiters on the hub.
pub struct PendingFileRequests {
    pending: DashMap<(String, String), oneshot::Sender<FileResponse>>,
    capacity: usize,
}

impl PendingFileRequests {
    pub fn new(capacity: usize) -> Self {
        Self {
            pending: DashMap::new(),
            capacity,
        }
    }

    pub fn register(
        &self,
        device_id: &str,
        request_id: &str,
    ) -> Result<oneshot::Receiver<FileResponse>, PendingFileRequestsAtCapacity> {
        if self.pending.len() >= self.capacity {
            return Err(PendingFileRequestsAtCapacity);
        }
        let (tx, rx) = oneshot::channel();
        self.pending
            .insert((device_id.to_string(), request_id.to_string()), tx);
        Ok(rx)
    }

    pub fn resolve(&self, device_id: &str, request_id: &str, response: FileResponse) {
        if let Some((_, tx)) = self
            .pending
            .remove(&(device_id.to_string(), request_id.to_string()))
        {
            let _ = tx.send(response);
        }
    }

    pub fn cancel(&self, device_id: &str, request_id: &str) {
        self.pending
            .remove(&(device_id.to_string(), request_id.to_string()));
    }

    #[cfg(test)]
    pub fn in_flight(&self) -> usize {
        self.pending.len()
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
    body: Bytes,
) -> ApiResult<Response> {
    auth.require_dashboard_access()?;

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
        .map_err(|_| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "FILE_REQUESTS_SATURATED",
                "hub pending file-request table is at capacity; retry shortly",
            )
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
