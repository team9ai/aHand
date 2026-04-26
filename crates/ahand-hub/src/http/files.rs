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
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use prost::Message;

use ahand_protocol::{FileError, FileErrorCode, FileRequest, FileResponse, envelope};

use crate::auth::AuthContextExt;
use crate::http::api_error::{ApiError, ApiResult};
use crate::pending_file_requests::{PendingFileRequests, PendingFileRequestsError};
use crate::state::AppState;

const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;
const PROTOBUF_CONTENT_TYPE: &str = "application/x-protobuf";

// PendingFileRequests itself now lives in `crate::pending_file_requests`
// (R17): the type is referenced by both this HTTP handler and by
// `JobRuntime::handle_device_frame` in the WS layer, so it shouldn't be
// scoped to the http module. AppState owns the shared instance.

/// RAII guard that releases a reserved slot in `PendingFileRequests` on
/// drop. Without this, a client that closes the connection mid-flight
/// (or any other early-future-drop path) leaves the slot occupied:
/// neither the `tokio::time::timeout` nor the `oneshot::Receiver` get a
/// chance to run their drop handlers in a way that knows the slot exists,
/// so the entry sits in the table until the device responds — and if
/// the device never does, that's a permanent slot leak (1024-cap DoS).
///
/// `PendingFileRequests::cancel` is idempotent: if a `resolve()` has
/// already removed the slot (the success path), the cancel is a no-op.
/// We therefore don't need to "disarm" the guard on success.
struct PendingSlotGuard {
    table: Arc<PendingFileRequests>,
    device_id: String,
    request_id: String,
}

impl Drop for PendingSlotGuard {
    fn drop(&mut self) {
        self.table.cancel(&self.device_id, &self.request_id);
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
        // Strip any `; charset=...` / parameter suffix. Per RFC 7231
        // §3.1.1.1 the type/subtype tokens are case-insensitive, so
        // normalize before comparing — `Application/X-Protobuf` from
        // a less-strict HTTP client must still be accepted.
        let base = ct_str.split(';').next().unwrap_or("").trim();
        if !base.is_empty() && !base.eq_ignore_ascii_case(PROTOBUF_CONTENT_TYPE) {
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

    // Arm the slot guard immediately after a successful register(). Every
    // exit path from this point on — explicit error returns, timeout,
    // channel close, the success return after we encode the response, and
    // the early future-drop case where the client closed the connection
    // mid-flight — runs Drop on `_slot_guard`, which calls cancel().
    // cancel() is idempotent against a slot already removed by resolve(),
    // so the success path is a no-op.
    let _slot_guard = PendingSlotGuard {
        table: state.pending_file_requests.clone(),
        device_id: device_id.clone(),
        request_id: request_id.clone(),
    };

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

    state.connections.send(&device_id, envelope).await?;

    let timeout_secs = DEFAULT_REQUEST_TIMEOUT_SECS;
    let response = match tokio::time::timeout(Duration::from_secs(timeout_secs), rx).await {
        Ok(Ok(resp)) => resp,
        Ok(Err(_)) => {
            return Err(ApiError::internal(
                "file response channel closed unexpectedly",
            ));
        }
        Err(_) => {
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

// PendingFileRequests unit tests live with the type itself in
// `crate::pending_file_requests`. The HTTP-level integration tests for
// `file_operation` are in `tests/http_files.rs`.

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
