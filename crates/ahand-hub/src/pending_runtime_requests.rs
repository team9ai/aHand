use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ahand_protocol::RuntimeResponse;
use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use tokio::sync::oneshot;

pub const MAX_PENDING_RUNTIME_REQUESTS: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingRuntimeRequestsError {
    AtCapacity,
    Duplicate,
}

pub struct PendingRuntimeRequests {
    pending: DashMap<(String, String), oneshot::Sender<RuntimeResponse>>,
    capacity: usize,
    in_flight: AtomicUsize,
}

impl PendingRuntimeRequests {
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
    ) -> Result<oneshot::Receiver<RuntimeResponse>, PendingRuntimeRequestsError> {
        let new_count = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        if new_count > self.capacity {
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            return Err(PendingRuntimeRequestsError::AtCapacity);
        }

        let key = (device_id.to_string(), request_id.to_string());
        match self.pending.entry(key) {
            Entry::Occupied(_) => {
                self.in_flight.fetch_sub(1, Ordering::SeqCst);
                Err(PendingRuntimeRequestsError::Duplicate)
            }
            Entry::Vacant(slot) => {
                let (tx, rx) = oneshot::channel();
                slot.insert(tx);
                Ok(rx)
            }
        }
    }

    pub fn resolve(&self, device_id: &str, request_id: &str, response: RuntimeResponse) {
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
}

impl Default for PendingRuntimeRequests {
    fn default() -> Self {
        Self::new(MAX_PENDING_RUNTIME_REQUESTS)
    }
}

pub fn new_pending_requests() -> Arc<PendingRuntimeRequests> {
    Arc::new(PendingRuntimeRequests::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_response(request_id: &str) -> RuntimeResponse {
        RuntimeResponse {
            request_id: request_id.to_string(),
            success: true,
            result_json: "{}".to_string(),
            error: None,
        }
    }

    #[tokio::test]
    async fn pending_runtime_requests_are_keyed_by_device_and_request() {
        let table = PendingRuntimeRequests::default();
        let rx_a = table.register("device-a", "req-1").unwrap();
        let _rx_b = table.register("device-b", "req-1").unwrap();

        table.resolve("device-b", "req-1", ok_response("req-1"));

        let poll = tokio::time::timeout(std::time::Duration::from_millis(25), rx_a).await;
        assert!(poll.is_err());
    }

    #[test]
    fn pending_runtime_requests_reject_duplicate_keys() {
        let table = PendingRuntimeRequests::default();
        let _first = table.register("device-a", "req-1").unwrap();
        let second = table.register("device-a", "req-1");
        assert_eq!(second.err(), Some(PendingRuntimeRequestsError::Duplicate));
    }
}
