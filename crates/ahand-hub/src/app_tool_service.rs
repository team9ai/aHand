//! Shared app-tool invocation service.
//!
//! The control-plane `POST /api/control/app-tool` handler performs its own
//! auth, ownership, rate-limiting, and audit work, then delegates the core
//! machinery (oneshot pending-map, envelope construction, WS dispatch,
//! timeout await) to [`invoke`] here.
//!
//! The design mirrors `browser_service.rs` exactly:
//!   - Presence check (fast-fail offline)
//!   - Oneshot registration under a fresh `tool_call_id` (UUID)
//!   - RAII `PendingGuard` that removes the entry on every exit path,
//!     including future cancellation
//!   - `AppToolRequest` envelope dispatched via `state.connections.send`
//!   - `tokio::time::timeout` await with a 2-second grace over the caller-
//!     supplied timeout
//!
//! The handler writes audit entries; this module only performs the wire work.

use std::time::Duration;

use ahand_protocol::{AppToolRequest, AppToolResponse, Envelope};
use thiserror::Error;

use crate::state::AppState;

/// Hub-side timeout clamp constants (mirrors daemon's own clamp so that the
/// hub never waits longer than the daemon ever could).
pub const DEFAULT_TIMEOUT_MS: u64 = 60_000;
pub const MIN_TIMEOUT_MS: u64 = 1_000;
pub const MAX_TIMEOUT_MS: u64 = 300_000;

/// Extra grace period added on top of the caller-supplied timeout so that
/// the hub waits slightly longer than the daemon's own execution window. This
/// ensures the hub receives a daemon-level error (e.g. `EXECUTION_TIMEOUT`)
/// rather than racing it with a hub-side timeout.
const GRACE_MS: u64 = 2_000;

/// RAII guard that removes an `app_tool_pending` entry on drop. Ensures
/// cleanup even if the surrounding handler future is cancelled mid-`.await`
/// (e.g. axum drops the future when the HTTP client disconnects or the
/// worker SDK calls `controller.abort()` between `insert` and the oneshot
/// resolving).
///
/// `DashMap::remove` is idempotent (returns `Option<_>`), so the WS
/// gateway's success-path remove at `device_gateway.rs` and this guard's
/// drop coexist safely.
struct PendingGuard<'a> {
    state: &'a AppState,
    tool_call_id: String,
}

impl<'a> PendingGuard<'a> {
    fn new(state: &'a AppState, tool_call_id: String) -> Self {
        Self {
            state,
            tool_call_id,
        }
    }
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        // Idempotent — harmless even if the WS gateway already removed
        // the entry on the success path.
        let _ = self.state.app_tool_pending.remove(&self.tool_call_id);
    }
}

/// Clamp the caller-supplied timeout to `[MIN_TIMEOUT_MS, MAX_TIMEOUT_MS]`.
/// Zero is treated as the default (60 s), matching the daemon's own clamping.
pub fn clamp_timeout(timeout_ms: u64) -> u64 {
    if timeout_ms == 0 {
        DEFAULT_TIMEOUT_MS
    } else {
        timeout_ms.clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS)
    }
}

/// Input to [`invoke`]. The handler is responsible for auth / ownership /
/// rate-limiting and for serializing `args` to `args_json`.
#[derive(Debug, Clone)]
pub struct AppToolInput {
    pub device_id: String,
    pub name: String,
    /// Pre-serialized args JSON object (e.g. `"{}"` when args are absent).
    pub args_json: String,
    /// Pre-serialized trusted invocation context JSON object.
    pub context_json: Option<String>,
    /// Caller-supplied timeout in milliseconds. Will be clamped to
    /// `[MIN_TIMEOUT_MS, MAX_TIMEOUT_MS]` by this function.
    pub timeout_ms: u64,
}

