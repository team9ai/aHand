//! File operations REST endpoints.
//!
//! Exposes `POST /api/devices/{device_id}/files` which accepts a raw
//! protobuf-encoded `FileRequest` (content-type `application/x-protobuf`),
//! forwards it to the connected device via the websocket gateway, and waits
//! for the matching `FileResponse` to come back (correlated by `request_id`).
//! The response body is a raw protobuf-encoded `FileResponse` with the same
//! content type.

use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use prost::Message;
use serde::Serialize;

use ahand_protocol::{FileError, FileErrorCode, FileRequest, FileResponse, envelope};

use crate::auth::AuthContextExt;
use crate::http::api_error::{ApiError, ApiResult};
use crate::pending_file_requests::{PendingFileRequests, PendingFileRequestsError};
use crate::state::AppState;

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

    // Translate client-supplied FullWrite{s3_object_key} into one the
    // daemon can act on by injecting a presigned GET URL. Daemons never
    // hold S3 credentials, so the hub is the only place that can speak
    // S3 directly. Object-key validation lives inside the helper.
    maybe_inject_full_write_download_url(&state, &mut request).await?;

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

    let timeout = state.file_request_timeout;
    let response = match tokio::time::timeout(timeout, rx).await {
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
                format!(
                    "device {device_id} did not respond within {}ms",
                    timeout.as_millis()
                ),
            ));
        }
    };

    // For large reads, swap inline content for a presigned S3 download
    // URL. Daemons always return inline bytes; the hub decides whether
    // the payload exceeds the threshold and uploads on the daemon's
    // behalf. Symmetric with the write path: only the hub talks to S3.
    // device_id was validated when the request entered the swap path
    // (see maybe_swap_large_read_response).
    let response = maybe_swap_large_read_response(&state, &device_id, response).await?;
    // device_id may have been used to build object keys above. Defensive
    // check is kept inside maybe_swap_large_read_response so call sites
    // don't have to remember to call it.

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

// ── S3 large-file transfer helpers ────────────────────────────────────────

#[derive(Serialize)]
pub struct UploadUrlResponse {
    pub object_key: String,
    pub upload_url: String,
    pub expires_at_ms: u64,
}

/// Issue a presigned PUT URL for a large-file upload. The client uploads
/// directly to S3, then sends `FileRequest { write: FullWrite { s3_object_key } }`
/// — `file_operation` will inject the corresponding presigned GET URL
/// before forwarding to the daemon.
///
/// Returns 503 with code `S3_DISABLED` when the hub has no `[s3]` block,
/// matching the route's contract: callers must treat S3 features as
/// optional and degrade gracefully when disabled.
pub async fn upload_url(
    auth: AuthContextExt,
    State(state): State<AppState>,
    Path(device_id): Path<String>,
) -> ApiResult<Json<UploadUrlResponse>> {
    auth.require_dashboard_access()?;
    validate_device_id_for_s3_key(&device_id)?;

    let Some(s3) = state.s3_client.as_ref() else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "S3_DISABLED",
            "S3 is not configured on this hub",
        ));
    };

    let object_key = build_upload_object_key(&device_id);
    let presigned = s3.generate_upload_url(&object_key).await.map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "S3_PRESIGN_FAILED",
            format!("failed to generate upload URL: {err}"),
        )
    })?;

    Ok(Json(UploadUrlResponse {
        object_key: presigned.object_key,
        upload_url: presigned.url,
        expires_at_ms: presigned.expires_at_ms,
    }))
}

/// Inspect a `FileResponse` produced by the daemon. If it carries inline
/// `read_binary` / `read_image` content larger than the configured
/// threshold, upload the bytes to S3 and rewrite the result to use a
/// presigned GET URL instead. The client then downloads directly from
/// S3, keeping the WebSocket frame size bounded.
async fn maybe_swap_large_read_response(
    state: &AppState,
    device_id: &str,
    mut response: FileResponse,
) -> Result<FileResponse, ApiError> {
    let Some(s3) = state.s3_client.as_ref() else {
        return Ok(response);
    };
    let threshold = s3.threshold();

    // Cheap up-front check on the device_id we'll embed in the object
    // key. Any caller who got this far already passed dashboard auth
    // and the connection registry's device-existence check, but a
    // device id with `..`/`/` would still let us write outside the
    // expected per-device key prefix. Reject loudly.
    validate_device_id_for_s3_key(device_id)?;

    use ahand_protocol::file_response::Result as Res;
    match response.result.as_mut() {
        Some(Res::ReadBinary(r)) if (r.content.len() as u64) > threshold => {
            let key = build_read_object_key(device_id);
            // Hand bytes off to upload_and_presign by value; only clear
            // r.content after both presign+upload succeed so an early
            // failure path keeps the response coherent and the handler
            // can still surface a meaningful 5xx without a half-mutated
            // proto.
            let presigned = upload_and_presign(s3.as_ref(), &key, std::mem::take(&mut r.content)).await?;
            r.download_url = Some(presigned.url);
            r.download_url_expires_ms = Some(presigned.expires_at_ms);
        }
        Some(Res::ReadImage(r)) if (r.content.len() as u64) > threshold => {
            let key = build_read_object_key(device_id);
            let presigned = upload_and_presign(s3.as_ref(), &key, std::mem::take(&mut r.content)).await?;
            r.download_url = Some(presigned.url);
            r.download_url_expires_ms = Some(presigned.expires_at_ms);
        }
        _ => {}
    }
    Ok(response)
}

