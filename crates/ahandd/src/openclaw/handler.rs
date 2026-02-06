//! Command handler for OpenClaw node invocations.
//!
//! Dispatches incoming commands to appropriate handlers and maps results
//! to OpenClaw protocol responses.

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    NodeInvokeRequest, NodeInvokeResult, RunResult, SystemRunParams, SystemWhichParams,
    SystemWhichResult, OUTPUT_CAP, OUTPUT_EVENT_TAIL,
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
            exec_approvals_path: exec_approvals_path
                .unwrap_or_else(default_exec_approvals_path),
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
                &[&result.stdout, &result.stderr, result.error.as_deref().unwrap_or("")]
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

    /// Handle browser.proxy command
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
        struct BrowserProxyParams {
            session_id: String,
            action: String,
            #[serde(default)]
            params_json: Option<String>,
            #[serde(default)]
            timeout_ms: Option<u64>,
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

        let params_json = params.params_json.as_deref().unwrap_or("{}");

        // Check domain restrictions for navigation actions.
        if let Err(msg) = self.browser_mgr.check_domain(&params.action, params_json) {
            return NodeInvokeResult {
                id: invoke.id.clone(),
                node_id: self.node_id.clone(),
                ok: false,
                payload_json: None,
                error: Some(InvokeError::new("PERMISSION_DENIED", msg)),
            };
        }

        let timeout_ms = params.timeout_ms.unwrap_or(0);

        match self
            .browser_mgr
            .execute(&params.session_id, &params.action, params_json, timeout_ms)
            .await
        {
            Ok(result) => {
                // Release session on "close" action.
                if params.action == "close" {
                    self.browser_mgr.release_session(&params.session_id).await;
                }

                #[derive(serde::Serialize)]
                struct BrowserProxyResult {
                    success: bool,
                    #[serde(skip_serializing_if = "String::is_empty")]
                    result_json: String,
                    #[serde(skip_serializing_if = "String::is_empty")]
                    error: String,
                    #[serde(
                        skip_serializing_if = "Vec::is_empty",
                        serialize_with = "serialize_base64"
                    )]
                    binary_data: Vec<u8>,
                    #[serde(skip_serializing_if = "String::is_empty")]
                    binary_mime: String,
                }

                fn serialize_base64<S: serde::Serializer>(
                    data: &Vec<u8>,
                    s: S,
                ) -> Result<S::Ok, S::Error> {
                    use base64::Engine;
                    s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(data))
                }

                let proxy_result = BrowserProxyResult {
                    success: result.success,
                    result_json: result.result_json,
                    error: result.error,
                    binary_data: result.binary_data,
                    binary_mime: result.binary_mime,
                };

                NodeInvokeResult {
                    id: invoke.id.clone(),
                    node_id: self.node_id.clone(),
                    ok: true,
                    payload_json: Some(
                        serde_json::to_string(&proxy_result).unwrap_or_default(),
                    ),
                    error: None,
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
            if arg.chars().any(|c| matches!(c, ' ' | '"' | '\'' | '\\' | '$' | '`' | '!' | '*' | '?' | '[' | ']' | '(' | ')' | '{' | '}' | '|' | '&' | ';' | '<' | '>' | '\n' | '\t')) {
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
