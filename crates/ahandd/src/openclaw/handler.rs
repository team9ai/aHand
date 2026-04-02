//! Command handler for OpenClaw node invocations.
//!
//! Dispatches incoming commands to appropriate handlers and maps results
//! to OpenClaw protocol responses.

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tracing::debug;

use crate::approval::ApprovalManager;
use crate::browser::BrowserManager;
use crate::registry::JobRegistry;
use crate::session::SessionManager;
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

/// Handler for OpenClaw node invocations
pub struct OpenClawHandler {
    node_id: String,
    registry: Arc<JobRegistry>,
    session_mgr: Arc<SessionManager>,
    approval_mgr: Arc<ApprovalManager>,
    store: Option<Arc<RunStore>>,
    exec_approvals_path: PathBuf,
    browser_mgr: Arc<BrowserManager>,
}

impl OpenClawHandler {
    pub fn new(
        node_id: String,
        registry: Arc<JobRegistry>,
        session_mgr: Arc<SessionManager>,
        approval_mgr: Arc<ApprovalManager>,
        store: Option<Arc<RunStore>>,
        exec_approvals_path: Option<PathBuf>,
        browser_mgr: Arc<BrowserManager>,
    ) -> Self {
        Self {
            node_id,
            registry,
            session_mgr,
            approval_mgr,
            store,
            exec_approvals_path: exec_approvals_path.unwrap_or_else(default_exec_approvals_path),
            browser_mgr,
        }
    }

    /// Handle a node.invoke.request
    pub async fn handle_invoke(
        &self,
        invoke: NodeInvokeRequest,
    ) -> (NodeInvokeResult, Option<ExecEventPayload>) {
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
    ) -> (NodeInvokeResult, Option<ExecEventPayload>) {
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
        let run_id = params
            .run_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let cmd_text = format_command(&params.command);

        // Check if approval is pre-granted
        let is_approved = params.approved == Some(true)
            || params.approval_decision == Some("allow-once".to_string())
            || params.approval_decision == Some("allow-always".to_string());

        // For now, execute directly (approval integration in Phase 5)
        // TODO: Integrate with SessionManager for approval flow

        let result = self.run_command(&params).await;

        let event = ExecEventPayload {
            session_key: session_key.clone(),
            run_id: run_id.clone(),
            host: "node".to_string(),
            command: Some(cmd_text),
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
            reason: None,
        };

        let invoke_result = NodeInvokeResult {
            id: invoke.id.clone(),
            node_id: self.node_id.clone(),
            ok: true,
            payload_json: Some(serde_json::to_string(&result).unwrap_or_default()),
            error: None,
        };

        (invoke_result, Some(event))
    }

    /// Execute a command and collect output
    async fn run_command(&self, params: &SystemRunParams) -> RunResult {
        let cwd = params.cwd.as_deref().filter(|s| !s.is_empty());
        let env_overrides = params.env.as_ref();
        let timeout_ms = params.timeout_ms.or(Some(120_000)); // default 2 minutes

        // Use raw_command with shell, or command array
        // If command array has 1 element, it's likely a full shell command string - use directly
        // If multiple elements, it's argv-style - escape and join
        let shell_cmd = params
            .raw_command
            .clone()
            .or_else(|| {
                if params.command.len() == 1 {
                    // Single element = full shell command, use directly
                    Some(params.command[0].clone())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| shell_escape_join(&params.command));

        debug!(shell_cmd = %shell_cmd, command_len = params.command.len(), "executing command via shell");

        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(&shell_cmd);

        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }

        // Apply environment overrides with sanitization
        if let Some(overrides) = env_overrides {
            let sanitized = sanitize_env(overrides);
            for (key, value) in sanitized {
                cmd.env(key, value);
            }
        }

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
        let timeout = timeout_ms.map(|ms| Duration::from_millis(ms));
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
        let path_env = env::var("PATH").unwrap_or_default();
        let path_dirs: Vec<&str> = path_env.split(':').collect();

        for bin in &params.bins {
            let bin = bin.trim();
            if bin.is_empty() || bin.contains('/') || bin.contains('\\') {
                continue;
            }

            for dir in &path_dirs {
                let candidate = Path::new(dir).join(bin);
                if candidate.exists() && candidate.is_file() {
                    found.insert(bin.to_string(), candidate.to_string_lossy().to_string());
                    break;
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
                if submit {
                    if let Ok(ref r) = result {
                        if r.success {
                            let press_params = serde_json::json!({ "key": "Enter" });
                            let _ = self
                                .browser_mgr
                                .execute(
                                    &session_id,
                                    "press",
                                    &press_params.to_string(),
                                    timeout_ms,
                                )
                                .await;
                        }
                    }
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
            if let Some(fields) = fields {
                if let Some(first) = fields.first() {
                    let ref_val = first.get("ref").and_then(|v| v.as_str()).unwrap_or("");
                    let value = first.get("value").and_then(|v| v.as_str()).unwrap_or("");
                    let params = serde_json::json!({ "ref": ref_val, "text": value });
                    return Ok(("fill".to_string(), params.to_string()));
                }
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

/// Join command array into shell-safe string
fn shell_escape_join(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| {
            // If arg contains shell special chars, quote it
            if arg.chars().any(|c| {
                matches!(
                    c,
                    ' ' | '"'
                        | '\''
                        | '\\'
                        | '$'
                        | '`'
                        | '!'
                        | '*'
                        | '?'
                        | '['
                        | ']'
                        | '('
                        | ')'
                        | '{'
                        | '}'
                        | '|'
                        | '&'
                        | ';'
                        | '<'
                        | '>'
                        | '\n'
                        | '\t'
                )
            }) {
                // Use single quotes and escape any single quotes inside
                format!("'{}'", arg.replace('\'', "'\\''"))
            } else if arg.is_empty() {
                "''".to_string()
            } else {
                arg.clone()
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

/// Sanitize environment variables
fn sanitize_env(overrides: &HashMap<String, String>) -> HashMap<String, String> {
    const BLOCKED_KEYS: &[&str] = &[
        "NODE_OPTIONS",
        "PYTHONHOME",
        "PYTHONPATH",
        "PERL5LIB",
        "PERL5OPT",
        "RUBYOPT",
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
            // Only allow PATH if it prepends to current PATH
            if trimmed == base_path || trimmed.ends_with(&format!(":{}", base_path)) {
                result.insert(key.clone(), value.clone());
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
