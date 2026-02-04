use std::collections::HashMap;
use std::time::Duration;

use ahand_protocol::{ApprovalRequest, ApprovalResponse, JobRequest, RefusalContext};
use tokio::sync::{oneshot, Mutex};
use tracing::info;

/// A pending approval entry.
struct PendingApproval {
    request: JobRequest,
    caller_uid: String,
    #[allow(dead_code)]
    approval_request: ApprovalRequest,
    result_tx: oneshot::Sender<ApprovalResponse>,
}

/// Manages pending approval requests. Shared between WS client and IPC server.
pub struct ApprovalManager {
    pending: Mutex<HashMap<String, PendingApproval>>,
    default_timeout: Duration,
}

impl ApprovalManager {
    pub fn new(timeout_secs: u64) -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            default_timeout: Duration::from_secs(timeout_secs),
        }
    }

    /// Submit a job that needs approval. Returns the ApprovalRequest to broadcast
    /// and a oneshot Receiver that the caller awaits (with timeout).
    pub async fn submit(
        &self,
        req: JobRequest,
        caller_uid: &str,
        reason: String,
        previous_refusals: Vec<RefusalContext>,
    ) -> (ApprovalRequest, oneshot::Receiver<ApprovalResponse>) {
        let (tx, rx) = oneshot::channel();
        let expires_ms = now_ms() + self.default_timeout.as_millis() as u64;

        let approval_req = ApprovalRequest {
            job_id: req.job_id.clone(),
            tool: req.tool.clone(),
            args: req.args.clone(),
            cwd: req.cwd.clone(),
            reason,
            detected_domains: Vec::new(),
            expires_ms,
            caller_uid: caller_uid.to_string(),
            previous_refusals,
        };

        let entry = PendingApproval {
            request: req,
            caller_uid: caller_uid.to_string(),
            approval_request: approval_req.clone(),
            result_tx: tx,
        };

        let job_id = entry.request.job_id.clone();
        self.pending.lock().await.insert(job_id.clone(), entry);

        info!(
            job_id = %job_id,
            caller_uid = caller_uid,
            "approval request submitted"
        );

        (approval_req, rx)
    }

    /// Resolve a pending approval. Sends the response through the oneshot channel
    /// to unblock the waiting task. Returns the (JobRequest, caller_uid) if the
    /// job_id was found, or None if already resolved or expired.
    pub async fn resolve(&self, response: &ApprovalResponse) -> Option<(JobRequest, String)> {
        let entry = self.pending.lock().await.remove(&response.job_id)?;
        let req = entry.request;
        let caller_uid = entry.caller_uid;
        // First-response-wins: if send fails, somebody else already resolved it.
        let _ = entry.result_tx.send(response.clone());
        Some((req, caller_uid))
    }

    /// Remove a timed-out entry. Returns true if it was still pending.
    pub async fn expire(&self, job_id: &str) -> bool {
        self.pending.lock().await.remove(job_id).is_some()
    }

    /// List all currently pending approval requests.
    #[allow(dead_code)]
    pub async fn list_pending(&self) -> Vec<ApprovalRequest> {
        self.pending
            .lock()
            .await
            .values()
            .map(|p| p.approval_request.clone())
            .collect()
    }

    /// The default timeout duration for approval requests.
    pub fn default_timeout(&self) -> Duration {
        self.default_timeout
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