/// Errors that [`invoke`] can return. Mapped to HTTP status codes by the
/// control-plane handler.
///
/// Note: `DeviceNotFound` is intentionally absent — existence + ownership are
/// checked by the handler before calling [`invoke`], so the service only sees
/// devices that are known. Offline detection is at the presence layer.
#[derive(Debug, Error)]
pub enum AppToolServiceError {
    /// Device is known but has no active WS connection. Mapped to **409**
    /// (`DEVICE_OFFLINE`) by the handler — see the 404/409 variant split
    /// note in `control_plane.rs`.
    #[error("device {device_id} is not connected")]
    DeviceOffline { device_id: String },
    /// WS send failed after the online check succeeded (the device went
    /// away in the narrow window between the check and the dispatch).
    /// Mapped to the same 409 as `DeviceOffline`.
    /// `tool_call_id` is the UUID that was minted before the send attempt —
    /// it appears in daemon logs if the frame was partially delivered.
    #[error("failed to send to device {device_id}: {reason}")]
    SendFailed {
        device_id: String,
        reason: String,
        tool_call_id: String,
    },
    /// No response received within `timeout_ms + GRACE_MS`. Mapped to 504.
    /// `tool_call_id` is the UUID sent to the daemon — use it to correlate
    /// hub-side timeout events with daemon-side execution logs.
    #[error("app tool invocation timed out after {ms}ms")]
    Timeout { ms: u64, tool_call_id: String },
    /// Oneshot receiver was dropped without a value (internal inconsistency).
    #[error("response channel closed unexpectedly")]
    ChannelClosed { tool_call_id: String },
}

/// Invoke an app tool on a device and await its response.
///
/// Steps:
/// 1. Verify the device is online via `state.connections.is_online`.
/// 2. Generate a UUID `tool_call_id` and register a oneshot sender in
///    `state.app_tool_pending`.
/// 3. Install an RAII `PendingGuard` so the entry is removed on every exit
///    path, including future cancellation.
/// 4. Build an `AppToolRequest` envelope and dispatch via the WS gateway.
/// 5. Await the response with `timeout_ms + GRACE_MS`.
pub async fn invoke(
    state: &AppState,
    input: AppToolInput,
) -> Result<AppToolResponse, AppToolServiceError> {
    // 1. Presence check (fast-fail — no waiting).
    if !state.connections.is_online(&input.device_id) {
        return Err(AppToolServiceError::DeviceOffline {
            device_id: input.device_id.clone(),
        });
    }

    // 2. Oneshot registration.
    let tool_call_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = tokio::sync::oneshot::channel();
    state.app_tool_pending.insert(tool_call_id.clone(), tx);

    // 3. RAII guard — ensures cleanup even if the handler future is cancelled.
    let _guard = PendingGuard::new(state, tool_call_id.clone());

    // 4. Build envelope + dispatch.
    let clamped = clamp_timeout(input.timeout_ms);
    let envelope = Envelope {
        device_id: input.device_id.clone(),
        msg_id: format!("app-tool-{tool_call_id}"),
        ts_ms: now_ms(),
        payload: Some(ahand_protocol::envelope::Payload::AppToolRequest(
            AppToolRequest {
                tool_call_id: tool_call_id.clone(),
                name: input.name.clone(),
                args_json: input.args_json,
                // Daemon field is u32; clamp guarantees [1_000, 300_000].
                timeout_ms: clamped as u32,
                context_json: input.context_json.unwrap_or_default(),
            },
        )),
        ..Default::default()
    };

    if let Err(err) = state
        .connections
        .send_envelope(&input.device_id, envelope)
        .await
    {
        return Err(AppToolServiceError::SendFailed {
            device_id: input.device_id.clone(),
            reason: err.to_string(),
            tool_call_id: tool_call_id.clone(),
        });
    }

    // 5. Await with deadline (caller timeout + grace so the daemon can reply
    //    with a structured error before we give up).
    let wait = Duration::from_millis(clamped + GRACE_MS);
    match tokio::time::timeout(wait, rx).await {
        Ok(Ok(resp)) => Ok(resp),
        Ok(Err(_)) => Err(AppToolServiceError::ChannelClosed {
            tool_call_id: tool_call_id.clone(),
        }),
        Err(_) => Err(AppToolServiceError::Timeout {
            ms: clamped,
            tool_call_id: tool_call_id.clone(),
        }),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
