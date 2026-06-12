use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use ahand_protocol::{ApprovalRequest, ApprovalResponse, JobRequest, RefusalContext};
use tokio::sync::{Mutex, oneshot};
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
///
/// Three `job_id` / `tool` namespaces share this HashMap:
///   1. **Real jobs** — `job_id` is the raw job_id from `JobRequest`; `tool` is
///      whatever the cloud sent (e.g. `"bash"`, `"computer"`).
///   2. **File requests** — `job_id = "file-req:{request_id}"`, `tool = "file"`.
///      Dedicated prefix prevents a file request_id from evicting a same-named
///      real job (see `handle_file_request`, ahand_client.rs ~line 1615).
///   3. **App-tool calls** — `job_id = "app-tool:{tool_call_id}"`,
///      `tool = "app:{name}"`. Dedicated prefix prevents a cloud-chosen
///      `tool_call_id` from colliding with a real job's pending entry
///      (see `handle_app_tool_request`, ahand_client.rs step 3).
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
    ///
    /// The advertised `expires_ms` is set to `now + default_timeout`, matching
    /// the window used for job and file requests.
    pub async fn submit(
        &self,
        req: JobRequest,
        caller_uid: &str,
        reason: String,
        previous_refusals: Vec<RefusalContext>,
    ) -> (ApprovalRequest, oneshot::Receiver<ApprovalResponse>) {
        self.submit_with_timeout(
            req,
            caller_uid,
            reason,
            previous_refusals,
            self.default_timeout,
        )
        .await
    }

    /// Like `submit`, but with an explicit wait bound for this request — the
    /// advertised `expires_ms` must match when the waiter actually gives up.
    /// Job/file callers keep using `submit` (default_timeout applies).
    pub async fn submit_with_timeout(
        &self,
        req: JobRequest,
        caller_uid: &str,
        reason: String,
        previous_refusals: Vec<RefusalContext>,
        timeout: Duration,
    ) -> (ApprovalRequest, oneshot::Receiver<ApprovalResponse>) {
        let (tx, rx) = oneshot::channel();
        let expires_ms = now_ms() + timeout.as_millis() as u64;

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

/// Shared terminal handling for an [`ApprovalResponse`] arriving from any
/// surface (cloud WS, local IPC, in-process embedder): resolve the pending
/// entry; a denial carrying a non-empty reason records a refusal for the
/// resolved tool. Returns `true` when a pending entry was resolved.
///
/// Callers that want a surface-specific log line (e.g. "received approval
/// response from cloud") should emit it **before** calling this helper.
/// The `principal` field identifies the source for future per-principal
/// refusal tracking (currently ignored by `SessionManager::record_refusal`,
/// which is tool-keyed today).
pub(crate) async fn apply_approval_response(
    approval_mgr: &Arc<ApprovalManager>,
    session_mgr: &Arc<crate::session::SessionManager>,
    resp: &ApprovalResponse,
    principal: &str,
) -> bool {
    info!(
        job_id = %resp.job_id,
        approved = resp.approved,
        principal,
        "applying approval response"
    );
    if !resp.approved && !resp.reason.is_empty() {
        if let Some((req, _)) = approval_mgr.resolve(resp).await {
            session_mgr
                .record_refusal(principal, &req.tool, &resp.reason)
                .await;
            return true;
        }
        false
    } else {
        approval_mgr.resolve(resp).await.is_some()
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use ahand_protocol::JobRequest;

    fn make_job_request(job_id: &str) -> JobRequest {
        JobRequest {
            job_id: job_id.to_string(),
            tool: "test_tool".to_string(),
            cwd: "/tmp".to_string(),
            ..Default::default()
        }
    }

    /// submit_with_timeout: expires_ms ≈ now + bound (±2 s tolerance).
    #[tokio::test]
    async fn submit_with_timeout_sets_correct_expiry() {
        let mgr = ApprovalManager::new(86400); // 24 h default_timeout
        let bound = Duration::from_secs(5);
        let before_ms = now_ms();
        let (approval_req, _rx) = mgr
            .submit_with_timeout(
                make_job_request("job-1"),
                "uid-1",
                "reason".to_string(),
                vec![],
                bound,
            )
            .await;
        let after_ms = now_ms();

        let expected_lo = before_ms + bound.as_millis() as u64;
        let expected_hi = after_ms + bound.as_millis() as u64;
        assert!(
            approval_req.expires_ms >= expected_lo,
            "expires_ms {} below lower bound {}",
            approval_req.expires_ms,
            expected_lo
        );
        assert!(
            approval_req.expires_ms <= expected_hi + 2_000,
            "expires_ms {} exceeds upper bound {} + 2 s slack",
            approval_req.expires_ms,
            expected_hi
        );
    }

    /// submit delegates to submit_with_timeout using default_timeout.
    /// Verify expires_ms ≈ now + default_timeout (not the explicit bound).
    #[tokio::test]
    async fn submit_uses_default_timeout_for_expiry() {
        let default_secs: u64 = 60;
        let mgr = ApprovalManager::new(default_secs);
        let before_ms = now_ms();
        let (approval_req, _rx) = mgr
            .submit(
                make_job_request("job-2"),
                "uid-2",
                "reason".to_string(),
                vec![],
            )
            .await;
        let after_ms = now_ms();

        let expected_lo = before_ms + default_secs * 1_000;
        let expected_hi = after_ms + default_secs * 1_000;
        assert!(
            approval_req.expires_ms >= expected_lo,
            "expires_ms {} below lower bound {}",
            approval_req.expires_ms,
            expected_lo
        );
        assert!(
            approval_req.expires_ms <= expected_hi + 2_000,
            "expires_ms {} exceeds upper bound {} + 2 s slack",
            approval_req.expires_ms,
            expected_hi
        );
    }
}
