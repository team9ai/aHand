//! Command handler for OpenClaw node invocations.
//!
//! Dispatches incoming commands to appropriate handlers and maps results
//! to OpenClaw protocol responses.

use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ahand_protocol::{ApprovalResponse, Envelope, JobRequest, envelope};
use serde::Deserialize;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::broadcast;
use tracing::debug;

use crate::approval::ApprovalManager;
use crate::browser::BrowserManager;
use crate::registry::JobRegistry;
use crate::session::{SessionDecision, SessionManager};
use crate::store::RunStore;

use super::exec_approvals::{
    default_exec_approvals_path, normalize_exec_approvals, read_exec_approvals_snapshot,
    redact_exec_approvals, save_exec_approvals,
};
use super::protocol::{
    ExecApprovalsSetParams, ExecApprovalsSnapshot, ExecEventPayload, InvokeError,
    NodeInvokeRequest, NodeInvokeResult, OUTPUT_CAP, OUTPUT_EVENT_TAIL, RunResult, SystemRunParams,
    SystemWhichParams, SystemWhichResult,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecEventKind {
    Finished,
    Denied,
}

impl ExecEventKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Finished => "exec.finished",
            Self::Denied => "exec.denied",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ExecEvent {
    pub kind: ExecEventKind,
    pub payload: ExecEventPayload,
}

/// Handler for OpenClaw node invocations
#[allow(dead_code)] // registry + store consumed via Arc; fields read in future methods
pub struct OpenClawHandler {
    node_id: String,
    registry: Arc<JobRegistry>,
    session_mgr: Arc<SessionManager>,
    approval_mgr: Arc<ApprovalManager>,
    approval_broadcast_tx: broadcast::Sender<Envelope>,
    store: Option<Arc<RunStore>>,
    exec_approvals_path: PathBuf,
    browser_mgr: Arc<BrowserManager>,
}

#[allow(clippy::too_many_arguments)] // constructor mirrors the struct fields 1:1; grouping would obscure intent
impl OpenClawHandler {
    pub fn new(
        node_id: String,
        registry: Arc<JobRegistry>,
        session_mgr: Arc<SessionManager>,
        approval_mgr: Arc<ApprovalManager>,
        approval_broadcast_tx: broadcast::Sender<Envelope>,
        store: Option<Arc<RunStore>>,
        exec_approvals_path: Option<PathBuf>,
        browser_mgr: Arc<BrowserManager>,
    ) -> Self {
        Self {
            node_id,
            registry,
            session_mgr,
            approval_mgr,
            approval_broadcast_tx,
            store,
            exec_approvals_path: exec_approvals_path.unwrap_or_else(default_exec_approvals_path),
            browser_mgr,
        }
    }

    /// Handle a node.invoke.request
    pub async fn handle_invoke(
        &self,
        invoke: NodeInvokeRequest,
    ) -> (NodeInvokeResult, Option<ExecEvent>) {
        let command = invoke.command.as_str();

        debug!(
            id = %invoke.id,
            command = %command,
            "handling invoke request"
        );

        let (result, event) = match command {
            "system.run" => self.handle_system_run(&invoke).await,
            "system.which" => {
                let result = self.handle_system_which(&invoke).await;
                (result, None)
            }
            "system.execApprovals.get" => {
                let result = self.handle_exec_approvals_get(&invoke).await;
                (result, None)
            }
            "system.execApprovals.set" => {
                let result = self.handle_exec_approvals_set(&invoke).await;
                (result, None)
            }
            "browser.proxy" => {
                let result = self.handle_browser_proxy(&invoke).await;
                (result, None)
            }
            _ => {
                let result = NodeInvokeResult {
                    id: invoke.id.clone(),
                    node_id: self.node_id.clone(),
                    ok: false,
                    payload_json: None,
                    error: Some(InvokeError::unavailable("command not supported")),
                };
                (result, None)
            }
        };

        (result, event)
    }

    /// Handle system.run command
    async fn handle_system_run(
        &self,
        invoke: &NodeInvokeRequest,
    ) -> (NodeInvokeResult, Option<ExecEvent>) {
        let params: SystemRunParams = match decode_params(&invoke.params_json) {
            Ok(p) => p,
            Err(e) => {
                return (
                    NodeInvokeResult {
                        id: invoke.id.clone(),
                        node_id: self.node_id.clone(),
                        ok: false,
                        payload_json: None,
                        error: Some(e),
                    },
                    None,
                );
            }
        };

        if params.command.is_empty() {
            return (
                NodeInvokeResult {
                    id: invoke.id.clone(),
                    node_id: self.node_id.clone(),
                    ok: false,
                    payload_json: None,
                    error: Some(InvokeError::invalid_request("command required")),
                },
                None,
            );
        }

        let session_key = params
            .session_key
            .clone()
            .unwrap_or_else(|| "openclaw".to_string());
        let run_id = params.run_id.clone().unwrap_or_else(|| invoke.id.clone());
        let cmd_text = format_command(&params.command);
        let request = build_job_request(invoke, &params, &run_id);

        match self.session_mgr.check(&request, &session_key).await {
            SessionDecision::Deny(reason) => {
                return self.denied_system_run(invoke, &session_key, &run_id, &cmd_text, reason);
            }
            SessionDecision::Allow => {}
            SessionDecision::NeedsApproval {
                reason,
                previous_refusals,
            } => match approval_disposition(&params) {
                ApprovalDisposition::Granted => {}
                ApprovalDisposition::Denied => {
                    return self.denied_system_run(
                        invoke,
                        &session_key,
                        &run_id,
                        &cmd_text,
                        "approval denied".to_string(),
                    );
                }
                ApprovalDisposition::Missing => {
                    let outcome = self
                        .await_local_approval(&request, &session_key, reason, previous_refusals)
                        .await;
                    match outcome {
                        ApprovalOutcome::Approved => {}
                        ApprovalOutcome::Denied(reason) => {
                            return self.denied_system_run(
                                invoke,
                                &session_key,
                                &run_id,
                                &cmd_text,
                                reason,
                            );
                        }
                        ApprovalOutcome::TimedOut => {
                            return self.denied_system_run(
                                invoke,
                                &session_key,
                                &run_id,
                                &cmd_text,
                                "approval timed out".to_string(),
                            );
                        }
                    }
                }
            },
        }

        let result = self.run_command(&params).await;
        let invoke_result = invoke_result_from_run(invoke, &self.node_id, &result);
        let event = ExecEvent {
            kind: ExecEventKind::Finished,
            payload: exec_event_payload(&session_key, &run_id, &cmd_text, &result, None),
        };

        (invoke_result, Some(event))
    }

    /// Execute a command and collect output.
    ///
    /// Two execution paths exist:
    ///
    /// **Shell path** (`raw_command` set, or `command` has exactly 1 element):
    /// The string is passed to the platform shell via `shell -c <string>`, which
    /// supports shell builtins, pipes, redirections, etc.
    ///
    /// **Direct path** (`command` has 2+ elements):
    /// `Command::new(cmd[0]).args(cmd[1..])` — argv boundaries are preserved
    /// exactly as the caller specified them.  The shell is never involved, so
    /// special characters in arguments (`&`, `|`, `>`, `^`, `%`, etc.) are
    /// passed through to the child process verbatim and cannot smuggle additional
    /// shell commands regardless of platform.  Shell builtins are not available
    /// in this form (they were unreliable before too — POSIX escaping only
    /// approximated safety on Windows cmd.exe).
    async fn run_command(&self, params: &SystemRunParams) -> RunResult {
        let cwd = params.cwd.as_deref().filter(|s| !s.is_empty());
        let env_overrides = params.env.as_ref();
        let timeout_ms = params.timeout_ms.or(Some(120_000)); // default 2 minutes

        let command_env = env_overrides
            .map(sanitize_env)
            .unwrap_or_else(|| env::vars().collect());
        let path_val = crate::plugin_runtime::path_env::child_process_path(&command_env).await;

        // Decide between direct spawn (array with 2+ elements) and shell spawn.
        let use_direct = params.raw_command.is_none() && params.command.len() >= 2;

        let mut cmd: Command = if use_direct {
            // Direct spawn: argv boundaries are preserved — no shell, no injection risk.
            let exe = &params.command[0];
            debug!(exe = %exe, args = ?&params.command[1..], "executing command directly (no shell)");
            let mut c = Command::new(exe);
            c.args(&params.command[1..]);
            c
        } else {
            // Shell spawn: raw_command or single-element command string.
            let shell_cmd = params
                .raw_command
                .clone()
                .unwrap_or_else(|| params.command.first().cloned().unwrap_or_default());
            debug!(shell_cmd = %shell_cmd, "executing command via shell");
            let shell = ahand_platform::shell::env_shell()
                .unwrap_or_else(|| ahand_platform::shell::default_shell().path);
            let mut c = Command::new(shell);
            c.arg(ahand_platform::shell::shell_c_flag()).arg(&shell_cmd);
            c
        };

        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }

        for (key, value) in &command_env {
            if !crate::plugin_runtime::path_env::is_path_env_key(key) {
                cmd.env(key, value);
            }
        }
        cmd.env("PATH", path_val);

        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return RunResult {
                    exit_code: None,
                    timed_out: false,
                    success: false,
                    stdout: String::new(),
                    stderr: String::new(),
                    error: Some(e.to_string()),
                };
            }
        };

        // Read stdout and stderr concurrently to avoid deadlock
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();

        let stdout_task = tokio::spawn(async move {
            let mut output = String::new();
            if let Some(pipe) = stdout_pipe {
                let mut reader = BufReader::new(pipe);
                let mut line = String::new();
                while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                    if output.len() < OUTPUT_CAP {
                        output.push_str(&line);
                    }
                    line.clear();
                }
            }
            output
        });

        let stderr_task = tokio::spawn(async move {
            let mut output = String::new();
            if let Some(pipe) = stderr_pipe {
                let mut reader = BufReader::new(pipe);
                let mut line = String::new();
                while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                    if output.len() < OUTPUT_CAP {
                        output.push_str(&line);
                    }
                    line.clear();
                }
            }
            output
        });

        // Wait with timeout
        let timeout = timeout_ms.map(Duration::from_millis);
        let (exit_code, timed_out) = if let Some(dur) = timeout {
            match tokio::time::timeout(dur, child.wait()).await {
                Ok(Ok(status)) => (status.code(), false),
                Ok(Err(_)) => (None, false),
                Err(_) => {
                    // Timeout - kill the process
                    let _ = child.kill().await;
                    (None, true)
                }
            }
        } else {
            match child.wait().await {
                Ok(status) => (status.code(), false),
                Err(_) => (None, false),
            }
        };

        // Collect output from tasks
        let mut stdout = stdout_task.await.unwrap_or_default();
        let mut stderr = stderr_task.await.unwrap_or_default();

        let truncated = stdout.len() >= OUTPUT_CAP || stderr.len() >= OUTPUT_CAP;
        if truncated {
            let suffix = "... (truncated)";
            if !stderr.is_empty() {
                stderr.push_str(suffix);
            } else {
                stdout.push_str(suffix);
            }
        }

        let success = exit_code == Some(0) && !timed_out;

        RunResult {
            exit_code,
            timed_out,
            success,
            stdout,
            stderr,
            error: None,
        }
    }

    async fn await_local_approval(
        &self,
        request: &JobRequest,
        caller_uid: &str,
        reason: String,
        previous_refusals: Vec<ahand_protocol::RefusalContext>,
    ) -> ApprovalOutcome {
        let (approval_req, approval_rx) = self
            .approval_mgr
            .submit(request.clone(), caller_uid, reason, previous_refusals)
            .await;
        let approval_env = Envelope {
            device_id: self.node_id.clone(),
            msg_id: new_msg_id(),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::ApprovalRequest(approval_req)),
            ..Default::default()
        };
        let _ = self.approval_broadcast_tx.send(approval_env);

        match tokio::time::timeout(self.approval_mgr.default_timeout(), approval_rx).await {
            Ok(Ok(resp)) if resp.approved => ApprovalOutcome::Approved,
            Ok(Ok(resp)) => {
                if !resp.reason.is_empty() {
                    self.session_mgr
                        .record_refusal(caller_uid, &request.tool, &resp.reason)
                        .await;
                }
                self.approval_mgr.expire(&request.job_id).await;
                ApprovalOutcome::Denied(approval_denied_reason(&resp))
            }
            _ => {
                self.approval_mgr.expire(&request.job_id).await;
                ApprovalOutcome::TimedOut
            }
        }
    }

    fn denied_system_run(
        &self,
        invoke: &NodeInvokeRequest,
        session_key: &str,
        run_id: &str,
        cmd_text: &str,
        reason: String,
    ) -> (NodeInvokeResult, Option<ExecEvent>) {
        let result = denied_run_result(reason.clone());
        let invoke_result = invoke_result_from_run(invoke, &self.node_id, &result);
        let event = ExecEvent {
            kind: ExecEventKind::Denied,
            payload: exec_event_payload(session_key, run_id, cmd_text, &result, Some(reason)),
        };
        (invoke_result, Some(event))
    }

    /// Handle system.which command
    async fn handle_system_which(&self, invoke: &NodeInvokeRequest) -> NodeInvokeResult {
        let params: SystemWhichParams = match decode_params(&invoke.params_json) {
            Ok(p) => p,
            Err(e) => {
                return NodeInvokeResult {
                    id: invoke.id.clone(),
                    node_id: self.node_id.clone(),
                    ok: false,
                    payload_json: None,
                    error: Some(e),
                };
            }
        };

        let mut found: HashMap<String, String> = HashMap::new();
        let base_path = env::var("PATH").unwrap_or_default();
        let path_env =
            crate::plugin_runtime::path_env::path_with_installed_runtime_bins_or_base(&base_path)
                .await;
        let path_dirs: Vec<PathBuf> = std::env::split_paths(&path_env).collect();

        for bin in &params.bins {
            let bin = bin.trim();
            if bin.is_empty() || bin.contains('/') || bin.contains('\\') {
                continue;
            }

            'dirs: for dir in &path_dirs {
                if let Some(p) = which_in_dir(dir, bin) {
                    found.insert(bin.to_string(), p.to_string_lossy().to_string());
                    break 'dirs;
                }
            }
        }

        let result = SystemWhichResult { bins: found };

        NodeInvokeResult {
            id: invoke.id.clone(),
            node_id: self.node_id.clone(),
            ok: true,
            payload_json: Some(serde_json::to_string(&result).unwrap_or_default()),
            error: None,
        }
    }

    /// Handle system.execApprovals.get command
    async fn handle_exec_approvals_get(&self, invoke: &NodeInvokeRequest) -> NodeInvokeResult {
        match read_exec_approvals_snapshot(&self.exec_approvals_path) {
            Ok(snapshot) => {
                let redacted = ExecApprovalsSnapshot {
                    path: snapshot.path,
                    exists: snapshot.exists,
                    hash: snapshot.hash,
                    file: redact_exec_approvals(snapshot.file),
                };
                NodeInvokeResult {
                    id: invoke.id.clone(),
                    node_id: self.node_id.clone(),
                    ok: true,
                    payload_json: Some(serde_json::to_string(&redacted).unwrap_or_default()),
                    error: None,
                }
            }
            Err(e) => NodeInvokeResult {
                id: invoke.id.clone(),
                node_id: self.node_id.clone(),
                ok: false,
                payload_json: None,
                error: Some(InvokeError::invalid_request(e.to_string())),
            },
        }
    }

    /// Handle browser.proxy command.
    ///
    /// Accepts the OpenClaw HTTP-style proxy protocol:
    ///   { method, path, query, body, timeoutMs, profile }
    /// Translates HTTP routes to playwright-cli calls via BrowserManager,
    /// and returns results in the OpenClaw-compatible format:
    ///   { result, files: [{ path, base64, mimeType }] }
    async fn handle_browser_proxy(&self, invoke: &NodeInvokeRequest) -> NodeInvokeResult {
        if !self.browser_mgr.is_enabled() {
            return NodeInvokeResult {
                id: invoke.id.clone(),
                node_id: self.node_id.clone(),
                ok: false,
                payload_json: None,
                error: Some(InvokeError::unavailable("browser not enabled")),
            };
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct BrowserProxyParams {
            #[serde(default)]
            method: Option<String>,
            #[serde(default)]
            path: Option<String>,
            #[serde(default)]
            query: Option<serde_json::Value>,
            #[serde(default)]
            body: Option<serde_json::Value>,
            #[serde(default)]
            timeout_ms: Option<u64>,
            #[serde(default)]
            profile: Option<String>,
        }

        let params: BrowserProxyParams = match decode_params(&invoke.params_json) {
            Ok(p) => p,
            Err(e) => {
                return NodeInvokeResult {
                    id: invoke.id.clone(),
                    node_id: self.node_id.clone(),
                    ok: false,
                    payload_json: None,
                    error: Some(e),
                };
            }
        };

        let path = params.path.as_deref().unwrap_or("").trim().to_string();
        if path.is_empty() {
            return NodeInvokeResult {
                id: invoke.id.clone(),
                node_id: self.node_id.clone(),
                ok: false,
                payload_json: None,
                error: Some(InvokeError::invalid_request("path required")),
            };
        }

        let method = params.method.as_deref().unwrap_or("GET").to_uppercase();
        let body = params
            .body
            .unwrap_or(serde_json::Value::Object(Default::default()));
        let query = params
            .query
            .unwrap_or(serde_json::Value::Object(Default::default()));
        let session_id = params.profile.as_deref().unwrap_or("default").to_string();
        let timeout_ms = params.timeout_ms.unwrap_or(0);

        // Translate the HTTP route to a CLI action + params_json for BrowserManager.
        let translated = translate_http_to_cli(&method, &path, &body, &query);
        let (action, action_params_json) = match translated {
            Ok(t) => t,
            Err(msg) => {
                return NodeInvokeResult {
                    id: invoke.id.clone(),
                    node_id: self.node_id.clone(),
                    ok: false,
                    payload_json: None,
                    error: Some(InvokeError::invalid_request(msg)),
                };
            }
        };

        // Check domain restrictions for navigation actions.
        if let Err(msg) = self.browser_mgr.check_domain(&action, &action_params_json) {
            return NodeInvokeResult {
                id: invoke.id.clone(),
                node_id: self.node_id.clone(),
                ok: false,
                payload_json: None,
                error: Some(InvokeError::new("PERMISSION_DENIED", msg)),
            };
        }

        // Special handling for download (filesystem polling) and wait (eval polling).
        let exec_result = match action.as_str() {
            "download" => {
                let ref_sel = serde_json::from_str::<serde_json::Value>(&action_params_json)
                    .ok()
                    .and_then(|v| {
                        v.get("selector")
                            .or(v.get("ref"))
                            .and_then(|s| s.as_str())
                            .map(String::from)
                    })
                    .unwrap_or_default();
                self.browser_mgr
                    .execute_download(&session_id, &ref_sel, timeout_ms)
                    .await
            }
            "wait" => {
                let text = serde_json::from_str::<serde_json::Value>(&action_params_json)
                    .ok()
                    .and_then(|v| v.get("text").and_then(|s| s.as_str()).map(String::from))
                    .unwrap_or_default();
                if text.is_empty() {
                    // Pure delay wait — use eval with Promise.
                    let delay_ms = serde_json::from_str::<serde_json::Value>(&action_params_json)
                        .ok()
                        .and_then(|v| {
                            v.get("timeMs")
                                .or(v.get("timeout"))
                                .and_then(|t| t.as_u64())
                        })
                        .unwrap_or(1000);
                    let js = format!("() => new Promise(r => setTimeout(r, {}))", delay_ms);
                    let params = serde_json::json!({ "expression": js });
                    self.browser_mgr
                        .execute(&session_id, "eval", &params.to_string(), timeout_ms)
                        .await
                } else {
                    self.browser_mgr
                        .execute_wait_for_text(&session_id, &text, timeout_ms)
                        .await
                }
            }
            // fill with submit: true requires an additional press Enter.
            "fill" => {
                let submit = serde_json::from_str::<serde_json::Value>(&action_params_json)
                    .ok()
                    .and_then(|v| v.get("submit").and_then(|s| s.as_bool()))
                    .unwrap_or(false);
                let result = self
                    .browser_mgr
                    .execute(&session_id, &action, &action_params_json, timeout_ms)
                    .await;
                if submit
                    && let Ok(ref r) = result
                    && r.success
                {
                    let press_params = serde_json::json!({ "key": "Enter" });
                    let _ = self
                        .browser_mgr
                        .execute(&session_id, "press", &press_params.to_string(), timeout_ms)
                        .await;
                }
                result
            }
            _ => {
                self.browser_mgr
                    .execute(&session_id, &action, &action_params_json, timeout_ms)
                    .await
            }
        };

        match exec_result {
            Ok(result) => {
                // Release session on "close" action.
                if action == "close" {
                    self.browser_mgr.release_session(&session_id).await;
                }

                // Build OpenClaw-compatible response: { result, files }
                // playwright-cli outputs plain text (not JSON), so we try
                // JSON parse first (for backwards compat), then fall back to
                // wrapping the text as a JSON string value.
                let result_value: serde_json::Value = if !result.result_json.is_empty() {
                    serde_json::from_str(&result.result_json)
                        .unwrap_or_else(|_| serde_json::Value::String(result.result_json.clone()))
                } else if !result.error.is_empty() {
                    serde_json::json!({ "error": result.error })
                } else {
                    serde_json::Value::Null
                };

                // Wrap with success status so the caller knows the outcome.
                let wrapped_result = serde_json::json!({
                    "success": result.success,
                    "data": result_value,
                });

                let mut files = Vec::<serde_json::Value>::new();
                if !result.binary_data.is_empty() {
                    use base64::Engine;
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&result.binary_data);
                    // Extract original file path from result data if available.
                    let file_path = result_value
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("output.bin")
                        .to_string();
                    files.push(serde_json::json!({
                        "path": file_path,
                        "base64": b64,
                        "mimeType": result.binary_mime,
                    }));
                }

                let proxy_response = serde_json::json!({
                    "result": wrapped_result,
                    "files": files,
                });

                NodeInvokeResult {
                    id: invoke.id.clone(),
                    node_id: self.node_id.clone(),
                    ok: result.success,
                    payload_json: Some(serde_json::to_string(&proxy_response).unwrap_or_default()),
                    error: if result.success {
                        None
                    } else {
                        Some(InvokeError::new(
                            "BROWSER_ERROR",
                            if !result.error.is_empty() {
                                result.error.clone()
                            } else {
                                "browser command failed".to_string()
                            },
                        ))
                    },
                }
            }
            Err(e) => NodeInvokeResult {
                id: invoke.id.clone(),
                node_id: self.node_id.clone(),
                ok: false,
                payload_json: None,
                error: Some(InvokeError::new("INTERNAL", e.to_string())),
            },
        }
    }

    /// Handle system.execApprovals.set command
    async fn handle_exec_approvals_set(&self, invoke: &NodeInvokeRequest) -> NodeInvokeResult {
        let params: ExecApprovalsSetParams = match decode_params(&invoke.params_json) {
            Ok(p) => p,
            Err(e) => {
                return NodeInvokeResult {
                    id: invoke.id.clone(),
                    node_id: self.node_id.clone(),
                    ok: false,
                    payload_json: None,
                    error: Some(e),
                };
            }
        };

        // Read current state to verify base hash
        let current = match read_exec_approvals_snapshot(&self.exec_approvals_path) {
            Ok(s) => s,
            Err(e) => {
                return NodeInvokeResult {
                    id: invoke.id.clone(),
                    node_id: self.node_id.clone(),
                    ok: false,
                    payload_json: None,
                    error: Some(InvokeError::invalid_request(e.to_string())),
                };
            }
        };

        // Verify base hash if file exists
        if current.exists {
            if let Some(base_hash) = &params.base_hash {
                if base_hash != &current.hash {
                    return NodeInvokeResult {
                        id: invoke.id.clone(),
                        node_id: self.node_id.clone(),
                        ok: false,
                        payload_json: None,
                        error: Some(InvokeError::invalid_request(
                            "exec approvals changed; reload and retry",
                        )),
                    };
                }
            } else {
                return NodeInvokeResult {
                    id: invoke.id.clone(),
                    node_id: self.node_id.clone(),
                    ok: false,
                    payload_json: None,
                    error: Some(InvokeError::invalid_request(
                        "exec approvals base hash required; reload and retry",
                    )),
                };
            }
        }

        // Normalize and save
        let normalized = normalize_exec_approvals(params.file);
        if let Err(e) = save_exec_approvals(&self.exec_approvals_path, &normalized) {
            return NodeInvokeResult {
                id: invoke.id.clone(),
                node_id: self.node_id.clone(),
                ok: false,
                payload_json: None,
                error: Some(InvokeError::invalid_request(e.to_string())),
            };
        }

        // Read and return updated snapshot
        match read_exec_approvals_snapshot(&self.exec_approvals_path) {
            Ok(snapshot) => {
                let redacted = ExecApprovalsSnapshot {
                    path: snapshot.path,
                    exists: snapshot.exists,
                    hash: snapshot.hash,
                    file: redact_exec_approvals(snapshot.file),
                };
                NodeInvokeResult {
                    id: invoke.id.clone(),
                    node_id: self.node_id.clone(),
                    ok: true,
                    payload_json: Some(serde_json::to_string(&redacted).unwrap_or_default()),
                    error: None,
                }
            }
            Err(e) => NodeInvokeResult {
                id: invoke.id.clone(),
                node_id: self.node_id.clone(),
                ok: false,
                payload_json: None,
                error: Some(InvokeError::invalid_request(e.to_string())),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApprovalDisposition {
    Granted,
    Denied,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ApprovalOutcome {
    Approved,
    Denied(String),
    TimedOut,
}

fn approval_disposition(params: &SystemRunParams) -> ApprovalDisposition {
    if params.approved == Some(true)
        || matches!(
            params.approval_decision.as_deref(),
            Some("allow-once" | "allow-always")
        )
    {
        ApprovalDisposition::Granted
    } else if params.approved == Some(false)
        || matches!(
            params.approval_decision.as_deref(),
            Some("deny" | "reject" | "blocked")
        )
    {
        ApprovalDisposition::Denied
    } else {
        ApprovalDisposition::Missing
    }
}

fn build_job_request(
    invoke: &NodeInvokeRequest,
    params: &SystemRunParams,
    run_id: &str,
) -> JobRequest {
    let tool = params
        .command
        .first()
        .cloned()
        .unwrap_or_else(|| "sh".to_string());
    let args = if params.command.len() > 1 {
        params.command[1..].to_vec()
    } else if let Some(raw_command) = &params.raw_command {
        vec![raw_command.clone()]
    } else {
        Vec::new()
    };

    JobRequest {
        job_id: run_id.to_string(),
        tool,
        args,
        cwd: params.cwd.clone().unwrap_or_default(),
        env: params.env.clone().unwrap_or_default(),
        timeout_ms: params.timeout_ms.or(invoke.timeout_ms).unwrap_or(120_000),
        interactive: false,
    }
}

fn denied_run_result(reason: String) -> RunResult {
    RunResult {
        exit_code: None,
        timed_out: false,
        success: false,
        stdout: String::new(),
        stderr: String::new(),
        error: Some(reason),
    }
}

fn approval_denied_reason(resp: &ApprovalResponse) -> String {
    if resp.reason.is_empty() {
        "approval denied".to_string()
    } else {
        format!("approval denied: {}", resp.reason)
    }
}

fn invoke_result_from_run(
    invoke: &NodeInvokeRequest,
    node_id: &str,
    result: &RunResult,
) -> NodeInvokeResult {
    NodeInvokeResult {
        id: invoke.id.clone(),
        node_id: node_id.to_string(),
        ok: true,
        payload_json: Some(serde_json::to_string(result).unwrap_or_default()),
        error: None,
    }
}

fn exec_event_payload(
    session_key: &str,
    run_id: &str,
    cmd_text: &str,
    result: &RunResult,
    reason: Option<String>,
) -> ExecEventPayload {
    ExecEventPayload {
        session_key: session_key.to_string(),
        run_id: run_id.to_string(),
        host: "node".to_string(),
        command: Some(cmd_text.to_string()),
        exit_code: result.exit_code,
        timed_out: Some(result.timed_out),
        success: Some(result.success),
        output: Some(truncate_output(
            &[
                &result.stdout,
                &result.stderr,
                result.error.as_deref().unwrap_or(""),
            ]
            .iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join("\n"),
            OUTPUT_EVENT_TAIL,
        )),
        reason,
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn new_msg_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Translate an OpenClaw HTTP-style browser proxy request into a CLI action
/// and params JSON that `BrowserManager::execute()` understands.
///
/// OpenClaw sends `{ method, path, query, body }` matching its internal
/// browser control HTTP routes.  We map these to playwright-cli commands.
fn translate_http_to_cli(
    method: &str,
    path: &str,
    body: &serde_json::Value,
    _query: &serde_json::Value,
) -> Result<(String, String), String> {
    let path = path.trim_end_matches('/');

    match (method, path) {
        // --- Page interaction: POST /act ---
        ("POST", "/act") => {
            let kind = body.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            translate_act_kind(kind, body)
        }

        // --- Snapshot: GET /snapshot ---
        ("GET", "/snapshot") => {
            // playwright-cli snapshot outputs YAML; query params are not directly
            // supported as CLI flags, so we pass them through for potential future use.
            let params = serde_json::Map::new();
            Ok((
                "snapshot".to_string(),
                serde_json::to_string(&params).unwrap_or_else(|_| "{}".to_string()),
            ))
        }

        // --- Screenshot: POST /screenshot ---
        ("POST", "/screenshot") => {
            let mut params = serde_json::Map::new();
            if let Some(r) = body.get("ref").and_then(|v| v.as_str()) {
                params.insert("ref".into(), serde_json::Value::String(r.to_string()));
            }
            Ok((
                "screenshot".to_string(),
                serde_json::to_string(&params).unwrap_or_else(|_| "{}".to_string()),
            ))
        }

        // --- Navigate: POST /navigate ---
        ("POST", "/navigate") => {
            let url = body.get("url").and_then(|v| v.as_str()).unwrap_or("");
            Ok((
                "goto".to_string(),
                serde_json::json!({ "url": url }).to_string(),
            ))
        }

        // --- PDF: POST /pdf ---
        ("POST", "/pdf") => Ok(("pdf".to_string(), "{}".to_string())),

        // --- Tabs: GET /tabs ---
        ("GET", "/tabs") => Ok(("tab-list".to_string(), "{}".to_string())),

        // --- Open tab: POST /tabs/open ---
        ("POST", "/tabs/open") => {
            let url = body.get("url").and_then(|v| v.as_str()).unwrap_or("");
            Ok((
                "tab-new".to_string(),
                serde_json::json!({ "url": url }).to_string(),
            ))
        }

        // --- Close tab: DELETE /tabs/<targetId> ---
        // playwright-cli uses index-based tab-close, so we pass through
        // and BrowserManager will need to handle the ID-to-index mapping.
        ("DELETE", p) if p.starts_with("/tabs/") => Ok(("tab-close".to_string(), "{}".to_string())),

        // --- Start browser: POST /start ---
        ("POST", "/start") => {
            // playwright-cli `open` starts a browser; no separate start command.
            Ok(("open".to_string(), "{}".to_string()))
        }

        // --- Stop browser: POST /stop ---
        ("POST", "/stop") => Ok(("close".to_string(), "{}".to_string())),

        // --- Status: GET / ---
        ("GET", "" | "/") => Ok(("list".to_string(), "{}".to_string())),

        // --- Console: GET /console ---
        ("GET", "/console") => Ok(("console".to_string(), "{}".to_string())),

        // --- Download: POST /download ---
        ("POST", "/download") => {
            // Handled specially by BrowserManager::execute_download.
            let params_str = serde_json::to_string(body).unwrap_or_else(|_| "{}".to_string());
            Ok(("download".to_string(), params_str))
        }

        // --- Wait download: POST /wait/download ---
        ("POST", "/wait/download") => {
            let params_str = serde_json::to_string(body).unwrap_or_else(|_| "{}".to_string());
            Ok(("download".to_string(), params_str))
        }

        // --- File chooser (upload): POST /hooks/file-chooser ---
        ("POST", "/hooks/file-chooser") => {
            let params_str = serde_json::to_string(body).unwrap_or_else(|_| "{}".to_string());
            Ok(("upload".to_string(), params_str))
        }

        // --- Dialog: POST /hooks/dialog ---
        ("POST", "/hooks/dialog") => {
            let accept = body.get("accept").and_then(|v| v.as_bool()).unwrap_or(true);
            if accept {
                let mut params = serde_json::Map::new();
                if let Some(text) = body.get("promptText").and_then(|v| v.as_str()) {
                    params.insert(
                        "promptText".into(),
                        serde_json::Value::String(text.to_string()),
                    );
                }
                Ok((
                    "dialog-accept".to_string(),
                    serde_json::to_string(&params).unwrap_or_else(|_| "{}".to_string()),
                ))
            } else {
                Ok(("dialog-dismiss".to_string(), "{}".to_string()))
            }
        }

        // --- Focus tab: POST /tabs/focus ---
        ("POST", "/tabs/focus") => {
            let target_id = body.get("targetId").and_then(|v| v.as_str()).unwrap_or("");
            Ok((
                "tab-select".to_string(),
                serde_json::json!({ "index": target_id }).to_string(),
            ))
        }

        // --- Profiles: GET /profiles ---
        // playwright-cli has no profiles concept; return empty result.
        ("GET", "/profiles") => Ok(("list".to_string(), "{}".to_string())),

        _ => Err(format!(
            "unsupported browser proxy route: {} {}",
            method, path
        )),
    }
}

/// Translate a `POST /act` request with a specific `kind` into a playwright-cli action.
fn translate_act_kind(kind: &str, body: &serde_json::Value) -> Result<(String, String), String> {
    match kind {
        "click" => {
            let ref_val = body.get("ref").and_then(|v| v.as_str()).unwrap_or("");
            let params = serde_json::json!({ "ref": ref_val });
            Ok(("click".to_string(), params.to_string()))
        }
        "type" => {
            // playwright-cli `fill` sets the value directly (covers ~90% of cases).
            // For keystroke simulation, use click + type combo.
            let ref_val = body.get("ref").and_then(|v| v.as_str()).unwrap_or("");
            let text = body.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let submit = body
                .get("submit")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let mut params = serde_json::Map::new();
            params.insert("ref".into(), serde_json::Value::String(ref_val.to_string()));
            params.insert("text".into(), serde_json::Value::String(text.to_string()));
            if submit {
                params.insert("submit".into(), serde_json::Value::Bool(true));
            }
            Ok(("fill".to_string(), serde_json::to_string(&params).unwrap()))
        }
        "press" => {
            let key = body.get("key").and_then(|v| v.as_str()).unwrap_or("");
            let params = serde_json::json!({ "key": key });
            Ok(("press".to_string(), params.to_string()))
        }
        "hover" => {
            let ref_val = body.get("ref").and_then(|v| v.as_str()).unwrap_or("");
            let params = serde_json::json!({ "ref": ref_val });
            Ok(("hover".to_string(), params.to_string()))
        }
        "scrollIntoView" => {
            // Use eval with scrollIntoView to avoid hover CSS side-effects.
            let ref_val = body.get("ref").and_then(|v| v.as_str()).unwrap_or("");
            let params = serde_json::json!({
                "expression": "el => el.scrollIntoView({block:'center'})",
                "ref": ref_val,
            });
            Ok(("eval".to_string(), params.to_string()))
        }
        "select" => {
            let ref_val = body.get("ref").and_then(|v| v.as_str()).unwrap_or("");
            let value = body
                .get("values")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let params = serde_json::json!({ "ref": ref_val, "value": value });
            Ok(("select".to_string(), params.to_string()))
        }
        "fill" => {
            // OpenClaw sends { fields: [{ref, type, value}, ...] }.
            // Use the first field for playwright-cli `fill <ref> <text>`.
            let fields = body.get("fields").and_then(|v| v.as_array());
            if let Some(fields) = fields
                && let Some(first) = fields.first()
            {
                let ref_val = first.get("ref").and_then(|v| v.as_str()).unwrap_or("");
                let value = first.get("value").and_then(|v| v.as_str()).unwrap_or("");
                let params = serde_json::json!({ "ref": ref_val, "text": value });
                return Ok(("fill".to_string(), params.to_string()));
            }
            Ok(("fill".to_string(), "{}".to_string()))
        }
        "wait" => {
            // playwright-cli has no native wait; handled by BrowserManager::execute_wait_for_text.
            let params_str = serde_json::to_string(body).unwrap_or_else(|_| "{}".to_string());
            Ok(("wait".to_string(), params_str))
        }
        "evaluate" => {
            let expr = body.get("fn").and_then(|v| v.as_str()).unwrap_or("");
            let ref_val = body.get("ref").and_then(|v| v.as_str());
            let mut params = serde_json::json!({ "expression": expr });
            if let Some(r) = ref_val {
                params
                    .as_object_mut()
                    .unwrap()
                    .insert("ref".into(), serde_json::Value::String(r.to_string()));
            }
            Ok(("eval".to_string(), params.to_string()))
        }
        "close" => Ok(("close".to_string(), "{}".to_string())),
        "drag" => {
            let start_ref = body.get("startRef").and_then(|v| v.as_str()).unwrap_or("");
            let end_ref = body.get("endRef").and_then(|v| v.as_str()).unwrap_or("");
            let params = serde_json::json!({ "startRef": start_ref, "endRef": end_ref });
            Ok(("drag".to_string(), params.to_string()))
        }
        "resize" => {
            let width = body.get("width").and_then(|v| v.as_i64()).unwrap_or(1280);
            let height = body.get("height").and_then(|v| v.as_i64()).unwrap_or(720);
            let params = serde_json::json!({ "width": width, "height": height });
            Ok(("resize".to_string(), params.to_string()))
        }
        "" => Err("act kind is required".to_string()),
        other => {
            // Unknown kind — pass through, let playwright-cli handle it.
            let params_str = serde_json::to_string(body).unwrap_or_else(|_| "{}".to_string());
            Ok((other.to_string(), params_str))
        }
    }
}

/// Probe one directory for `bin`, considering PATHEXT on Windows.
///
/// On Unix: checks `dir/bin` existence only.
///
/// On Windows: first probes `dir/bin` as-is (handles the case where the
/// binary already has an extension); if that fails and `bin` has no
/// extension, probes `dir/bin{ext}` for each extension in the `PATHEXT`
/// environment variable (falling back to `.COM;.EXE;.BAT;.CMD`).
fn which_in_dir(dir: &std::path::Path, bin: &str) -> Option<std::path::PathBuf> {
    let candidate = dir.join(bin);
    if candidate.is_file() {
        return Some(candidate);
    }

    #[cfg(windows)]
    {
        // Only probe PATHEXT suffixes when the binary name has no extension.
        use std::path::Path;
        if Path::new(bin).extension().is_none() {
            let pathext =
                std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
            for ext in pathext.split(';') {
                let ext = ext.trim();
                if ext.is_empty() {
                    continue;
                }
                let with_ext = dir.join(format!("{bin}{ext}"));
                if with_ext.is_file() {
                    return Some(with_ext);
                }
            }
        }
    }

    None
}

/// Decode params from JSON string
fn decode_params<T: serde::de::DeserializeOwned>(
    params_json: &Option<String>,
) -> Result<T, InvokeError> {
    let json = params_json
        .as_ref()
        .ok_or_else(|| InvokeError::invalid_request("paramsJSON required"))?;

    serde_json::from_str(json)
        .map_err(|e| InvokeError::invalid_request(format!("invalid params: {}", e)))
}

/// Format command array as string
fn format_command(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| {
            let trimmed = arg.trim();
            if trimmed.is_empty() {
                "\"\"".to_string()
            } else if trimmed.contains(' ') || trimmed.contains('"') {
                format!("\"{}\"", trimmed.replace('"', "\\\""))
            } else {
                trimmed.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Truncate output to max characters
fn truncate_output(raw: &str, max_chars: usize) -> String {
    if raw.len() <= max_chars {
        raw.to_string()
    } else {
        format!("... (truncated) {}", &raw[raw.len() - max_chars..])
    }
}

/// Returns `true` iff `override_val` is a strict PATH-prepend of `base`:
/// the entries of `base` must appear as a contiguous suffix of the entries
/// in `override_val`.  Uses `std::env::split_paths` so the separator is
/// platform-correct (`;` on Windows, `:` on Unix).
///
/// Note: `std::env::split_paths("")` yields a single empty-string entry on
/// some platforms; we filter those out so that an empty-string input is
/// treated as "no entries".
///
/// Decision for empty `base`: a non-empty `override_val` is accepted as
/// "all entries are prepended to an empty base", mirroring the common case
/// where the daemon starts with `PATH=""` (e.g. in a stripped sandbox).
/// An empty `override_val` is rejected (nothing meaningful was prepended).
///
/// Windows note: case-variant base entries (e.g. `C:\Windows` vs `c:\windows`)
/// fail the suffix equality check and the override is dropped, so the daemon's
/// own PATH is used unchanged.  This is intentional fail-closed behaviour.
fn path_override_is_prepend_only(override_val: &str, base: &str) -> bool {
    // Filter out empty path entries produced by split_paths("").
    let non_empty = |p: &std::path::PathBuf| !p.as_os_str().is_empty();
    let override_entries: Vec<_> = std::env::split_paths(override_val)
        .filter(non_empty)
        .collect();
    let base_entries: Vec<_> = std::env::split_paths(base).filter(non_empty).collect();

    if override_entries.is_empty() {
        // An empty override adds nothing; reject.
        return false;
    }

    if base_entries.is_empty() {
        // Empty base: any non-empty override is acceptable — all entries are
        // "prepended" to an empty base.
        return true;
    }

    // The base entries must form a contiguous suffix of the override entries.
    let base_len = base_entries.len();
    let over_len = override_entries.len();
    if base_len > over_len {
        return false;
    }
    let suffix_start = over_len - base_len;
    override_entries[suffix_start..] == base_entries[..]
}

/// Sanitize environment variables
fn sanitize_env(overrides: &HashMap<String, String>) -> HashMap<String, String> {
    const BLOCKED_KEYS: &[&str] = &[
        "NODE_OPTIONS",
        "PYTHONHOME",
        "PYTHONPATH",
        "PERL5LIB",
        "PERL5OPT",
        "RUBYOPT",
        // Redirects the Python interpreter binary; a plugin could use it to
        // run an attacker-controlled python executable when the daemon spawns
        // python for a managed runtime.
        "PYTHONEXECUTABLE",
        // Auto-executes a script every time the Python interpreter starts;
        // allows arbitrary code injection into any python subprocess the
        // daemon spawns.
        "PYTHONSTARTUP",
        // Injects module-resolution directories into Node.js require(); a
        // plugin could use it to preload attacker-controlled code when the
        // daemon spawns node for a managed runtime.
        "NODE_PATH",
        // Auto-sourced by bash when invoked non-interactively (e.g. via $SHELL);
        // allows arbitrary code injection into any bash subprocess the daemon spawns.
        "BASH_ENV",
        // On Windows, cmd.exe consults PATHEXT to resolve the extension of a bare
        // command name; a cloud JobRequest could override it to make an approved bare
        // command resolve to an attacker-controlled script extension instead of .EXE.
        "PATHEXT",
        // On Windows, %COMSPEC% names the command interpreter; a child-env override
        // redirects shell spawns to an attacker-controlled binary when the daemon
        // invokes the shell by that variable rather than a hard-coded path.
        "COMSPEC",
    ];

    const BLOCKED_PREFIXES: &[&str] = &["DYLD_", "LD_"];

    let base_path = env::var("PATH").unwrap_or_default();
    let mut result: HashMap<String, String> = env::vars().collect();

    for (key, value) in overrides {
        let upper = key.to_uppercase();

        // Handle PATH specially
        if upper == "PATH" {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                continue;
            }
            // Only allow PATH overrides that strictly prepend to the current
            // PATH (platform-correct: split_paths uses `;` on Windows, `:` on
            // Unix).  Overrides that replace or reorder existing entries are
            // rejected to prevent DLL/binary hijacking.
            if path_override_is_prepend_only(trimmed, &base_path) {
                result.retain(|existing_key, _| {
                    !crate::plugin_runtime::path_env::is_path_env_key(existing_key)
                });
                result.insert("PATH".to_string(), value.clone());
            }
            continue;
        }

        // Block dangerous env vars
        if BLOCKED_KEYS.iter().any(|k| upper == *k) {
            continue;
        }

        if BLOCKED_PREFIXES.iter().any(|p| upper.starts_with(p)) {
            continue;
        }

        result.insert(key.clone(), value.clone());
    }

    result
}

#[cfg(test)]
mod tests {
    use super::{ExecEventKind, OpenClawHandler};
    use crate::approval::ApprovalManager;
    use crate::browser::BrowserManager;
    use crate::config::BrowserConfig;
    use crate::registry::JobRegistry;
    use crate::session::SessionManager;
    use ahand_protocol::{ApprovalResponse, SessionMode, envelope};
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::sync::broadcast;

    fn test_handler(
        approval_timeout_secs: u64,
    ) -> (
        OpenClawHandler,
        Arc<SessionManager>,
        Arc<ApprovalManager>,
        broadcast::Sender<ahand_protocol::Envelope>,
    ) {
        let session_mgr = Arc::new(SessionManager::new(5));
        let approval_mgr = Arc::new(ApprovalManager::new(approval_timeout_secs));
        let (approval_broadcast_tx, _) = broadcast::channel(8);
        let handler = OpenClawHandler::new(
            "device-test".to_string(),
            Arc::new(JobRegistry::new(1)),
            session_mgr.clone(),
            approval_mgr.clone(),
            approval_broadcast_tx.clone(),
            None,
            None,
            Arc::new(BrowserManager::new(BrowserConfig::default())),
        );
        (handler, session_mgr, approval_mgr, approval_broadcast_tx)
    }

    fn unique_output_path() -> PathBuf {
        std::env::temp_dir().join(format!("ahand-openclaw-{}", uuid::Uuid::new_v4()))
    }

    /// Returns a shell command string that creates a marker file at `path`.
    ///
    /// Unix:    `printf X > '<path>'` via sh  — single-quotes the path so names
    ///          with spaces are handled correctly.
    /// Windows: `echo X><path>` via COMSPEC (typically cmd.exe) — UNQUOTED path.
    ///          The whole command is passed to `cmd.exe /C` as a SINGLE arg by
    ///          `tokio::process::Command`, which serialises it with MSVC-CRT
    ///          quoting (`"` → `\"`).  cmd.exe does not understand `\"`, so a
    ///          quoted path would be mangled and the redirect would write to the
    ///          wrong target (see the M4 backlog note in the cross-platform spec).
    ///          `unique_output_path` builds the path under `temp_dir()`, which is
    ///          space-free on the CI runner (`C:\Users\runneradmin\AppData\Local\
    ///          Temp`), so an unquoted path is safe here.  No space before `>` so
    ///          cmd.exe doesn't include a trailing space in the redirect token.
    ///          Tests assert only `path.exists()`, not exact content, so the
    ///          trailing CRLF cmd adds is irrelevant.
    fn write_marker_command(path: &std::path::Path) -> String {
        #[cfg(unix)]
        {
            format!("printf X > '{}'", path.display())
        }
        #[cfg(windows)]
        {
            // Unquoted, no space before `>`.  See the doc comment above for why
            // quotes must NOT be used through `cmd.exe /C` from Rust's Command.
            format!("echo X>{}", path.display())
        }
        #[cfg(not(any(unix, windows)))]
        {
            format!("printf X > '{}'", path.display())
        }
    }

    fn system_run_invoke(
        session_key: &str,
        raw_command: String,
        approved: Option<bool>,
        approval_decision: Option<&str>,
    ) -> super::NodeInvokeRequest {
        let mut params = serde_json::Map::new();
        // rawCommand is set so use_direct=false; the shell drives execution and the
        // "command" array is ignored.  A non-empty placeholder is required because
        // handle_invoke rejects an empty command array before reaching the run path.
        params.insert("command".into(), json!(["<rawCommand>"]));
        params.insert("rawCommand".into(), json!(raw_command));
        params.insert("sessionKey".into(), json!(session_key));
        params.insert("runId".into(), json!("run-1"));
        if let Some(approved) = approved {
            params.insert("approved".into(), json!(approved));
        }
        if let Some(approval_decision) = approval_decision {
            params.insert("approvalDecision".into(), json!(approval_decision));
        }

        super::NodeInvokeRequest {
            id: "invoke-1".to_string(),
            node_id: "node-1".to_string(),
            command: "system.run".to_string(),
            params_json: Some(serde_json::Value::Object(params).to_string()),
            timeout_ms: Some(1_000),
            idempotency_key: None,
        }
    }

    fn payload_json(result: &super::NodeInvokeResult) -> serde_json::Value {
        serde_json::from_str(result.payload_json.as_deref().unwrap()).unwrap()
    }

    #[tokio::test]
    async fn inactive_session_denies_system_run_without_execution() {
        let (handler, _session_mgr, _approval_mgr, _broadcast_tx) = test_handler(1);
        let output_path = unique_output_path();
        // Command never executes (session inactive); write_marker_command keeps
        // the helper cross-platform-portable for consistency.
        let command = write_marker_command(&output_path);

        let (result, event) = handler
            .handle_invoke(system_run_invoke("session-1", command, None, None))
            .await;

        assert!(!output_path.exists());
        let payload = payload_json(&result);
        assert_eq!(payload["success"], false);
        assert_eq!(payload["error"], "session not activated");

        let event = event.unwrap();
        assert_eq!(event.kind, ExecEventKind::Denied);
        assert_eq!(
            event.payload.reason.as_deref(),
            Some("session not activated")
        );

        let _ = std::fs::remove_file(output_path);
    }

    // Executes a real shell job (sh -c on Unix; cmd /C on Windows) and asserts
    // the output file was created.  The command is built by `write_marker_command`
    // which selects the right syntax per platform.  `unique_output_path` uses
    // `std::env::temp_dir()` so the path is writable on both platforms.
    #[tokio::test]
    async fn strict_mode_preapproved_request_executes_without_local_wait() {
        let (handler, session_mgr, _approval_mgr, approval_broadcast_tx) = test_handler(1);
        session_mgr
            .set_mode("session-1", SessionMode::Strict, 0)
            .await;
        let mut approval_rx = approval_broadcast_tx.subscribe();
        let output_path = unique_output_path();
        let command = write_marker_command(&output_path);

        let (result, event) = handler
            .handle_invoke(system_run_invoke("session-1", command, Some(true), None))
            .await;

        assert!(output_path.exists());
        let payload = payload_json(&result);
        assert_eq!(payload["success"], true);

        let event = event.unwrap();
        assert_eq!(event.kind, ExecEventKind::Finished);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), approval_rx.recv())
                .await
                .is_err()
        );

        let _ = std::fs::remove_file(output_path);
    }

    // Executes a real shell job (sh -c on Unix; cmd /C on Windows) and asserts
    // the output file was created after approval.  The command is built by
    // `write_marker_command` which selects the right syntax per platform.
    #[tokio::test]
    async fn strict_mode_waits_for_local_approval_before_running() {
        let (handler, session_mgr, approval_mgr, approval_broadcast_tx) = test_handler(1);
        session_mgr
            .set_mode("session-1", SessionMode::Strict, 0)
            .await;
        let mut approval_rx = approval_broadcast_tx.subscribe();
        let output_path = unique_output_path();
        let command = write_marker_command(&output_path);

        let resolver = tokio::spawn(async move {
            let envelope = approval_rx.recv().await.unwrap();
            let request = match envelope.payload.unwrap() {
                envelope::Payload::ApprovalRequest(request) => request,
                other => panic!("unexpected payload: {other:?}"),
            };
            approval_mgr
                .resolve(&ApprovalResponse {
                    job_id: request.job_id,
                    approved: true,
                    remember: false,
                    reason: String::new(),
                })
                .await;
        });

        let (result, event) = handler
            .handle_invoke(system_run_invoke("session-1", command, None, None))
            .await;

        resolver.await.unwrap();
        assert!(output_path.exists());
        let payload = payload_json(&result);
        assert_eq!(payload["success"], true);
        assert_eq!(event.unwrap().kind, ExecEventKind::Finished);

        let _ = std::fs::remove_file(output_path);
    }

    #[tokio::test]
    async fn strict_mode_denial_broadcasts_request_and_does_not_execute() {
        let (handler, session_mgr, approval_mgr, approval_broadcast_tx) = test_handler(1);
        session_mgr
            .set_mode("session-1", SessionMode::Strict, 0)
            .await;
        let mut approval_rx = approval_broadcast_tx.subscribe();
        let output_path = unique_output_path();
        // Command is denied before execution; write_marker_command keeps the
        // helper cross-platform-portable and consistent with sibling tests.
        let command = write_marker_command(&output_path);

        let resolver = tokio::spawn(async move {
            let envelope = approval_rx.recv().await.unwrap();
            let request = match envelope.payload.unwrap() {
                envelope::Payload::ApprovalRequest(request) => request,
                other => panic!("unexpected payload: {other:?}"),
            };
            approval_mgr
                .resolve(&ApprovalResponse {
                    job_id: request.job_id,
                    approved: false,
                    remember: false,
                    reason: "operator rejected".to_string(),
                })
                .await;
        });

        let (result, event) = handler
            .handle_invoke(system_run_invoke("session-1", command, None, None))
            .await;

        resolver.await.unwrap();
        assert!(!output_path.exists());
        let payload = payload_json(&result);
        assert_eq!(payload["success"], false);
        assert_eq!(payload["error"], "approval denied: operator rejected");
        let event = event.unwrap();
        assert_eq!(event.kind, ExecEventKind::Denied);
        assert_eq!(
            event.payload.reason.as_deref(),
            Some("approval denied: operator rejected")
        );

        let _ = std::fs::remove_file(output_path);
    }

    #[tokio::test]
    async fn strict_mode_timeout_broadcasts_request_and_does_not_execute() {
        let (handler, session_mgr, _approval_mgr, approval_broadcast_tx) = test_handler(0);
        session_mgr
            .set_mode("session-1", SessionMode::Strict, 0)
            .await;
        let mut approval_rx = approval_broadcast_tx.subscribe();
        let output_path = unique_output_path();
        // Command times out before execution; write_marker_command keeps the
        // helper cross-platform-portable and consistent with sibling tests.
        let command = write_marker_command(&output_path);

        let (result, event) = handler
            .handle_invoke(system_run_invoke("session-1", command, None, None))
            .await;

        let envelope = approval_rx.recv().await.unwrap();
        match envelope.payload.unwrap() {
            envelope::Payload::ApprovalRequest(request) => {
                assert_eq!(request.caller_uid, "session-1");
            }
            other => panic!("unexpected payload: {other:?}"),
        }
        assert!(!output_path.exists());
        let payload = payload_json(&result);
        assert_eq!(payload["success"], false);
        assert_eq!(payload["error"], "approval timed out");
        let event = event.unwrap();
        assert_eq!(event.kind, ExecEventKind::Denied);
        assert_eq!(event.payload.reason.as_deref(), Some("approval timed out"));

        let _ = std::fs::remove_file(output_path);
    }

    #[tokio::test]
    async fn execution_failures_emit_finished_events_not_denied() {
        let (handler, session_mgr, _approval_mgr, _broadcast_tx) = test_handler(1);
        session_mgr
            .set_mode("session-1", SessionMode::AutoAccept, 0)
            .await;

        let (result, event) = handler
            .handle_invoke(system_run_invoke(
                "session-1",
                "exit 7".to_string(),
                None,
                None,
            ))
            .await;

        let payload = payload_json(&result);
        assert_eq!(payload["success"], false);
        assert_eq!(payload["exitCode"], 7);
        assert_eq!(event.unwrap().kind, ExecEventKind::Finished);
    }

    /// Build an invoke request that uses a true argv array (no rawCommand).
    /// Used to test the direct-spawn path. Its only caller is the
    /// unix-gated ampersand test, so gate it the same way.
    #[cfg(unix)]
    fn array_command_invoke(
        session_key: &str,
        argv: Vec<&str>,
        approved: Option<bool>,
    ) -> super::NodeInvokeRequest {
        let mut params = serde_json::Map::new();
        let cmd_json: serde_json::Value = argv.iter().map(|s| json!(s)).collect();
        params.insert("command".into(), cmd_json);
        params.insert("sessionKey".into(), json!(session_key));
        params.insert("runId".into(), json!("run-arr-1"));
        if let Some(approved) = approved {
            params.insert("approved".into(), json!(approved));
        }
        super::NodeInvokeRequest {
            id: "invoke-arr-1".to_string(),
            node_id: "node-1".to_string(),
            command: "system.run".to_string(),
            params_json: Some(serde_json::Value::Object(params).to_string()),
            timeout_ms: Some(5_000),
            idempotency_key: None,
        }
    }

    // ── which_in_dir tests ────────────────────────────────────────────────────

    #[test]
    fn which_in_dir_finds_exact_match() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_path = tmp.path().join("mybinary");
        std::fs::write(&bin_path, b"").unwrap();
        let result = super::which_in_dir(tmp.path(), "mybinary");
        assert_eq!(result, Some(bin_path));
    }

    #[test]
    fn which_in_dir_returns_none_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let result = super::which_in_dir(tmp.path(), "doesnotexist");
        assert!(result.is_none());
    }

    /// On Windows, a binary without an extension should be found when probing
    /// PATHEXT extensions. Gate to cfg(windows) since we need actual .exe files
    /// to exist and PATHEXT semantics are Windows-specific.
    #[cfg(windows)]
    #[test]
    fn which_in_dir_finds_pathext_extension_on_windows() {
        let tmp = tempfile::tempdir().unwrap();
        // Create "mybinary.exe" — PATHEXT typically contains .EXE
        let bin_path = tmp.path().join("mybinary.exe");
        std::fs::write(&bin_path, b"").unwrap();
        // Query without extension. PATHEXT entries are typically upper-case
        // (".EXE") and NTFS is case-insensitive, so the returned path may
        // carry the PATHEXT casing — compare case-insensitively.
        let result = super::which_in_dir(tmp.path(), "mybinary");
        let found = result.expect("should find mybinary.exe via PATHEXT");
        assert_eq!(
            found.to_string_lossy().to_lowercase(),
            bin_path.to_string_lossy().to_lowercase(),
            "should find mybinary.exe via PATHEXT (case-insensitive)"
        );
    }

    #[cfg(windows)]
    #[test]
    fn which_in_dir_already_has_extension_does_not_double_probe() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_path = tmp.path().join("mybinary.exe");
        std::fs::write(&bin_path, b"").unwrap();
        // Query WITH extension — should find via exact match, not PATHEXT loop.
        let result = super::which_in_dir(tmp.path(), "mybinary.exe");
        assert_eq!(result, Some(bin_path));
    }

    /// Array-form commands spawn directly (no shell), so an argument containing
    /// `&` is passed verbatim to the child process rather than interpreted by a
    /// shell.  We verify by running `echo` with an arg that contains `&foo` and
    /// asserting the output contains the literal ampersand — if it were passed
    /// to a shell the `&` would fork a background job instead.
    ///
    /// This test covers Unix only because `echo` semantics on Windows differ;
    /// the security property (no shell involved) applies on all platforms.
    #[cfg(unix)]
    #[tokio::test]
    async fn array_command_with_ampersand_does_not_spawn_second_command() {
        let (handler, session_mgr, _approval_mgr, _broadcast_tx) = test_handler(1);
        session_mgr
            .set_mode("session-1", SessionMode::AutoAccept, 0)
            .await;

        // The arg "hello&world" must arrive verbatim; a shell would interpret
        // `&` as a command separator and the output would just be "hello".
        let invoke = array_command_invoke("session-1", vec!["echo", "hello&world"], None);

        let (result, _event) = handler.handle_invoke(invoke).await;
        let payload = payload_json(&result);
        assert_eq!(
            payload["success"], true,
            "direct-spawn echo should succeed: {:?}",
            payload
        );
        let stdout = payload["stdout"].as_str().unwrap_or("").trim().to_string();
        assert_eq!(
            stdout, "hello&world",
            "ampersand must be passed verbatim, not shell-interpreted; got: {stdout:?}"
        );
    }

    // ── path_override_is_prepend_only tests ──────────────────────────────────

    /// Build a platform-correct PATH string from a list of directory strings
    /// using std::env::join_paths so the separator is always correct.
    fn join_path_entries(entries: &[&str]) -> String {
        let paths: Vec<std::path::PathBuf> = entries.iter().map(std::path::PathBuf::from).collect();
        std::env::join_paths(&paths)
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn path_override_prepend_single_entry_is_accepted() {
        let base = join_path_entries(&["/usr/bin", "/bin"]);
        let override_val = join_path_entries(&["/custom/bin", "/usr/bin", "/bin"]);
        assert!(
            super::path_override_is_prepend_only(&override_val, &base),
            "prepending one entry to base must be accepted"
        );
    }

    #[test]
    fn path_override_prepend_multiple_entries_is_accepted() {
        let base = join_path_entries(&["/usr/bin", "/bin"]);
        let override_val = join_path_entries(&["/a", "/b", "/usr/bin", "/bin"]);
        assert!(
            super::path_override_is_prepend_only(&override_val, &base),
            "prepending multiple entries to base must be accepted"
        );
    }

    #[test]
    fn path_override_identical_to_base_is_accepted() {
        let base = join_path_entries(&["/usr/bin", "/bin"]);
        // Same entries, zero prepended — still valid (nothing harmful added).
        assert!(
            super::path_override_is_prepend_only(&base, &base),
            "PATH identical to base must be accepted"
        );
    }

    #[test]
    fn path_override_full_replace_is_rejected() {
        let base = join_path_entries(&["/usr/bin", "/bin"]);
        let override_val = join_path_entries(&["/attacker/bin", "/other"]);
        assert!(
            !super::path_override_is_prepend_only(&override_val, &base),
            "full replacement of base entries must be rejected"
        );
    }

    #[test]
    fn path_override_middle_insert_is_rejected() {
        let base = join_path_entries(&["/usr/bin", "/bin"]);
        // Insert an entry between the two base entries — this is not a pure prepend.
        let override_val = join_path_entries(&["/usr/bin", "/injected", "/bin"]);
        assert!(
            !super::path_override_is_prepend_only(&override_val, &base),
            "inserting an entry in the middle of base entries must be rejected"
        );
    }

    #[test]
    fn path_override_reorder_is_rejected() {
        let base = join_path_entries(&["/usr/bin", "/bin"]);
        // Reversed order — base entries are present but not as a suffix.
        let override_val = join_path_entries(&["/bin", "/usr/bin"]);
        assert!(
            !super::path_override_is_prepend_only(&override_val, &base),
            "reordering base entries must be rejected"
        );
    }

    #[test]
    fn path_override_empty_override_is_rejected() {
        let base = join_path_entries(&["/usr/bin"]);
        assert!(
            !super::path_override_is_prepend_only("", &base),
            "empty override must be rejected"
        );
    }

    #[test]
    fn path_override_empty_base_non_empty_override_is_accepted() {
        // Empty base: any non-empty override is treated as "all prepended".
        let override_val = join_path_entries(&["/custom/bin"]);
        assert!(
            super::path_override_is_prepend_only(&override_val, ""),
            "non-empty override with empty base must be accepted"
        );
    }

    #[test]
    fn path_override_empty_base_empty_override_is_rejected() {
        assert!(
            !super::path_override_is_prepend_only("", ""),
            "both empty must be rejected (empty override adds nothing)"
        );
    }

    /// An override that is the base entries followed by an extra entry is an
    /// append, not a prepend — and must be rejected.  The base entries appear
    /// as a PREFIX of the override, not a suffix.
    #[test]
    fn path_override_append_is_rejected() {
        let base = join_path_entries(&["/usr/bin", "/bin"]);
        // base entries first, then an extra entry — this is an append, not a prepend.
        let override_val = join_path_entries(&["/usr/bin", "/bin", "/evil"]);
        assert!(
            !super::path_override_is_prepend_only(&override_val, &base),
            "appending an entry after base entries must be rejected (not a pure prepend)"
        );
    }

    // ── sanitize_env blocked-key tests ───────────────────────────────────────

    #[test]
    fn sanitize_env_strips_pythonexecutable() {
        // Assert the platform-robust invariant: a malicious override for a blocked
        // key never takes effect.  (`!contains_key` only happens to hold here
        // because PYTHONEXECUTABLE isn't in a normal base env; the override-never-
        // wins assertion is correct even if some host did set it.)
        let malicious = "/attacker/python";
        let mut overrides = std::collections::HashMap::new();
        overrides.insert("PYTHONEXECUTABLE".to_string(), malicious.to_string());
        overrides.insert("SAFE_VAR".to_string(), "hello".to_string());

        let result = super::sanitize_env(&overrides);

        assert_ne!(
            result.get("PYTHONEXECUTABLE").map(String::as_str),
            Some(malicious),
            "a blocked PYTHONEXECUTABLE override must never take effect"
        );
        assert_eq!(
            result.get("SAFE_VAR").map(String::as_str),
            Some("hello"),
            "non-blocked variables must pass through"
        );
    }

    #[test]
    fn sanitize_env_strips_pythonstartup() {
        let malicious = "/attacker/startup.py";
        let mut overrides = std::collections::HashMap::new();
        overrides.insert("PYTHONSTARTUP".to_string(), malicious.to_string());

        let result = super::sanitize_env(&overrides);

        assert_ne!(
            result.get("PYTHONSTARTUP").map(String::as_str),
            Some(malicious),
            "a blocked PYTHONSTARTUP override must never take effect"
        );
    }

    #[test]
    fn sanitize_env_strips_node_path() {
        let malicious = "/attacker/node_modules";
        let mut overrides = std::collections::HashMap::new();
        overrides.insert("NODE_PATH".to_string(), malicious.to_string());
        overrides.insert("SAFE_VAR".to_string(), "ok".to_string());

        let result = super::sanitize_env(&overrides);

        assert_ne!(
            result.get("NODE_PATH").map(String::as_str),
            Some(malicious),
            "a blocked NODE_PATH override must never take effect"
        );
        assert_eq!(
            result.get("SAFE_VAR").map(String::as_str),
            Some("ok"),
            "non-blocked variables must pass through"
        );
    }

    #[test]
    fn sanitize_env_strips_bash_env() {
        let malicious = "/attacker/inject.sh";
        let mut overrides = std::collections::HashMap::new();
        overrides.insert("BASH_ENV".to_string(), malicious.to_string());

        let result = super::sanitize_env(&overrides);

        assert_ne!(
            result.get("BASH_ENV").map(String::as_str),
            Some(malicious),
            "a blocked BASH_ENV override must never take effect"
        );
    }

    #[test]
    fn sanitize_env_strips_pathext() {
        // On Windows, cmd.exe uses PATHEXT to resolve a bare command's extension;
        // a cloud JobRequest could hide an env override that changes which
        // script/exe an approved bare command resolves to.  The real invariant is
        // that a malicious override never *takes effect*: `sanitize_env` seeds its
        // result from the daemon's own process env, so a blocked key already in the
        // base env (PATHEXT/COMSPEC are present on Windows) keeps the daemon's
        // value — but the override value must never win.  Asserting absence would
        // be wrong on Windows; asserting the override didn't take is correct on
        // every platform.
        let malicious = ".BAT;.CMD;.VBS";
        let mut overrides = std::collections::HashMap::new();
        overrides.insert("PATHEXT".to_string(), malicious.to_string());
        overrides.insert("SAFE_VAR".to_string(), "ok".to_string());

        let result = super::sanitize_env(&overrides);

        assert_ne!(
            result.get("PATHEXT").map(String::as_str),
            Some(malicious),
            "a blocked PATHEXT override must never take effect (base value is kept, \
             override is rejected)"
        );
        assert_eq!(
            result.get("SAFE_VAR").map(String::as_str),
            Some("ok"),
            "non-blocked variables must pass through"
        );
    }

    #[test]
    fn sanitize_env_strips_comspec() {
        // A child-env COMSPEC override redirects the command interpreter
        // %COMSPEC% resolves to, allowing an attacker-controlled binary to be
        // invoked as the shell.  On Windows COMSPEC is present in the base process
        // env, so the result keeps the daemon's value; the invariant we assert is
        // that the malicious override value never wins (platform-robust).
        let malicious = r"C:\attacker\evil.exe";
        let mut overrides = std::collections::HashMap::new();
        overrides.insert("COMSPEC".to_string(), malicious.to_string());
        overrides.insert("SAFE_VAR".to_string(), "ok".to_string());

        let result = super::sanitize_env(&overrides);

        assert_ne!(
            result.get("COMSPEC").map(String::as_str),
            Some(malicious),
            "a blocked COMSPEC override must never take effect (base value is kept, \
             override is rejected)"
        );
        assert_eq!(
            result.get("SAFE_VAR").map(String::as_str),
            Some("ok"),
            "non-blocked variables must pass through"
        );
    }

    #[test]
    fn sanitize_env_strips_existing_blocked_keys() {
        let blocked = &[
            "NODE_OPTIONS",
            "PYTHONHOME",
            "PYTHONPATH",
            "PERL5LIB",
            "PERL5OPT",
            "RUBYOPT",
        ];
        let malicious = "injected";
        let mut overrides = std::collections::HashMap::new();
        for key in blocked {
            overrides.insert((*key).to_string(), malicious.to_string());
        }

        let result = super::sanitize_env(&overrides);

        for key in blocked {
            assert_ne!(
                result.get(*key).map(String::as_str),
                Some(malicious),
                "a blocked {key} override must never take effect"
            );
        }
    }

    // ── sanitize_env BLOCKED_PREFIXES tests ──────────────────────────────────

    /// DYLD_INSERT_LIBRARIES and DYLD_LIBRARY_PATH are macOS loader-injection
    /// vectors; LD_PRELOAD is the Linux/ELF equivalent.  They are the ONLY
    /// loader-injection defence and must be blocked unconditionally on every
    /// platform (the blocklist is enforced by `BLOCKED_PREFIXES`, so the same
    /// code path runs on Windows too).
    #[test]
    fn sanitize_env_blocks_dyld_insert_libraries() {
        let malicious = "/attacker/evil.dylib";
        let mut overrides = std::collections::HashMap::new();
        overrides.insert("DYLD_INSERT_LIBRARIES".to_string(), malicious.to_string());
        overrides.insert("SAFE_VAR".to_string(), "ok".to_string());

        let result = super::sanitize_env(&overrides);

        assert_ne!(
            result.get("DYLD_INSERT_LIBRARIES").map(String::as_str),
            Some(malicious),
            "a blocked DYLD_INSERT_LIBRARIES override must never take effect"
        );
        assert_eq!(
            result.get("SAFE_VAR").map(String::as_str),
            Some("ok"),
            "non-blocked variables must pass through"
        );
    }

    #[test]
    fn sanitize_env_blocks_dyld_library_path() {
        let malicious = "/attacker/libs";
        let mut overrides = std::collections::HashMap::new();
        overrides.insert("DYLD_LIBRARY_PATH".to_string(), malicious.to_string());

        let result = super::sanitize_env(&overrides);

        assert_ne!(
            result.get("DYLD_LIBRARY_PATH").map(String::as_str),
            Some(malicious),
            "a blocked DYLD_LIBRARY_PATH override must never take effect"
        );
    }

    #[test]
    fn sanitize_env_blocks_ld_preload() {
        let malicious = "/attacker/inject.so";
        let mut overrides = std::collections::HashMap::new();
        overrides.insert("LD_PRELOAD".to_string(), malicious.to_string());

        let result = super::sanitize_env(&overrides);

        assert_ne!(
            result.get("LD_PRELOAD").map(String::as_str),
            Some(malicious),
            "a blocked LD_PRELOAD override must never take effect"
        );
    }

    // ── sanitize_env case-variant key bypass tests ────────────────────────────

    /// sanitize_env uppercases all keys before comparing against BLOCKED_KEYS
    /// and BLOCKED_PREFIXES.  A lowercase variant of a blocked key must not
    /// bypass the blocklist.
    #[test]
    fn sanitize_env_case_variant_blocked_keys() {
        // Exact-key blocks: lowercase variants of BLOCKED_KEYS entries must not
        // win (the uppercase-normalisation must apply before the check).
        let malicious = "injected_value";
        let mut overrides = std::collections::HashMap::new();
        overrides.insert("node_options".to_string(), malicious.to_string());
        overrides.insert("pythonpath".to_string(), malicious.to_string());
        overrides.insert("SAFE_CUSTOM".to_string(), "legit".to_string());

        let result = super::sanitize_env(&overrides);

        // Neither the lowercase key nor the uppercase form must hold the malicious value.
        assert_ne!(
            result.get("node_options").map(String::as_str),
            Some(malicious),
            "lowercase 'node_options' key must be treated as blocked NODE_OPTIONS"
        );
        assert_ne!(
            result.get("NODE_OPTIONS").map(String::as_str),
            Some(malicious),
            "blocked NODE_OPTIONS must not appear with attacker value under any casing"
        );
        assert_ne!(
            result.get("pythonpath").map(String::as_str),
            Some(malicious),
            "lowercase 'pythonpath' key must be treated as blocked PYTHONPATH"
        );
        assert_ne!(
            result.get("PYTHONPATH").map(String::as_str),
            Some(malicious),
            "blocked PYTHONPATH must not appear with attacker value under any casing"
        );
        assert_eq!(
            result.get("SAFE_CUSTOM").map(String::as_str),
            Some("legit"),
            "non-blocked variables must still pass through"
        );
    }

    /// A lowercase variant of a BLOCKED_PREFIX key must also be blocked.
    /// The prefix check runs on the uppercased key, so `dyld_insert_libraries`
    /// must be treated identically to `DYLD_INSERT_LIBRARIES`.
    #[test]
    fn sanitize_env_case_variant_blocked_prefix() {
        let malicious = "/attacker/evil.dylib";
        let mut overrides = std::collections::HashMap::new();
        overrides.insert("dyld_insert_libraries".to_string(), malicious.to_string());

        let result = super::sanitize_env(&overrides);

        assert_ne!(
            result.get("dyld_insert_libraries").map(String::as_str),
            Some(malicious),
            "lowercase 'dyld_insert_libraries' must be blocked by the DYLD_ prefix rule"
        );
        assert_ne!(
            result.get("DYLD_INSERT_LIBRARIES").map(String::as_str),
            Some(malicious),
            "the uppercase form must also not hold the malicious value"
        );
    }

    #[test]
    fn sanitize_env_path_prepend_is_accepted() {
        let base_path = std::env::var("PATH").unwrap_or_default();
        if base_path.is_empty() {
            // Can't meaningfully test PATH prepend without a base PATH.
            return;
        }
        let new_dir = join_path_entries(&["/prepended/bin"]);
        let prepended = format!(
            "{}",
            std::env::join_paths(
                std::iter::once(std::path::PathBuf::from("/prepended/bin"))
                    .chain(std::env::split_paths(&base_path))
            )
            .unwrap()
            .to_string_lossy()
        );

        let mut overrides = std::collections::HashMap::new();
        overrides.insert("PATH".to_string(), prepended.clone());

        let result = super::sanitize_env(&overrides);

        assert_eq!(
            result.get("PATH").map(String::as_str),
            Some(prepended.as_str()),
            "PATH prepend must be accepted; base_path={base_path:?} new_dir={new_dir:?}"
        );
    }

    #[test]
    fn sanitize_env_path_full_replace_is_rejected() {
        let base_path = std::env::var("PATH").unwrap_or_default();
        if base_path.is_empty() {
            return;
        }
        let replace = join_path_entries(&["/attacker/bin"]);

        let mut overrides = std::collections::HashMap::new();
        overrides.insert("PATH".to_string(), replace);

        let result = super::sanitize_env(&overrides);

        // The PATH in result must still be the original base (not the attacker's).
        assert_ne!(
            result.get("PATH").map(String::as_str),
            Some("/attacker/bin"),
            "full PATH replacement must be rejected"
        );
    }
}
