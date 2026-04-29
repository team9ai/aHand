//! Shared file-operation execution service.
//!
//! Both the dashboard `POST /api/devices/{id}/files` endpoint and the
//! control-plane `POST /api/control/files` endpoint funnel through
//! [`execute`] here. The two HTTP handlers handle their own auth +
//! ownership checks and request encoding/decoding, then delegate the
//! core machinery (pending-slot registration, envelope construction,
//! WS dispatch, timeout-await, RAII cleanup) to this function.
//!
//! Behavior is intentionally identical to the original
//! `http::files::file_operation` implementation — this module is a
//! pure refactor that lifts the shared machinery out so it can be
//! reused. The dashboard contract is preserved byte-for-byte.
//!
//! The file flow does NOT do a capability check (unlike browser),
//! mirroring the existing dashboard semantics: any connected device
//! receives the FileRequest envelope and the device-side handler
//! decides whether it can serve the operation. A
//! `DEVICE_DOES_NOT_SUPPORT_FILES` error, if needed, would be a
//! follow-up at the daemon level, not here.

use std::sync::Arc;
use std::time::Duration;

use ahand_protocol::{Envelope, FileRequest, FileResponse, envelope};
use thiserror::Error;

use crate::pending_file_requests::{PendingFileRequests, PendingFileRequestsError};
use crate::state::AppState;

/// RAII guard that releases a reserved slot in `PendingFileRequests` on
/// drop. Mirrors the guard in `http::files::file_operation` — necessary
/// because `tokio::time::timeout` and `oneshot::Receiver` don't run
/// drop handlers in a way that knows the slot exists. Without the
/// guard, a handler future cancelled mid-await (e.g. axum dropping the
/// future when the HTTP client disconnects) leaks the slot.
///
/// `PendingFileRequests::cancel` is idempotent against a slot already
/// removed by `resolve()`, so the success path is a safe no-op.
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

/// Errors surfaced by [`execute`]. The two HTTP layers map these to
/// their own wire-format error envelopes (see
/// `http::files::map_service_error` for the dashboard side).
#[derive(Debug, Error)]
pub enum FileServiceError {
    /// The hub's pending-file-request table is at its configured cap.
    /// Caller should retry shortly.
    #[error("pending file-request table at capacity")]
    AtCapacity,
    /// A waiter for the same `(device_id, request_id)` is already in
    /// flight. The control-plane handler ALWAYS mints a fresh
    /// request_id (uuid v4) so this can only fire under control-plane
    /// `correlation_id`-driven retries — even there only when the
    /// caller reuses the same id within the timeout window.
    #[error("request_id {request_id} is already in flight for device {device_id}")]
    Duplicate {
        device_id: String,
        request_id: String,
    },
    /// The device row exists in the store but no live WS is attached.
    #[error("device {device_id} is not connected")]
    DeviceOffline { device_id: String },
    /// The hub-side timeout fired before the daemon responded. Carries
    /// the timeout value the handler used so the message can render
    /// the budget the client saw.
    #[error("device did not respond within {ms}ms")]
    Timeout { ms: u128 },
    /// The oneshot was dropped without a response. Indicates a logic
    /// bug in the WS gateway (resolve path) — the handler maps this to
    /// HTTP 500.
    #[error("response channel closed unexpectedly")]
    ChannelClosed,
    /// Catch-all for unexpected `state.connections.send` failures that
    /// aren't `DeviceOffline`. The underlying message is preserved so
    /// the operator-facing log/UI can surface it.
    #[error("internal error: {0}")]
    Internal(String),
}

/// Execute a file operation against a device and await its response.
///
/// Steps:
/// 1. Register a oneshot channel under `(device_id, request.request_id)`
///    in `state.pending_file_requests` BEFORE sending so we don't race
///    a fast device.
/// 2. Arm an RAII guard so any future cancellation (axum drops the
///    handler future) cleanly releases the slot.
/// 3. Wrap the request in an `Envelope` and send via the WS gateway.
/// 4. Await with the supplied timeout. On timeout, return
///    `FileServiceError::Timeout`; the guard releases the slot.
///
/// `request.request_id` MUST be set by the caller. The dashboard
/// handler mints a UUID when the protobuf body omits it; the
/// control-plane handler always mints one (the wire-level
/// `correlation_id` is a logical-retry hint, not the hub-side
/// request_id).
pub async fn execute(
    state: &AppState,
    device_id: &str,
    request: FileRequest,
    timeout: Duration,
) -> Result<FileResponse, FileServiceError> {
    debug_assert!(
        !request.request_id.is_empty(),
        "file_service::execute requires a non-empty request_id"
    );

    let request_id = request.request_id.clone();

    let rx = state
        .pending_file_requests
        .register(device_id, &request_id)
        .map_err(|err| match err {
            PendingFileRequestsError::AtCapacity => FileServiceError::AtCapacity,
            PendingFileRequestsError::Duplicate => FileServiceError::Duplicate {
                device_id: device_id.to_string(),
                request_id: request_id.clone(),
            },
        })?;

    // Arm the slot guard immediately. Every exit path from here on
    // — explicit error, timeout, channel close, success — runs Drop
    // on `_slot_guard` and calls cancel(). cancel() is idempotent
    // against a slot already removed by resolve(), so the success
    // path is a no-op.
    let _slot_guard = PendingSlotGuard {
        table: state.pending_file_requests.clone(),
        device_id: device_id.to_string(),
        request_id: request_id.clone(),
    };

    let envelope = Envelope {
        device_id: device_id.to_string(),
        msg_id: format!("file-{request_id}"),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::FileRequest(request)),
        ..Default::default()
    };

    if let Err(err) = state.connections.send_envelope(device_id, envelope).await {
        // Match the dashboard handler: a WS-send failure for a known
        // device row is reported as `DeviceOffline`. Any other failure
        // bubbles up as `Internal`.
        return Err(match err.downcast_ref::<ahand_hub_core::HubError>() {
            Some(ahand_hub_core::HubError::DeviceOffline(_)) => FileServiceError::DeviceOffline {
                device_id: device_id.to_string(),
            },
            _ => FileServiceError::Internal(err.to_string()),
        });
    }

    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(resp)) => Ok(resp),
        Ok(Err(_)) => Err(FileServiceError::ChannelClosed),
        Err(_) => Err(FileServiceError::Timeout {
            ms: timeout.as_millis(),
        }),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
