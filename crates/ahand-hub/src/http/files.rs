//! File operations REST endpoints.
//!
//! Exposes `POST /api/devices/{device_id}/files` which accepts a JSON-encoded
//! `FileRequest`, forwards it to the connected device via the websocket
//! gateway, and waits for the matching `FileResponse` to come back (correlated
//! by `request_id`).

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use dashmap::DashMap;
use prost::Message;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use ahand_protocol::{
    envelope, FileError, FileErrorCode, FileRequest, FileResponse,
};

use crate::auth::AuthContextExt;
use crate::http::api_error::{ApiError, ApiResult};
use crate::state::AppState;

const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;

/// Tracks in-flight file requests so the device_gateway can resolve them when a
/// FileResponse arrives.
#[derive(Default)]
pub struct PendingFileRequests {
    pending: DashMap<String, oneshot::Sender<FileResponse>>,
}

impl PendingFileRequests {
    pub fn register(&self, request_id: String) -> oneshot::Receiver<FileResponse> {
        let (tx, rx) = oneshot::channel();
        self.pending.insert(request_id, tx);
        rx
    }

    pub fn resolve(&self, request_id: &str, response: FileResponse) {
        if let Some((_, tx)) = self.pending.remove(request_id) {
            let _ = tx.send(response);
        }
    }

    pub fn cancel(&self, request_id: &str) {
        self.pending.remove(request_id);
    }
}

/// JSON body for `POST /api/devices/{device_id}/files`.
///
/// Accepts the proto `FileRequest` in its standard serde-JSON representation
/// (prost-generated types implement serde when the `serde` feature is
/// enabled). Since we currently don't emit serde impls on the proto types, we
/// accept a raw JSON value and decode to proto via `serde_json` + `prost`.
#[derive(Debug, Deserialize)]
pub struct FileOperationRequest {
    /// Raw proto FileRequest as protobuf base64 (preferred for clients that
    /// already use the generated types) or inline JSON. Exactly one field must
    /// be set.
    #[serde(default)]
    pub proto_b64: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FileOperationResponse {
    /// Base64-encoded proto `FileResponse`.
    pub proto_b64: String,
}

/// Handle a client-initiated file operation: forward to the device, wait for
/// the matching FileResponse, and return it.
pub async fn file_operation(
    auth: AuthContextExt,
    State(state): State<AppState>,
    Path(device_id): Path<String>,
    Json(body): Json<FileOperationRequest>,
) -> ApiResult<Json<FileOperationResponse>> {
    auth.require_dashboard_access()?;

    // Decode the inbound FileRequest. For now we accept base64-encoded proto
    // bytes so that callers can use the generated TypeScript/Rust types
    // directly without reinventing a JSON schema on top.
    let proto_b64 = body
        .proto_b64
        .ok_or_else(|| ApiError::validation("proto_b64 is required"))?;
    let proto_bytes = base64_decode(&proto_b64)
        .map_err(|e| ApiError::validation(format!("invalid proto_b64: {e}")))?;
    let mut request: FileRequest = FileRequest::decode(proto_bytes.as_slice())
        .map_err(|e| ApiError::validation(format!("failed to decode FileRequest: {e}")))?;

    // Ensure request_id is set so we can correlate the response.
    if request.request_id.is_empty() {
        request.request_id = body
            .request_id
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    }

    // Register the pending slot BEFORE sending so we don't race a fast device.
    let rx = state
        .pending_file_requests
        .register(request.request_id.clone());

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
        state.pending_file_requests.cancel(&request_id);
        return Err(err.into());
    }

    // Wait for the FileResponse or timeout.
    let timeout_secs = DEFAULT_REQUEST_TIMEOUT_SECS;
    let response = match tokio::time::timeout(Duration::from_secs(timeout_secs), rx).await {
        Ok(Ok(resp)) => resp,
        Ok(Err(_)) => {
            state.pending_file_requests.cancel(&request_id);
            return Err(ApiError::internal(
                "file response channel closed unexpectedly",
            ));
        }
        Err(_) => {
            state.pending_file_requests.cancel(&request_id);
            // 504 Gateway Timeout — device did not respond in time.
            return Err(ApiError::new(
                StatusCode::GATEWAY_TIMEOUT,
                "DEVICE_TIMEOUT",
                format!("device {device_id} did not respond within {timeout_secs}s"),
            ));
        }
    };

    // Encode and return.
    let encoded = response.encode_to_vec();
    Ok(Json(FileOperationResponse {
        proto_b64: base64_encode(&encoded),
    }))
}

// ── base64 helpers (without adding a dedicated dep) ────────────────────────

fn base64_encode(bytes: &[u8]) -> String {
    // Use std-friendly approach with the `ed25519-dalek` dep we already pull —
    // actually we have nothing; write a small helper.
    simple_base64::encode(bytes)
}

fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    simple_base64::decode(input)
}

/// Minimal, dependency-free base64 encoder/decoder (standard alphabet).
mod simple_base64 {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
        let mut i = 0;
        while i + 3 <= bytes.len() {
            let b0 = bytes[i] as u32;
            let b1 = bytes[i + 1] as u32;
            let b2 = bytes[i + 2] as u32;
            let n = (b0 << 16) | (b1 << 8) | b2;
            out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
            out.push(ALPHABET[((n >> 6) & 0x3F) as usize] as char);
            out.push(ALPHABET[(n & 0x3F) as usize] as char);
            i += 3;
        }
        let rem = bytes.len() - i;
        if rem == 1 {
            let b0 = bytes[i] as u32;
            let n = b0 << 16;
            out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
            out.push('=');
            out.push('=');
        } else if rem == 2 {
            let b0 = bytes[i] as u32;
            let b1 = bytes[i + 1] as u32;
            let n = (b0 << 16) | (b1 << 8);
            out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
            out.push(ALPHABET[((n >> 6) & 0x3F) as usize] as char);
            out.push('=');
        }
        out
    }

    pub fn decode(input: &str) -> Result<Vec<u8>, String> {
        let s: String = input.chars().filter(|c| !c.is_whitespace()).collect();
        if s.len() % 4 != 0 {
            return Err(format!("base64 input length must be multiple of 4 (got {})", s.len()));
        }
        let mut out = Vec::with_capacity(s.len() / 4 * 3);
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let c0 = decode_char(bytes[i])?;
            let c1 = decode_char(bytes[i + 1])?;
            let c2 = bytes[i + 2];
            let c3 = bytes[i + 3];
            let c2d = if c2 == b'=' { 0 } else { decode_char(c2)? };
            let c3d = if c3 == b'=' { 0 } else { decode_char(c3)? };
            let n = (c0 << 18) | (c1 << 12) | (c2d << 6) | c3d;
            out.push(((n >> 16) & 0xFF) as u8);
            if c2 != b'=' {
                out.push(((n >> 8) & 0xFF) as u8);
            }
            if c3 != b'=' {
                out.push((n & 0xFF) as u8);
            }
            i += 4;
        }
        Ok(out)
    }

    fn decode_char(c: u8) -> Result<u32, String> {
        match c {
            b'A'..=b'Z' => Ok((c - b'A') as u32),
            b'a'..=b'z' => Ok((c - b'a' + 26) as u32),
            b'0'..=b'9' => Ok((c - b'0' + 52) as u32),
            b'+' => Ok(62),
            b'/' => Ok(63),
            _ => Err(format!("invalid base64 character: {}", c as char)),
        }
    }
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