/// Validate that `device_id` is safe to embed in an S3 object key
/// without escaping its `file-ops/<device_id>/` prefix. We have no
/// reason to allow `/`, `..`, control chars, or empty strings — those
/// either let a caller climb out of the per-device namespace
/// (path traversal in the bucket layout) or produce keys S3 would
/// happily collapse into surprising names. `file_operation` already
/// does device-existence checks via `connections.send`, so a rejected
/// id here can never become a 4xx that hides a real auth problem.
fn validate_device_id_for_s3_key(device_id: &str) -> Result<(), ApiError> {
    let bad = device_id.is_empty()
        || device_id.contains('/')
        || device_id.contains('\\')
        || device_id.contains("..")
        || device_id.chars().any(|c| c.is_control());
    if bad {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "INVALID_DEVICE_ID",
            "device_id contains characters not allowed in an S3 object key",
        ));
    }
    Ok(())
}

fn build_read_object_key(device_id: &str) -> String {
    // device_id was already validated by the caller (file_operation).
    format!("file-ops/{device_id}/read-{}.bin", uuid::Uuid::new_v4())
}

fn build_upload_object_key(device_id: &str) -> String {
    // device_id was already validated by upload_url before this is
    // reached.
    format!("file-ops/{device_id}/{}.bin", uuid::Uuid::new_v4())
}

/// Presign the GET URL FIRST, then upload. Two reasons:
///   1. Presigning is a purely local HMAC computation; it doesn't talk
///      to S3 at all. If it fails, that's a configuration/SDK bug, not
///      a transient outage, and we want to fail BEFORE creating an
///      orphaned object in the bucket.
///   2. If the upload itself fails, no presigned URL has been delivered
///      to the client yet, so there's no dangling pointer to a missing
///      object.
async fn upload_and_presign(
    s3: &crate::s3::S3Client,
    key: &str,
    bytes: Vec<u8>,
) -> Result<crate::s3::PresignedUrl, ApiError> {
    let presigned = s3.generate_download_url(key).await.map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "S3_PRESIGN_FAILED",
            format!("failed to generate download URL: {err}"),
        )
    })?;
    s3.upload_bytes(key, bytes).await.map_err(|err| {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "S3_UPLOAD_FAILED",
            format!("failed to upload to S3: {err}"),
        )
    })?;
    Ok(presigned)
}

/// If the request is a `FullWrite { s3_object_key }`, fill in
/// `s3_download_url` so the daemon (which holds no S3 credentials) can
/// fetch the bytes via plain HTTP. Returns `503 S3_DISABLED` if the
/// client supplied an `s3_object_key` but the hub has no S3 configured —
/// fail fast at the hub layer instead of letting the daemon surface a
/// confusing "no download URL" error.
async fn maybe_inject_full_write_download_url(
    state: &AppState,
    request: &mut FileRequest,
) -> Result<(), ApiError> {
    use ahand_protocol::{file_request, file_write, full_write};

    let Some(file_request::Operation::Write(write)) = request.operation.as_mut() else {
        return Ok(());
    };
    let Some(file_write::Method::FullWrite(fw)) = write.method.as_mut() else {
        return Ok(());
    };
    let Some(full_write::Source::S3ObjectKey(key)) = fw.source.as_ref() else {
        return Ok(());
    };

    let Some(s3) = state.s3_client.as_ref() else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "S3_DISABLED",
            "S3 is not configured on this hub",
        ));
    };

    // Reject keys that would let a caller poison a presigned URL via
    // path-traversal characters. We don't insist on the
    // `file-ops/<device_id>/` prefix being literally present (callers
    // may legitimately reuse a key generated for a different device —
    // the S3 ACL is what controls access), but `..` / `\0` / leading
    // `/` are obvious traversal/injection attempts.
    validate_object_key(key)?;

    let presigned = s3.generate_download_url(key).await.map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "S3_PRESIGN_FAILED",
            format!("failed to generate download URL: {err}"),
        )
    })?;
    fw.s3_download_url = Some(presigned.url);
    fw.s3_download_url_expires_ms = Some(presigned.expires_at_ms);
    Ok(())
}

/// Reject obviously-bad object keys before they reach the AWS SDK.
/// We don't try to enforce the exact `file-ops/<device_id>/` shape
/// because callers may reasonably reuse a key generated by a previous
/// upload-url call, but `..` and `\0` are never legitimate.
fn validate_object_key(key: &str) -> Result<(), ApiError> {
    let bad = key.is_empty()
        || key.contains('\0')
        || key.contains("..")
        || key.starts_with('/');
    if bad {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "INVALID_S3_OBJECT_KEY",
            "FullWrite.s3_object_key contains characters not allowed in an S3 object key",
        ));
    }
    Ok(())
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
