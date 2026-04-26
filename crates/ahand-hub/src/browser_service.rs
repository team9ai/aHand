//! Shared browser command execution service.
//!
//! Both the dashboard `POST /api/browser` endpoint and the control-plane
//! `POST /api/control/browser` endpoint funnel through [`execute`] here.
//! The two HTTP handlers handle their own auth + ownership checks then
//! delegate the core machinery (envelope construction, WS dispatch,
//! oneshot await, response decode) to this function.
//!
//! Behavior is intentionally identical to the original
//! `http::browser::browser_command` implementation — this module is a
//! pure refactor that lifts the shared machinery out so it can be
//! reused. Error code strings returned via [`crate::http::browser::map_service_error`]
//! match the originals so the dashboard contract is preserved
//! byte-for-byte.

use std::time::Duration;

use ahand_hub_core::traits::DeviceStore;
use ahand_protocol::{BrowserResponse, Envelope};
use thiserror::Error;

use crate::state::AppState;

/// Input to [`execute`]. The handler is expected to have already done
/// any auth / ownership / rate-limiting checks that are specific to its
/// endpoint and to have parsed the JSON request body.
#[derive(Debug, Clone)]
pub struct BrowserCommandInput {
    pub device_id: String,
    pub session_id: String,
    pub action: String,
    /// Pre-serialized params (the dashboard handler stringifies its
    /// `serde_json::Value` body once before calling). An empty string
    /// means "no params" and is forwarded as-is to the daemon.
    pub params_json: String,
    pub timeout_ms: u64,
    /// Forward-compatibility hook for the control-plane endpoint
    /// introduced in Task 9. The dashboard handler always passes
    /// `None`; the worker handler will pass its caller-supplied
    /// correlation id. Currently unused by this function — present so
    /// callers can begin threading it through without a future API
    /// break.
    pub correlation_id: Option<String>,
}

#[derive(Debug, Error)]
pub enum BrowserServiceError {
    #[error("device {device_id} not found")]
    DeviceNotFound { device_id: String },
    #[error("device {device_id} is not connected")]
    DeviceOffline { device_id: String },
    /// WS-send failure. Mapped to the same HTTP code as `DeviceOffline`
    /// (the device went away mid-dispatch) but carries the underlying
    /// error message so the original "Failed to send to device: <err>"
    /// wording is preserved.
    #[error("failed to send to device {device_id}: {reason}")]
    SendFailed { device_id: String, reason: String },
    #[error("device {device_id} does not support browser")]
    CapabilityMissing { device_id: String },
    #[error("browser command timed out after {ms}ms")]
    Timeout { ms: u64 },
    #[error("response channel closed unexpectedly")]
    ChannelClosed,
    #[error("internal error: {0}")]
    Internal(String),
}

/// Execute a browser command against a device and await its response.
///
/// Steps:
/// 1. Look up the device and verify it's online.
/// 2. Verify it advertises the `browser` capability.
/// 3. Register a oneshot channel under a freshly-minted `request_id` in
///    `state.browser_pending` so the WS receive path can deliver the
///    response back here.
/// 4. Build a `BrowserRequest` envelope and send it via the WS gateway.
/// 5. Await the response with the caller's `timeout_ms` (clamped to a
///    minimum of 1000ms, matching the original handler).
/// 6. Clean up the pending entry on every error path.
pub async fn execute(
    state: &AppState,
    input: BrowserCommandInput,
) -> Result<BrowserResponse, BrowserServiceError> {
    // Suppress unused-field warning until Task 9 wires the correlation
    // id through into structured logging / audit. Keeping it as a real
    // field on the struct preserves the public API.
    let _ = input.correlation_id;

    // 1. Device lookup + online check.
    let device = state
        .devices
        .get(&input.device_id)
        .await
        .map_err(|e| BrowserServiceError::Internal(format!("device store: {e}")))?
        .ok_or_else(|| BrowserServiceError::DeviceNotFound {
            device_id: input.device_id.clone(),
        })?;

    if !device.online {
        return Err(BrowserServiceError::DeviceOffline {
            device_id: input.device_id.clone(),
        });
    }

    // 2. Capability check.
    if !device.capabilities.iter().any(|c| c == "browser") {
        return Err(BrowserServiceError::CapabilityMissing {
            device_id: input.device_id.clone(),
        });
    }

    // 3. Oneshot registration.
    let request_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = tokio::sync::oneshot::channel();
    state.browser_pending.insert(request_id.clone(), tx);

    // 4. Build envelope + dispatch.
    let envelope = Envelope {
        device_id: input.device_id.clone(),
        msg_id: format!("browser-{request_id}"),
        ts_ms: now_ms(),
        payload: Some(ahand_protocol::envelope::Payload::BrowserRequest(
            ahand_protocol::BrowserRequest {
                request_id: request_id.clone(),
                session_id: input.session_id,
                action: input.action,
                params_json: input.params_json,
                timeout_ms: input.timeout_ms,
            },
        )),
        ..Default::default()
    };

    if let Err(err) = state.connections.send(&input.device_id, envelope).await {
        state.browser_pending.remove(&request_id);
        // Match the original handler: a WS-send failure is reported to
        // the caller as DEVICE_OFFLINE so it has the same external
        // contract regardless of whether the device went offline before
        // or during the dispatch. We surface the underlying error via
        // `Internal` so `map_service_error` can render it the same way
        // the original handler did ("Failed to send to device: <err>").
        return Err(BrowserServiceError::SendFailed {
            device_id: input.device_id.clone(),
            reason: err.to_string(),
        });
    }

    // 5. Await with deadline. Floor at 1s — matches original handler.
    let timeout = Duration::from_millis(input.timeout_ms.max(1000));
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(resp)) => Ok(resp),
        Ok(Err(_)) => {
            state.browser_pending.remove(&request_id);
            Err(BrowserServiceError::ChannelClosed)
        }
        Err(_) => {
            state.browser_pending.remove(&request_id);
            Err(BrowserServiceError::Timeout {
                ms: input.timeout_ms,
            })
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
