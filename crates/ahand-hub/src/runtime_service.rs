use std::sync::Arc;
use std::time::Duration;

use ahand_protocol::{Envelope, RuntimeRequest, RuntimeResponse, envelope};
use thiserror::Error;

use crate::pending_runtime_requests::{PendingRuntimeRequests, PendingRuntimeRequestsError};
use crate::state::AppState;

struct PendingSlotGuard {
    table: Arc<PendingRuntimeRequests>,
    device_id: String,
    request_id: String,
}

impl Drop for PendingSlotGuard {
    fn drop(&mut self) {
        self.table.cancel(&self.device_id, &self.request_id);
    }
}

#[derive(Debug, Error)]
pub enum RuntimeServiceError {
    #[error("pending runtime-request table at capacity")]
    AtCapacity,
    #[error("request_id {request_id} is already in flight for device {device_id}")]
    Duplicate {
        device_id: String,
        request_id: String,
    },
    #[error("device {device_id} is not connected")]
    DeviceOffline { device_id: String },
    #[error("device did not respond within {ms}ms")]
    Timeout { ms: u128 },
    #[error("response channel closed unexpectedly")]
    ChannelClosed,
    #[error("internal error: {0}")]
    Internal(String),
}

pub async fn execute(
    state: &AppState,
    device_id: &str,
    request: RuntimeRequest,
    timeout: Duration,
) -> Result<RuntimeResponse, RuntimeServiceError> {
    debug_assert!(
        !request.request_id.is_empty(),
        "runtime_service::execute requires a non-empty request_id"
    );

    let request_id = request.request_id.clone();
    let rx = state
        .pending_runtime_requests
        .register(device_id, &request_id)
        .map_err(|err| match err {
            PendingRuntimeRequestsError::AtCapacity => RuntimeServiceError::AtCapacity,
            PendingRuntimeRequestsError::Duplicate => RuntimeServiceError::Duplicate {
                device_id: device_id.to_string(),
                request_id: request_id.clone(),
            },
        })?;

    let _slot_guard = PendingSlotGuard {
        table: state.pending_runtime_requests.clone(),
        device_id: device_id.to_string(),
        request_id: request_id.clone(),
    };

    let envelope = Envelope {
        device_id: device_id.to_string(),
        msg_id: format!("runtime-{request_id}"),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::RuntimeRequest(request)),
        ..Default::default()
    };

    if let Err(err) = state.connections.send_envelope(device_id, envelope).await {
        return Err(match err.downcast_ref::<ahand_hub_core::HubError>() {
            Some(ahand_hub_core::HubError::DeviceOffline(_)) => {
                RuntimeServiceError::DeviceOffline {
                    device_id: device_id.to_string(),
                }
            }
            _ => RuntimeServiceError::Internal(err.to_string()),
        });
    }

    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(resp)) => Ok(resp),
        Ok(Err(_)) => Err(RuntimeServiceError::ChannelClosed),
        Err(_) => Err(RuntimeServiceError::Timeout {
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
