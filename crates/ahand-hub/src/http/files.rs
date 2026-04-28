//! Dashboard file operations REST endpoint.
//!
//! Exposes `POST /api/devices/{device_id}/files` which accepts a raw
//! protobuf-encoded `FileRequest` (content-type `application/x-protobuf`),
//! forwards it to the connected device via the websocket gateway, and waits
//! for the matching `FileResponse` to come back (correlated by `request_id`).
//! The response body is a raw protobuf-encoded `FileResponse` with the same
//! content type.
//!
//! The HTTP-level concerns (auth, content-type validation, protobuf
//! decode/encode, error mapping) live here. The shared transport
//! machinery (pending-slot registration, WS dispatch, timeout-await,
//! RAII cleanup) lives in `crate::file_service::execute`. The
//! control-plane sibling `POST /api/control/files`
//! (`http::control_plane::control_files`) reuses the same service, so
//! both endpoints surface byte-identical wire-level semantics.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use prost::Message;

use ahand_protocol::{FileError, FileErrorCode, FileRequest, FileResponse};

use crate::auth::AuthContextExt;
use crate::file_service::{self, FileServiceError};
use crate::http::api_error::{ApiError, ApiResult};
use crate::state::AppState;

const PROTOBUF_CONTENT_TYPE: &str = "application/x-protobuf";

/// Handle a client-initiated file operation.
///
/// The request body is a raw protobuf `FileRequest`. The response body is a
/// raw protobuf `FileResponse`. Content-type on both is
/// `application/x-protobuf`.
///
/// Flow:
/// 1. Decode the protobuf body.
/// 2. Assign a request_id if the client didn't supply one.
/// 3. Delegate the WS round-trip to `file_service::execute`, which
///    registers the pending slot, dispatches the envelope, awaits the
///    response with the configured timeout, and arms an RAII guard so
///    cancellation cannot leak slots.
/// 4. Encode the response back to protobuf bytes and return it with the
///    same content type.
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

    let response = file_service::execute(&state, &device_id, request, state.file_request_timeout)
        .await
        .map_err(map_service_error)?;

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

/// Translate a [`FileServiceError`] into the dashboard wire-format
/// [`ApiError`]. Preserved verbatim from the pre-refactor handler so
/// the dashboard contract is unchanged:
///   * `Duplicate`       → 409 `FILE_REQUEST_DUPLICATE`
///   * `AtCapacity`      → 503 `FILE_REQUESTS_SATURATED`
///   * `DeviceOffline`   → 409 `DEVICE_OFFLINE`
///   * `Timeout`         → 504 `DEVICE_TIMEOUT`
///   * `ChannelClosed`   → 500 `INTERNAL_ERROR`
///   * `Internal`        → 500 `INTERNAL_ERROR`
///
/// `pub(crate)` so the control-plane handler can reuse the same
/// hub-error mapping (the control plane wraps the result in its own
/// JSON envelope but its hub-error contract is identical).
pub(crate) fn map_service_error(err: FileServiceError) -> ApiError {
    match err {
        FileServiceError::AtCapacity => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "FILE_REQUESTS_SATURATED",
            "hub pending file-request table is at capacity; retry shortly",
        ),
        FileServiceError::Duplicate {
            device_id,
            request_id,
        } => ApiError::new(
            StatusCode::CONFLICT,
            "FILE_REQUEST_DUPLICATE",
            format!("request_id {request_id} is already in flight for device {device_id}"),
        ),
        FileServiceError::DeviceOffline { device_id } => ApiError::new(
            StatusCode::CONFLICT,
            "DEVICE_OFFLINE",
            format!("Device {device_id} is not currently connected"),
        ),
        FileServiceError::Timeout { ms } => ApiError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "DEVICE_TIMEOUT",
            format!("device did not respond within {ms}ms"),
        ),
        FileServiceError::ChannelClosed => {
            ApiError::internal("file response channel closed unexpectedly")
        }
        FileServiceError::Internal(msg) => ApiError::internal(msg),
    }
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
