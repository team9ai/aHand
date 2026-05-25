use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ahand_protocol::{Envelope, JobEvent, JobFinished, JobRequest, envelope, job_event};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::executor::EnvelopeSink;
use crate::store::RunStore;

use super::mcp_config::{
    MCP_CONFIG_ENV, MCP_CONFIG_MODE_ENV, McpConfig, McpConfigMode, server_names,
};

const INPUT_FORMAT_ENV: &str = "AHAND_INPUT_FORMAT";
const OUTPUT_FORMAT_ENV: &str = "AHAND_OUTPUT_FORMAT";
const EXECUTABLE_ENV: &str = "AHAND_AGENT_EXECUTABLE";
const PROMPT_ENV: &str = "AHAND_AGENT_PROMPT";
const MODEL_ENV: &str = "AHAND_AGENT_MODEL";
const SESSION_ENV: &str = "AHAND_AGENT_SESSION_ID";
const INSTRUCTIONS_ENV: &str = "AHAND_AGENT_INSTRUCTIONS";

pub fn is_hermes_acp_job(req: &JobRequest) -> bool {
    ahand_protocol::resolve_job_input_format(req)
        == ahand_protocol::INPUT_FORMAT_HERMES_ACP_JSON_RPC
        && ahand_protocol::resolve_job_output_format(req)
            == ahand_protocol::OUTPUT_FORMAT_HERMES_ACP_JSON_RPC
        || req
            .env
            .get(INPUT_FORMAT_ENV)
            .is_some_and(|value| value == ahand_protocol::INPUT_FORMAT_HERMES_ACP_JSON_RPC)
            && req
                .env
                .get(OUTPUT_FORMAT_ENV)
                .is_some_and(|value| value == ahand_protocol::OUTPUT_FORMAT_HERMES_ACP_JSON_RPC)
}

#[derive(Debug, Clone)]
struct HermesAcpConfig {
    executable: String,
    cwd: String,
    env: HashMap<String, String>,
    prompt: String,
    model: Option<String>,
    session_id: Option<String>,
    instructions: Option<String>,
    mcp_config: Option<McpConfig>,
}

impl HermesAcpConfig {
    fn from_job(req: &JobRequest) -> Result<Self, String> {
        let executable = req
            .env
            .get(EXECUTABLE_ENV)
            .filter(|value| !value.trim().is_empty())
            .cloned()
            .or_else(|| (!req.tool.trim().is_empty()).then(|| req.tool.clone()))
            .ok_or_else(|| {
                "hermes-acp requires AHAND_AGENT_EXECUTABLE or JobRequest.tool".to_string()
            })?;

        let prompt = req
            .env
            .get(PROMPT_ENV)
            .filter(|value| !value.trim().is_empty())
            .cloned()
            .ok_or_else(|| "hermes-acp requires AHAND_AGENT_PROMPT".to_string())?;

        Ok(Self {
            executable,
            cwd: req.cwd.clone(),
            env: req.env.clone(),
            prompt,
            model: req
                .env
                .get(MODEL_ENV)
                .filter(|value| !value.trim().is_empty())
                .cloned(),
            session_id: req
                .env
                .get(SESSION_ENV)
                .filter(|value| !value.trim().is_empty())
                .cloned(),
            instructions: req
                .env
                .get(INSTRUCTIONS_ENV)
                .filter(|value| !value.trim().is_empty())
                .cloned(),
            mcp_config: McpConfig::from_env(
                req.env.get(MCP_CONFIG_ENV).map(String::as_str),
                req.env.get(MCP_CONFIG_MODE_ENV).map(String::as_str),
            )?,
        })
    }
}

pub async fn run_hermes_acp<T>(
    device_id: String,
    req: JobRequest,
    tx: T,
    mut cancel_rx: mpsc::Receiver<()>,
    store: Option<Arc<RunStore>>,
) -> (i32, String)
where
    T: EnvelopeSink,
{
    let job_id = req.job_id.clone();
    info!(job_id = %job_id, "starting hermes ACP job");

    if let Some(s) = &store {
        s.start_run(&job_id, &req);
    }

    let config = match HermesAcpConfig::from_job(&req) {
        Ok(config) => config,
        Err(error) => return finish(&device_id, &job_id, -1, &error, &tx, &store),
    };

    if let Err(error) = prepare_context(&config, &job_id, &store) {
        return finish(&device_id, &job_id, -1, &error, &tx, &store);
    }

    let mut child = match spawn_hermes(&config).await {
        Ok(child) => child,
        Err(error) => return finish(&device_id, &job_id, -1, &error, &tx, &store),
    };

    let Some(stdin) = child.stdin.take() else {
        let _ = child.kill().await;
        return finish(
            &device_id,
            &job_id,
            -1,
            "failed to open Hermes stdin",
            &tx,
            &store,
        );
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill().await;
        return finish(
            &device_id,
            &job_id,
            -1,
            "failed to open Hermes stdout",
            &tx,
            &store,
        );
    };

    let stderr = child.stderr.take();
    let (frame_tx, frame_rx) = mpsc::unbounded_channel::<Value>();
    let stdout_store = store.clone();
    let stdout_job_id = job_id.clone();
    let stdout_handle = tokio::spawn(async move {
        read_stdout_frames(stdout, frame_tx, stdout_store, stdout_job_id).await;
    });

    let stderr_tx = tx.clone();
    let stderr_store = store.clone();
    let stderr_device_id = device_id.clone();
    let stderr_job_id = job_id.clone();
    let provider_error = Arc::new(Mutex::new(None::<ProviderError>));
    let stderr_provider_error = provider_error.clone();
    let stderr_handle = tokio::spawn(async move {
        if let Some(stderr) = stderr {
            read_stderr(
                stderr,
                stderr_tx,
                stderr_store,
                stderr_device_id,
                stderr_job_id,
                stderr_provider_error,
            )
            .await;
        }
    });

    let timeout = (req.timeout_ms > 0).then(|| std::time::Duration::from_millis(req.timeout_ms));
    let sequence = run_sequence(
        stdin,
        frame_rx,
        HermesAcpFormatter::new(&req, &config),
        &device_id,
        &job_id,
        &tx,
        &store,
        &config,
    );

    let result = match timeout {
        Some(timeout) => {
            tokio::select! {
                _ = cancel_rx.recv() => Err("cancelled".to_string()),
                result = tokio::time::timeout(timeout, sequence) => {
                    match result {
                        Ok(result) => result,
                        Err(_) => Err("timeout".to_string()),
                    }
                }
            }
        }
        None => {
            tokio::select! {
                _ = cancel_rx.recv() => Err("cancelled".to_string()),
                result = sequence => result,
            }
        }
    };

    let mut formatter = match result {
        Ok(formatter) => formatter,
        Err(error) => {
            let error = provider_error
                .lock()
                .ok()
                .and_then(|guard| guard.clone())
                .map(|provider_error| format!("{error}; {}", provider_error.summary()))
                .unwrap_or(error);
            warn!(job_id = %job_id, error = %error, "Hermes ACP job failed");
            let _ = child.kill().await;
            stdout_handle.abort();
            stderr_handle.abort();
            return finish(&device_id, &job_id, -1, &error, &tx, &store);
        }
    };

    if let Some(provider_error) = provider_error.lock().ok().and_then(|guard| guard.clone()) {
        emit_records(
            &device_id,
            &job_id,
            &tx,
            &store,
            formatter.format_provider_error(&provider_error),
        );
        let _ = child.kill().await;
        stdout_handle.abort();
        stderr_handle.abort();
        return finish(
            &device_id,
            &job_id,
            -1,
            &provider_error.summary(),
            &tx,
            &store,
        );
    }

    let _ = child.kill().await;
    let _ = stdout_handle.await;
    let _ = stderr_handle.await;
    finish(&device_id, &job_id, 0, "", &tx, &store)
}

async fn spawn_hermes(config: &HermesAcpConfig) -> Result<tokio::process::Child, String> {
    let mut cmd = Command::new(&config.executable);
    cmd.arg("acp")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    if !config.cwd.is_empty() {
        cmd.current_dir(&config.cwd);
    }
    for (key, value) in &config.env {
        if key == MCP_CONFIG_ENV || key == MCP_CONFIG_MODE_ENV {
            continue;
        }
        cmd.env(key, value);
    }

    cmd.spawn()
        .map_err(|error| format!("failed to spawn Hermes ACP: {error}"))
}

fn prepare_context(
    config: &HermesAcpConfig,
    job_id: &str,
    store: &Option<Arc<RunStore>>,
) -> Result<(), String> {
    let Some(instructions) = &config.instructions else {
        return Ok(());
    };
    if config.cwd.trim().is_empty() {
        return Err(format!("{INSTRUCTIONS_ENV} requires a non-empty cwd"));
    }

    let cwd = std::path::Path::new(&config.cwd);
    let primary = cwd.join("AGENTS.md");
    let path = if primary.exists() {
        cwd.join("AGENTS.ahand.md")
    } else {
        primary
    };
    if path.exists() {
        return Err(format!(
            "refusing to overwrite existing context file {}",
            path.display()
        ));
    }
    std::fs::write(&path, instructions)
        .map_err(|error| format!("failed to write Hermes context {}: {error}", path.display()))?;

    append_json_line(
        store,
        job_id,
        "context.jsonl",
        &json!({
            "kind": "hermes_context_file",
            "path": path.display().to_string(),
            "source": INSTRUCTIONS_ENV,
        }),
    );
    Ok(())
}

fn hermes_mcp_servers(
    config: &HermesAcpConfig,
    job_id: &str,
    store: &Option<Arc<RunStore>>,
) -> Result<Vec<Value>, String> {
    let Some(mcp_config) = &config.mcp_config else {
        return Ok(Vec::new());
    };
    let servers = mcp_config.hermes_servers()?;
    let names = server_names(&mcp_config.value);
    let server_count = names.len();
    append_json_line(
        store,
        job_id,
        "mcp.jsonl",
        &json!({
            "kind": "mcp_config_injected",
            "agent": "hermes-acp",
            "mode": match mcp_config.mode {
                McpConfigMode::Merge => "merge",
                McpConfigMode::Replace => "replace",
            },
            "serverNames": names,
            "serverCount": server_count,
            "target": "session/new.mcpServers",
        }),
    );
    Ok(servers)
}

async fn run_sequence<T>(
    stdin: tokio::process::ChildStdin,
    mut frame_rx: mpsc::UnboundedReceiver<Value>,
    mut formatter: HermesAcpFormatter,
    device_id: &str,
    job_id: &str,
    tx: &T,
    store: &Option<Arc<RunStore>>,
    config: &HermesAcpConfig,
) -> Result<HermesAcpFormatter, String>
where
    T: EnvelopeSink,
{
    let mut client = HermesRpcClient { stdin, next_id: 1 };

    let initialize = client
        .request(
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientInfo": {
                    "name": "ahandd",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "clientCapabilities": {},
            }),
            &mut frame_rx,
            &mut formatter,
            &EmitContext {
                device_id,
                job_id,
                tx,
                store,
            },
        )
        .await?;
    emit_records(
        device_id,
        job_id,
        tx,
        store,
        formatter.format_initialize(&initialize),
    );

    let session = if let Some(session_id) = &config.session_id {
        if config.mcp_config.is_some() {
            return Err("mcpConfig is only supported for new Hermes sessions".to_string());
        }
        client
            .request(
                "session/resume",
                json!({
                    "cwd": config.cwd,
                    "sessionId": session_id,
                }),
                &mut frame_rx,
                &mut formatter,
                &EmitContext {
                    device_id,
                    job_id,
                    tx,
                    store,
                },
            )
            .await?
    } else {
        let mcp_servers = hermes_mcp_servers(config, job_id, store)?;
        let mut params = json!({
            "cwd": config.cwd,
            "mcpServers": mcp_servers,
        });
        if let Some(model) = &config.model {
            params["model"] = json!(model);
        }
        client
            .request(
                "session/new",
                params,
                &mut frame_rx,
                &mut formatter,
                &EmitContext {
                    device_id,
                    job_id,
                    tx,
                    store,
                },
            )
            .await?
    };
    formatter.capture_session(&session);
    if let Some(store) = store {
        store.write_json_artifact(
            job_id,
            "hermes-session.json",
            &json!({
                "sessionId": formatter.session_id(),
                "model": config.model.as_deref(),
                "raw": session,
            }),
        );
    }
    emit_records(
        device_id,
        job_id,
        tx,
        store,
        formatter.format_session(&session),
    );

    if let Some(model) = &config.model {
        let model_result = client
            .request(
                "session/set_model",
                json!({
                    "sessionId": formatter.session_id(),
                    "modelId": model,
                }),
                &mut frame_rx,
                &mut formatter,
                &EmitContext {
                    device_id,
                    job_id,
                    tx,
                    store,
                },
            )
            .await?;
        emit_records(
            device_id,
            job_id,
            tx,
            store,
            formatter.format_status("model_set", &model_result),
        );
    }

    let prompt_result = client
        .request(
            "session/prompt",
            json!({
                "sessionId": formatter.session_id(),
                "prompt": [
                    {
                        "type": "text",
                        "text": config.prompt,
                    }
                ],
            }),
            &mut frame_rx,
            &mut formatter,
            &EmitContext {
                device_id,
                job_id,
                tx,
                store,
            },
        )
        .await?;
    emit_records(
        device_id,
        job_id,
        tx,
        store,
        formatter.format_prompt_result(&prompt_result),
    );
    Ok(formatter)
}

struct HermesRpcClient {
    stdin: tokio::process::ChildStdin,
    next_id: u64,
}

struct EmitContext<'a, T> {
    device_id: &'a str,
    job_id: &'a str,
    tx: &'a T,
    store: &'a Option<Arc<RunStore>>,
}

impl HermesRpcClient {
    async fn request<T>(
        &mut self,
        method: &str,
        params: Value,
        frame_rx: &mut mpsc::UnboundedReceiver<Value>,
        formatter: &mut HermesAcpFormatter,
        emit: &EmitContext<'_, T>,
    ) -> Result<Value, String>
    where
        T: EnvelopeSink,
    {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        append_json_line(emit.store, emit.job_id, "acp-requests.jsonl", &request);
        let mut line = serde_json::to_vec(&request)
            .map_err(|error| format!("failed to encode JSON-RPC request: {error}"))?;
        line.push(b'\n');
        self.stdin
            .write_all(&line)
            .await
            .map_err(|error| format!("failed to write Hermes request: {error}"))?;
        self.stdin
            .flush()
            .await
            .map_err(|error| format!("failed to flush Hermes request: {error}"))?;

        while let Some(frame) = frame_rx.recv().await {
            if frame.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(error) = frame.get("error") {
                    return Err(format!("Hermes {method} failed: {error}"));
                }
                return Ok(frame.get("result").cloned().unwrap_or(Value::Null));
            }
            if self.handle_peer_request(&frame, formatter, emit).await? {
                continue;
            }
            emit_records(
                emit.device_id,
                emit.job_id,
                emit.tx,
                emit.store,
                formatter.format_event(frame),
            );
        }

        Err(format!("Hermes exited before {method} response"))
    }

    async fn handle_peer_request(
        &mut self,
        frame: &Value,
        formatter: &mut HermesAcpFormatter,
        emit: &EmitContext<'_, impl EnvelopeSink>,
    ) -> Result<bool, String> {
        let Some(id) = frame.get("id").and_then(Value::as_u64) else {
            return Ok(false);
        };
        let Some(method) = frame.get("method").and_then(Value::as_str) else {
            return Ok(false);
        };

        let response = if method == "session/request_permission" {
            let option_id =
                select_permission_option_id(frame).unwrap_or_else(|| "approve_for_session".into());
            emit_records(
                emit.device_id,
                emit.job_id,
                emit.tx,
                emit.store,
                formatter.format_permission_request(frame, &option_id),
            );
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "outcome": {
                        "outcome": "selected",
                        "optionId": option_id,
                    }
                }
            })
        } else {
            emit_records(
                emit.device_id,
                emit.job_id,
                emit.tx,
                emit.store,
                formatter.format_policy_decision(
                    frame,
                    "deny",
                    &format!("method not found: {method}"),
                ),
            );
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("method not found: {method}"),
                }
            })
        };

        append_json_line(emit.store, emit.job_id, "acp-requests.jsonl", &response);
        let mut line = serde_json::to_vec(&response)
            .map_err(|error| format!("failed to encode JSON-RPC response: {error}"))?;
        line.push(b'\n');
        self.stdin
            .write_all(&line)
            .await
            .map_err(|error| format!("failed to write Hermes response: {error}"))?;
        self.stdin
            .flush()
            .await
            .map_err(|error| format!("failed to flush Hermes response: {error}"))?;
        Ok(true)
    }
}

async fn read_stdout_frames(
    stdout: tokio::process::ChildStdout,
    frame_tx: mpsc::UnboundedSender<Value>,
    store: Option<Arc<RunStore>>,
    job_id: String,
) {
    let mut lines = BufReader::new(stdout).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if let Some(store) = &store {
                    let mut raw = line.as_bytes().to_vec();
                    raw.push(b'\n');
                    store.append_artifact(&job_id, "acp-events.jsonl", &raw);
                }
                match serde_json::from_str::<Value>(&line) {
                    Ok(value) => {
                        let _ = frame_tx.send(value);
                    }
                    Err(error) => {
                        let value = json!({
                            "ahand_parse_error": error.to_string(),
                            "line": line,
                        });
                        let _ = frame_tx.send(value);
                    }
                }
            }
            Ok(None) => break,
            Err(error) => {
                warn!(job_id = %job_id, error = %error, "failed to read Hermes stdout");
                break;
            }
        }
    }
}

async fn read_stderr<T>(
    stderr: tokio::process::ChildStderr,
    tx: T,
    store: Option<Arc<RunStore>>,
    device_id: String,
    job_id: String,
    provider_error: Arc<Mutex<Option<ProviderError>>>,
) where
    T: EnvelopeSink,
{
    let mut reader = tokio::io::BufReader::new(stderr);
    let mut buf = vec![0u8; 4096];
    loop {
        match tokio::io::AsyncReadExt::read(&mut reader, &mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                let chunk = &buf[..n];
                if let Some(store) = &store {
                    store.append_stderr(&job_id, chunk);
                }
                if let Ok(text) = std::str::from_utf8(chunk)
                    && let Some(error) = detect_provider_error(text)
                    && let Ok(mut slot) = provider_error.lock()
                    && slot.is_none()
                {
                    *slot = Some(error);
                }
                let _ = tx.send(make_event_envelope(
                    &device_id,
                    &job_id,
                    None,
                    Some(chunk.to_vec()),
                ));
            }
            Err(error) => {
                warn!(job_id = %job_id, error = %error, "failed to read Hermes stderr");
                break;
            }
        }
    }
}

#[derive(Debug, Clone)]
struct ProviderError {
    code: String,
    message: String,
}

impl ProviderError {
    fn summary(&self) -> String {
        format!("Hermes provider error [{}]: {}", self.code, self.message)
    }
}

fn detect_provider_error(text: &str) -> Option<ProviderError> {
    let lower = text.to_ascii_lowercase();
    if is_transient_provider_retry_warning(&lower) {
        return None;
    }
    if lower.contains("tool ") && lower.contains(" returned error") {
        return None;
    }

    let looks_error = lower.contains("error")
        || lower.contains("failed")
        || lower.contains("exception")
        || lower.contains("rate limit")
        || lower.contains("quota")
        || lower.contains("unauthorized")
        || lower.contains("authentication")
        || lower.contains("invalid api key");
    if !looks_error {
        return None;
    }

    let code = if lower.contains("rate limit")
        || lower.contains("http 429")
        || lower.contains("status 429")
        || lower.contains("code 429")
    {
        "provider_rate_limited"
    } else if lower.contains("http 402")
        || lower.contains("status 402")
        || lower.contains("code 402")
        || lower.contains("insufficient_quota")
        || lower.contains("quota")
        || lower.contains("billing")
        || lower.contains("credits")
    {
        "provider_quota_exceeded"
    } else if lower.contains("unauthorized")
        || lower.contains("authentication")
        || lower.contains("invalid api key")
        || lower.contains("401")
    {
        "provider_auth_failed"
    } else if lower.contains("provider") && lower.contains("error") {
        "provider_error"
    } else {
        return None;
    };

    Some(ProviderError {
        code: code.to_string(),
        message: text.trim().to_string(),
    })
}

fn is_transient_provider_retry_warning(lower: &str) -> bool {
    let stream_retry = lower.contains("stream drop")
        || lower.contains("readtimeout")
        || lower.contains("read operation timed out");
    if !stream_retry {
        return false;
    }

    let retry_in_progress = lower.contains("retrying")
        || lower.contains("reconnecting")
        || lower.contains("attempt 2/")
        || lower.contains("attempt 3/");
    let retry_exhausted = lower.contains("failed after")
        || lower.contains("exhausted")
        || lower.contains("giving up");

    retry_in_progress && !retry_exhausted
}

struct HermesAcpFormatter {
    job_id: String,
    agent_id: String,
    session_id: Option<String>,
    model: Option<String>,
    cwd: String,
    executable: String,
    seq: u64,
    source_seq: u64,
    pending_llm: Option<PendingLlmMessage>,
    tool_calls: HashMap<String, ToolCallState>,
}

#[derive(Debug)]
struct PendingLlmMessage {
    channel: String,
    text: String,
    start_seq: u64,
    end_seq: u64,
    chunk_count: u64,
    started_at_ms: u64,
    ended_at_ms: u64,
    source_kind: String,
}

#[derive(Debug, Clone)]
struct ToolCallState {
    tool_name: String,
    tool_kind: String,
    input: Value,
}

impl HermesAcpFormatter {
    fn new(req: &JobRequest, config: &HermesAcpConfig) -> Self {
        Self {
            job_id: req.job_id.clone(),
            agent_id: format!("{}:hermes", req.job_id),
            session_id: config.session_id.clone(),
            model: config.model.clone(),
            cwd: config.cwd.clone(),
            executable: config.executable.clone(),
            seq: 0,
            source_seq: 0,
            pending_llm: None,
            tool_calls: HashMap::new(),
        }
    }

    fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    fn capture_session(&mut self, value: &Value) {
        if let Some(session_id) = find_string(value, &["session_id", "sessionId", "id"]) {
            self.session_id = Some(session_id.to_string());
        }
    }

    fn format_initialize(&mut self, raw: &Value) -> Vec<Value> {
        let mut records = self.flush_llm_message();
        records.push(self.record("status", json!({"status": "initialized"}), raw.clone()));
        records
    }

    fn format_session(&mut self, raw: &Value) -> Vec<Value> {
        let mut records = self.flush_llm_message();
        records.push(self.record("agent_session", json!({}), raw.clone()));
        records
    }

    fn format_status(&mut self, status: &str, raw: &Value) -> Vec<Value> {
        let mut records = self.flush_llm_message();
        records.push(self.record("status", json!({ "status": status }), raw.clone()));
        records
    }

    fn format_prompt_result(&mut self, raw: &Value) -> Vec<Value> {
        let mut records = self.flush_llm_message();
        let text = output_text(raw);
        if !text.is_empty() {
            records.push(self.record(
                "llm_message",
                json!({
                    "channel": "message",
                    "responseText": text,
                }),
                raw.clone(),
            ));
        }
        records.push(self.record("llm_call_end", usage_payload(raw), raw.clone()));
        records
    }

    fn format_permission_request(&mut self, raw: &Value, option_id: &str) -> Vec<Value> {
        let params = raw.get("params").cloned().unwrap_or_else(|| raw.clone());
        let mut records = self.flush_llm_message();
        records.push(self.record(
            "permission_request",
            permission_payload(&params),
            raw.clone(),
        ));
        records.push(self.record(
            "policy_decision",
            json!({
                "decision": "approve",
                "optionId": option_id,
                "scope": "session",
                "reason": "Hermes ACP requested permission through session/request_permission",
            }),
            raw.clone(),
        ));
        records
    }

    fn format_policy_decision(&mut self, raw: &Value, decision: &str, reason: &str) -> Vec<Value> {
        let mut records = self.flush_llm_message();
        records.push(self.record(
            "policy_decision",
            json!({
                "decision": decision,
                "reason": reason,
            }),
            raw.clone(),
        ));
        records
    }

    fn format_provider_error(&mut self, error: &ProviderError) -> Vec<Value> {
        let mut records = self.flush_llm_message();
        records.push(self.record(
            "error",
            json!({
                "code": error.code.clone(),
                "message": error.message.clone(),
                "source": "stderr",
                "isProviderError": true,
            }),
            json!({
                "source": "stderr",
                "message": error.message.clone(),
            }),
        ));
        records
    }

    fn format_event(&mut self, raw: Value) -> Vec<Value> {
        self.source_seq += 1;
        if raw.get("ahand_parse_error").is_some() {
            let mut records = self.flush_llm_message();
            records.push(self.record("parse_error", raw.clone(), raw));
            return records;
        }

        let method = raw.get("method").and_then(Value::as_str).unwrap_or("");
        let params = raw.get("params").cloned().unwrap_or_else(|| raw.clone());
        if matches!(method, "session/update" | "session/notification") {
            return self.format_session_update(&params, raw);
        }

        let lower = method.to_ascii_lowercase();

        if lower.contains("tool") && (lower.contains("start") || lower.contains("call")) {
            let mut records = self.flush_llm_message();
            let payload = self.tool_payload_for_event(&params, "started");
            records.push(self.record("tool_call_start", payload, raw));
            return records;
        }
        if lower.contains("tool") && (lower.contains("result") || lower.contains("output")) {
            let mut records = self.flush_llm_message();
            let payload = self.tool_payload_for_event(&params, "output");
            records.push(self.record("tool_call_output", payload, raw));
            return records;
        }
        if lower.contains("tool") && (lower.contains("end") || lower.contains("finish")) {
            let mut records = self.flush_llm_message();
            let payload = self.tool_payload_for_event(&params, "completed");
            records.push(self.record("tool_call_end", payload, raw));
            return records;
        }
        if lower.contains("error") {
            let mut records = self.flush_llm_message();
            records.push(self.record("error", params, raw));
            return records;
        }
        if let Some(text) = find_string(&params, &["text", "content", "delta", "message"]) {
            return self.push_llm_chunk("message", text, "text");
        }

        let mut records = self.flush_llm_message();
        records.push(self.record("raw", json!({}), raw));
        records
    }

    fn format_session_update(&mut self, params: &Value, raw: Value) -> Vec<Value> {
        if let Some(session_id) = find_string(params, &["sessionId", "session_id"]) {
            self.session_id = Some(session_id.to_string());
        }

        let update = params.get("update").unwrap_or(params);
        let update_type = update_type(update);
        let update_body = externally_tagged_body(update).unwrap_or(update);

        match update_type.as_deref() {
            Some("agent_message_chunk") | Some("AgentMessageChunk") => {
                self.push_llm_chunk("message", &content_text(update_body), "agent_message_chunk")
            }
            Some("agent_thought_chunk") | Some("AgentThoughtChunk") => self.push_llm_chunk(
                "thinking",
                &content_text(update_body),
                "agent_thought_chunk",
            ),
            Some("tool_call") | Some("ToolCall") => {
                let mut records = self.flush_llm_message();
                let payload = self.tool_payload_for_event(update_body, "started");
                records.push(self.record("tool_call_start", payload, raw));
                records
            }
            Some("tool_call_update") | Some("ToolCallUpdate") => {
                let status = find_string(update_body, &["status"]).unwrap_or("completed");
                let kind = if matches!(status, "completed" | "failed") {
                    "tool_call_end"
                } else {
                    "tool_call_output"
                };
                let mut records = self.flush_llm_message();
                let payload = self.tool_payload_for_event(update_body, status);
                records.push(self.record(kind, payload, raw));
                records
            }
            Some("usage_update") | Some("UsageUpdate") => {
                let mut records = self.flush_llm_message();
                records.push(self.record("llm_call_end", usage_payload(update_body), raw));
                records
            }
            Some("permission_request") | Some("PermissionRequest") => {
                let mut records = self.flush_llm_message();
                records.push(self.record(
                    "permission_request",
                    permission_payload(update_body),
                    raw,
                ));
                records
            }
            Some("policy_decision") | Some("PolicyDecision") => {
                let mut records = self.flush_llm_message();
                records.push(self.record("policy_decision", update_body.clone(), raw));
                records
            }
            Some("turn_end") | Some("end_turn") | Some("TurnEnd") => {
                let mut records = self.flush_llm_message();
                records.push(self.record("llm_call_end", usage_payload(update_body), raw));
                records
            }
            Some("plan") | Some("Plan") => {
                let mut records = self.flush_llm_message();
                records.push(self.record("plan_update", plan_payload(update_body), raw));
                records
            }
            _ => {
                let mut records = self.flush_llm_message();
                records.push(self.record("raw", json!({ "updateType": update_type }), raw));
                records
            }
        }
    }

    fn tool_payload_for_event(&mut self, value: &Value, status: &str) -> Value {
        let mut payload = tool_payload(value, status);
        let tool_call_id = payload
            .get("toolCallId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        if let Some(state) = self.tool_calls.get(&tool_call_id) {
            if payload
                .get("toolName")
                .and_then(Value::as_str)
                .unwrap_or("")
                .is_empty()
            {
                payload["toolName"] = json!(state.tool_name);
            }
            if payload
                .get("toolKind")
                .and_then(Value::as_str)
                .unwrap_or("")
                .is_empty()
            {
                payload["toolKind"] = json!(state.tool_kind);
            }
            if payload.get("input").is_none_or(Value::is_null) && !state.input.is_null() {
                payload["input"] = state.input.clone();
            }
        }

        if !tool_call_id.is_empty() && status == "started" {
            self.tool_calls.insert(
                tool_call_id.clone(),
                ToolCallState {
                    tool_name: payload
                        .get("toolName")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    tool_kind: payload
                        .get("toolKind")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    input: payload.get("input").cloned().unwrap_or(Value::Null),
                },
            );
        }

        if matches!(status, "completed" | "failed") {
            self.tool_calls.remove(&tool_call_id);
        }

        payload
    }

    fn push_llm_chunk(&mut self, channel: &str, text: &str, source_kind: &str) -> Vec<Value> {
        let mut records = Vec::new();
        if text.is_empty() {
            return records;
        }

        let should_flush = self
            .pending_llm
            .as_ref()
            .is_some_and(|pending| pending.channel != channel);
        if should_flush {
            records.extend(self.flush_llm_message());
        }

        let now = now_ms();
        match &mut self.pending_llm {
            Some(pending) if pending.channel == channel => {
                pending.text.push_str(text);
                pending.end_seq = self.source_seq;
                pending.chunk_count += 1;
                pending.ended_at_ms = now;
            }
            _ => {
                self.pending_llm = Some(PendingLlmMessage {
                    channel: channel.to_string(),
                    text: text.to_string(),
                    start_seq: self.source_seq,
                    end_seq: self.source_seq,
                    chunk_count: 1,
                    started_at_ms: now,
                    ended_at_ms: now,
                    source_kind: source_kind.to_string(),
                });
            }
        }
        records
    }

    fn flush_llm_message(&mut self) -> Vec<Value> {
        let Some(pending) = self.pending_llm.take() else {
            return Vec::new();
        };
        if pending.text.is_empty() {
            return Vec::new();
        }

        let mut record = self.record(
            "llm_message",
            json!({
                "channel": pending.channel,
                "responseText": pending.text,
            }),
            json!({
                "source": "stdout",
                "protocol": "acp-json-rpc",
                "parser": "hermes",
                "parserVersion": 1,
                "aggregated": true,
            }),
        );
        record["stream"] = json!({
            "sourceKind": pending.source_kind,
            "chunkCount": pending.chunk_count,
            "startSeq": pending.start_seq,
            "endSeq": pending.end_seq,
        });
        record["time"]["startedAtMs"] = json!(pending.started_at_ms);
        record["time"]["endedAtMs"] = json!(pending.ended_at_ms);
        vec![record]
    }

    fn record(&mut self, kind: &str, payload: Value, raw: Value) -> Value {
        self.seq += 1;
        let mut record = json!({
            "schemaVersion": 1,
            "jobId": self.job_id,
            "seq": self.seq,
            "kind": kind,
            "agent": {
                "agentId": self.agent_id,
                "agentKind": "hermes",
                "model": {
                    "provider": "nous",
                    "id": self.model.as_deref().unwrap_or("unknown"),
                },
            },
            "time": {
                "observedAtMs": now_ms(),
            },
            "runtime": {
                "jobId": self.job_id,
                "executionMode": "pipe_stream",
                "resultParser": "hermes",
                "inputFormat": "hermes-acp-json-rpc",
                "outputFormat": "hermes-acp-json-rpc",
                "cwd": self.cwd,
                "tool": self.executable,
                "args": ["acp"],
            },
            "raw": {
                "source": "stdout",
                "protocol": "acp-json-rpc",
                "parser": "hermes",
                "parserVersion": 1,
                "json": raw,
            },
        });
        if let Some(session_id) = &self.session_id {
            record["agent"]["agentSessionId"] = json!(session_id);
        }
        match kind {
            "llm_message" | "llm_call_delta" | "llm_call_end" => record["llmResponse"] = payload,
            "tool_call_start" | "tool_call_output" | "tool_call_end" => {
                record["toolCall"] = payload;
            }
            "error" | "parse_error" => record["error"] = payload,
            "status" => record["status"] = payload,
            "permission_request" => record["permission"] = payload,
            "policy_decision" => record["policy"] = payload,
            "plan_update" => record["plan"] = payload,
            _ => {}
        }
        record
    }
}

fn emit_records<T>(
    device_id: &str,
    job_id: &str,
    tx: &T,
    store: &Option<Arc<RunStore>>,
    records: Vec<Value>,
) where
    T: EnvelopeSink,
{
    for record in records {
        if let Some(store) = store {
            store.append_observation(job_id, &record);
        }
        if let Ok(mut line) = serde_json::to_vec(&record) {
            line.push(b'\n');
            if let Some(store) = store {
                store.append_stdout(job_id, &line);
            }
            let _ = tx.send(make_event_envelope(device_id, job_id, Some(line), None));
        }
    }
}

fn append_json_line(store: &Option<Arc<RunStore>>, job_id: &str, name: &str, value: &Value) {
    if let Some(store) = store
        && let Ok(mut line) = serde_json::to_vec(value)
    {
        line.push(b'\n');
        store.append_artifact(job_id, name, &line);
    }
}

fn make_event_envelope(
    device_id: &str,
    job_id: &str,
    stdout_chunk: Option<Vec<u8>>,
    stderr_chunk: Option<Vec<u8>>,
) -> Envelope {
    Envelope {
        device_id: device_id.to_string(),
        msg_id: new_msg_id(),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::JobEvent(JobEvent {
            job_id: job_id.to_string(),
            event: if let Some(data) = stdout_chunk {
                Some(job_event::Event::StdoutChunk(data))
            } else {
                stderr_chunk.map(job_event::Event::StderrChunk)
            },
        })),
        ..Default::default()
    }
}

fn finish(
    device_id: &str,
    job_id: &str,
    exit_code: i32,
    error: &str,
    tx: &impl EnvelopeSink,
    store: &Option<Arc<RunStore>>,
) -> (i32, String) {
    if let Some(store) = store {
        store.finish_run(job_id, exit_code, error);
    }
    let envelope = Envelope {
        device_id: device_id.to_string(),
        msg_id: new_msg_id(),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::JobFinished(JobFinished {
            job_id: job_id.to_string(),
            exit_code,
            error: error.to_string(),
        })),
        ..Default::default()
    };
    let _ = tx.send(envelope);
    (exit_code, error.to_string())
}

fn tool_payload(value: &Value, status: &str) -> Value {
    let input = value
        .get("rawInput")
        .or_else(|| value.get("input"))
        .or_else(|| value.get("parameters"))
        .cloned()
        .unwrap_or(Value::Null);
    let output = output_text(value);
    let title = find_string(value, &["title"]).unwrap_or("");
    let tool_name = infer_tool_name(title)
        .or_else(|| find_string(value, &["name", "tool", "command"]).map(str::to_string))
        .unwrap_or_default();
    let mut payload = json!({
        "toolCallId": find_string(value, &["toolCallId", "tool_call_id", "id"]).unwrap_or(""),
        "toolName": tool_name,
        "toolKind": find_string(value, &["kind"]).unwrap_or("hermes-acp"),
        "status": status,
        "input": input,
    });
    if !output.is_empty() {
        payload["outputText"] = json!(output);
    }
    payload
}

fn permission_payload(value: &Value) -> Value {
    let mut payload = json!({
        "permissionId": find_string(value, &["permissionId", "permission_id", "id"]).unwrap_or(""),
        "toolCallId": find_string(value, &["toolCallId", "tool_call_id"]).unwrap_or(""),
        "title": find_string(value, &["title"]).unwrap_or(""),
        "kind": find_string(value, &["kind", "type"]).unwrap_or("permission"),
        "options": value.get("options").cloned().unwrap_or(Value::Null),
    });
    if let Some(text) = find_string(value, &["message", "description", "reason"]) {
        payload["message"] = json!(text);
    }
    payload
}

fn select_permission_option_id(frame: &Value) -> Option<String> {
    let options = frame
        .get("params")
        .and_then(|params| params.get("options"))
        .or_else(|| frame.get("options"))?
        .as_array()?;

    let option_matches = |option: &Value, candidates: &[&str]| -> bool {
        ["optionId", "option_id", "id", "kind", "name"]
            .iter()
            .filter_map(|key| option.get(*key).and_then(Value::as_str))
            .any(|value| {
                let normalized = value.to_ascii_lowercase();
                candidates
                    .iter()
                    .any(|candidate| normalized == *candidate || normalized.contains(candidate))
            })
    };

    let option_id = |option: &Value| -> Option<String> {
        find_string(option, &["optionId", "option_id", "id"]).map(str::to_string)
    };

    for candidate in ["allow_session", "approve_for_session", "allow_always"] {
        if let Some(id) = options
            .iter()
            .find(|option| option_matches(option, &[candidate]))
            .and_then(option_id)
        {
            return Some(id);
        }
    }

    options
        .iter()
        .find(|option| {
            option_matches(option, &["allow", "approve"])
                && !option_matches(option, &["deny", "reject"])
        })
        .and_then(option_id)
        .or_else(|| {
            options
                .iter()
                .find(|option| !option_matches(option, &["deny", "reject"]))
                .and_then(option_id)
        })
}

fn plan_payload(value: &Value) -> Value {
    json!({
        "entries": value.get("entries").cloned().unwrap_or_else(|| json!([])),
    })
}

fn usage_payload(value: &Value) -> Value {
    let usage = value.get("usage").unwrap_or(value);
    let mut payload = json!({
        "usage": {
            "inputTokens": find_u64(usage, &["inputTokens", "input_tokens", "prompt_tokens"]),
            "outputTokens": find_u64(usage, &["outputTokens", "output_tokens", "completion_tokens"]),
            "cachedReadTokens": find_u64(usage, &["cachedReadTokens", "cached_read_tokens"]),
            "totalTokens": find_u64(usage, &["totalTokens", "total_tokens"]),
            "thoughtTokens": find_u64(usage, &["thoughtTokens", "thought_tokens"]),
        }
    });
    if let Some(stop_reason) = find_string(value, &["stopReason", "stop_reason"]) {
        payload["stopReason"] = json!(stop_reason);
    }
    payload
}

fn update_type(value: &Value) -> Option<String> {
    if let Some(kind) = find_string(value, &["sessionUpdate", "type"]) {
        return Some(kind.to_string());
    }
    externally_tagged_name(value).map(to_snake_case)
}

fn externally_tagged_name(value: &Value) -> Option<&str> {
    let object = value.as_object()?;
    if object.len() == 1 {
        object.keys().next().map(String::as_str)
    } else {
        None
    }
}

fn externally_tagged_body(value: &Value) -> Option<&Value> {
    let key = externally_tagged_name(value)?;
    value.get(key)
}

fn content_text(value: &Value) -> String {
    if let Some(content) = value.get("content") {
        return output_text(content);
    }
    output_text(value)
}

fn output_text(value: &Value) -> String {
    if let Some(items) = value.as_array() {
        return join_content_items(items);
    }
    if value.get("type").and_then(Value::as_str) == Some("diff") {
        return diff_summary(value);
    }
    if let Some(text) = find_string(value, &["rawOutput", "output", "text"]) {
        return text.to_string();
    }
    if let Some(items) = value.get("content").and_then(Value::as_array) {
        return join_content_items(items);
    }
    if let Some(text) = value
        .get("content")
        .and_then(|content| find_string(content, &["text"]))
    {
        return text.to_string();
    }
    String::new()
}

fn join_content_items(items: &[Value]) -> String {
    let mut out = String::new();
    for item in items {
        if item.get("type").and_then(Value::as_str) == Some("diff") {
            out.push_str(&diff_summary(item));
        } else if let Some(text) = find_string(item, &["text"]) {
            out.push_str(text);
        } else if let Some(content) = item.get("content") {
            out.push_str(&output_text(content));
        }
    }
    out
}

fn diff_summary(item: &Value) -> String {
    let path = find_string(item, &["path", "filePath", "file_path"]).unwrap_or("unknown");
    let old_len = find_string(item, &["oldText", "old_text"])
        .map(str::len)
        .unwrap_or(0);
    let new_len = find_string(item, &["newText", "new_text"])
        .map(str::len)
        .unwrap_or(0);
    format!("--- {path}\n+++ {path}\n(edited: {old_len} -> {new_len} bytes)\n")
}

fn infer_tool_name(title: &str) -> Option<String> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some((name, _)) = trimmed.split_once(':') {
        let name = name.trim();
        if !name.is_empty() {
            return Some(name.to_string());
        }
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("execute code") {
        return Some("execute_code".to_string());
    }
    lower.split_whitespace().next().map(str::to_string)
}

fn to_snake_case(value: &str) -> String {
    let mut out = String::new();
    for (index, ch) in value.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn find_string<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    for key in keys {
        if let Some(text) = value.get(*key).and_then(Value::as_str) {
            return Some(text);
        }
    }
    None
}

fn find_u64(value: &Value, keys: &[&str]) -> Option<u64> {
    for key in keys {
        if let Some(number) = value.get(*key).and_then(Value::as_u64) {
            return Some(number);
        }
    }
    None
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn new_msg_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!("hermes-acp-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ahand_protocol::ExecutionMode;

    fn req() -> JobRequest {
        JobRequest {
            job_id: "job-1".to_string(),
            tool: "/bin/hermes".to_string(),
            cwd: "/repo".to_string(),
            env: HashMap::from([
                (
                    INPUT_FORMAT_ENV.to_string(),
                    ahand_protocol::INPUT_FORMAT_HERMES_ACP_JSON_RPC.to_string(),
                ),
                (
                    OUTPUT_FORMAT_ENV.to_string(),
                    ahand_protocol::OUTPUT_FORMAT_HERMES_ACP_JSON_RPC.to_string(),
                ),
                (PROMPT_ENV.to_string(), "hello".to_string()),
                (MODEL_ENV.to_string(), "Hermes-3".to_string()),
            ]),
            execution_mode: ExecutionMode::PipeStream as i32,
            input_format: ahand_protocol::INPUT_FORMAT_HERMES_ACP_JSON_RPC.to_string(),
            output_format: ahand_protocol::OUTPUT_FORMAT_HERMES_ACP_JSON_RPC.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn recognizes_hermes_acp_jobs_by_explicit_env() {
        assert!(is_hermes_acp_job(&req()));
        let mut other = req();
        other.env.remove(INPUT_FORMAT_ENV);
        other.input_format.clear();
        assert!(!is_hermes_acp_job(&other));
    }

    #[test]
    fn config_requires_prompt() {
        let mut req = req();
        req.env.remove(PROMPT_ENV);
        let error = HermesAcpConfig::from_job(&req).unwrap_err();
        assert!(error.contains(PROMPT_ENV));
    }

    #[test]
    fn config_converts_mcp_config_body_to_hermes_servers() {
        let mut req = req();
        req.env.insert(
            MCP_CONFIG_ENV.to_string(),
            r#"{"mcpServers":{"fs":{"command":"npx","args":["-y","server"],"env":{"TOKEN":"secret"}}}}"#
                .to_string(),
        );
        let config = HermesAcpConfig::from_job(&req).unwrap();
        let servers = config.mcp_config.unwrap().hermes_servers().unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0]["name"], "fs");
        assert_eq!(servers[0]["command"], "npx");
        assert_eq!(servers[0]["args"][1], "server");
        assert_eq!(servers[0]["env"][0]["name"], "TOKEN");
        assert_eq!(servers[0]["env"][0]["value"], "secret");
    }

    #[test]
    fn formatter_aggregates_text_observations() {
        let req = req();
        let config = HermesAcpConfig::from_job(&req).unwrap();
        let mut formatter = HermesAcpFormatter::new(&req, &config);
        let session = json!({"sessionId": "s-1"});
        formatter.capture_session(&session);
        let records = formatter.format_session(&session);
        assert_eq!(records[0]["kind"], "agent_session");
        assert_eq!(records[0]["agent"]["agentSessionId"], "s-1");

        let records = formatter.format_event(json!({
            "method": "session/update",
            "params": {
                "sessionId": "s-1",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": "hi" }
                }
            }
        }));
        assert!(records.is_empty());

        let records = formatter.format_event(json!({
            "method": "session/update",
            "params": {
                "sessionId": "s-1",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": " there" }
                }
            }
        }));
        assert!(records.is_empty());

        let records = formatter.format_event(json!({
            "method": "session/update",
            "params": {
                "sessionId": "s-1",
                "update": {
                    "sessionUpdate": "agent_thought_chunk",
                    "content": { "type": "text", "text": "thinking" }
                }
            }
        }));
        assert_eq!(records[0]["kind"], "llm_message");
        assert_eq!(records[0]["llmResponse"]["channel"], "message");
        assert_eq!(records[0]["llmResponse"]["responseText"], "hi there");
        assert_eq!(records[0]["stream"]["chunkCount"], 2);
    }

    #[test]
    fn formatter_maps_tool_update_and_usage() {
        let req = req();
        let config = HermesAcpConfig::from_job(&req).unwrap();
        let mut formatter = HermesAcpFormatter::new(&req, &config);
        let records = formatter.format_event(json!({
            "method": "session/update",
            "params": {
                "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "tc-start-end",
                    "title": "terminal: python3 script.py",
                    "kind": "execute",
                    "rawInput": {"cmd": "python3 script.py"}
                }
            }
        }));
        assert_eq!(records[0]["kind"], "tool_call_start");
        assert_eq!(records[0]["toolCall"]["toolName"], "terminal");
        let records = formatter.format_event(json!({
            "method": "session/update",
            "params": {
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "tc-start-end",
                    "kind": "execute",
                    "status": "completed",
                    "content": [{"type": "content", "content": {"type": "text", "text": "done"}}]
                }
            }
        }));
        assert_eq!(records[0]["kind"], "tool_call_end");
        assert_eq!(records[0]["toolCall"]["toolName"], "terminal");
        assert_eq!(records[0]["toolCall"]["input"]["cmd"], "python3 script.py");

        let records = formatter.format_event(json!({
            "method": "session/update",
            "params": {
                "update": {
                    "type": "ToolCallUpdate",
                    "toolCallId": "tc-1",
                    "title": "terminal: ls -la",
                    "status": "completed",
                    "rawOutput": "ok\n"
                }
            }
        }));
        assert_eq!(records[0]["kind"], "tool_call_end");
        assert_eq!(records[0]["toolCall"]["toolName"], "terminal");
        assert_eq!(records[0]["toolCall"]["outputText"], "ok\n");

        let records = formatter.format_event(json!({
            "method": "session/notification",
            "params": {
                "update": {
                    "type": "TurnEnd",
                    "stopReason": "end_turn",
                    "usage": { "inputTokens": 3, "outputTokens": 4, "cachedReadTokens": 1 }
                }
            }
        }));
        assert_eq!(records[0]["kind"], "llm_call_end");
        assert_eq!(records[0]["llmResponse"]["usage"]["inputTokens"], 3);
        assert_eq!(records[0]["llmResponse"]["usage"]["cachedReadTokens"], 1);
    }

    #[test]
    fn formatter_maps_content_diff_permission_and_provider_error() {
        let req = req();
        let config = HermesAcpConfig::from_job(&req).unwrap();
        let mut formatter = HermesAcpFormatter::new(&req, &config);

        let records = formatter.format_event(json!({
            "method": "session/update",
            "params": {
                "update": {
                    "type": "AgentMessageChunk",
                    "content": [
                        { "type": "text", "text": "changed\n" },
                        { "type": "diff", "path": "src/main.rs", "oldText": "a", "newText": "ab" }
                    ]
                }
            }
        }));
        assert!(records.is_empty());
        let records = formatter.format_prompt_result(&json!({
            "stopReason": "end_turn",
            "usage": { "inputTokens": 1, "outputTokens": 1 }
        }));
        assert_eq!(records[0]["kind"], "llm_message");
        let text = records[0]["llmResponse"]["responseText"].as_str().unwrap();
        assert!(text.contains("changed"));
        assert!(text.contains("src/main.rs"));

        let records = formatter.format_permission_request(
            &json!({
                "jsonrpc": "2.0",
                "id": 99,
                "method": "session/request_permission",
                "params": {
                    "permissionId": "perm-1",
                    "toolCallId": "tc-1",
                    "title": "terminal: cargo test",
                    "options": [{"id": "approve_for_session"}]
                }
            }),
            "approve_for_session",
        );
        assert_eq!(records[0]["kind"], "permission_request");
        assert_eq!(records[0]["permission"]["permissionId"], "perm-1");
        assert_eq!(records[1]["kind"], "policy_decision");
        assert_eq!(records[1]["policy"]["decision"], "approve");

        let error = detect_provider_error("Provider error: 429 rate limit").unwrap();
        let records = formatter.format_provider_error(&error);
        assert_eq!(records[0]["kind"], "error");
        assert_eq!(records[0]["error"]["code"], "provider_rate_limited");
        assert_eq!(records[0]["error"]["isProviderError"], true);

        assert!(
            detect_provider_error(
                "Tool mcp_capability_hub_youtube_find_creator_email returned error: HTTP 404"
            )
            .is_none()
        );
        assert!(
            detect_provider_error(
                "Stream drop mid tool-call on attempt 2/3 — retrying. error_type=ReadTimeout error=The read operation timed out"
            )
            .is_none()
        );
        assert!(
            detect_provider_error(
                "custom stream drop (ReadTimeout) after 127.0s — reconnecting, retry 3/3"
            )
            .is_none()
        );
        let error =
            detect_provider_error("API call failed after 3 retries. HTTP 402: quota exceeded")
                .unwrap();
        assert_eq!(error.code, "provider_quota_exceeded");
        let error =
            detect_provider_error("API call failed after 3 retries: ReadTimeout provider error")
                .unwrap();
        assert_eq!(error.code, "provider_error");
    }

    #[test]
    fn permission_option_selection_uses_actual_hermes_options() {
        let frame = json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "session/request_permission",
            "params": {
                "options": [
                    {"kind": "allow_once", "name": "Allow once", "optionId": "allow_once"},
                    {"kind": "allow_always", "name": "Allow for session", "optionId": "allow_session"},
                    {"kind": "allow_always", "name": "Allow always", "optionId": "allow_always"},
                    {"kind": "reject_once", "name": "Deny", "optionId": "deny"}
                ]
            }
        });
        assert_eq!(
            select_permission_option_id(&frame).as_deref(),
            Some("allow_session")
        );
    }

    #[test]
    fn permission_option_selection_keeps_legacy_approve_id() {
        let frame = json!({
            "params": {
                "options": [
                    {"id": "approve_for_session"},
                    {"id": "deny"}
                ]
            }
        });
        assert_eq!(
            select_permission_option_id(&frame).as_deref(),
            Some("approve_for_session")
        );
    }

    #[test]
    fn permission_option_selection_falls_back_without_options() {
        assert_eq!(select_permission_option_id(&json!({})), None);
    }

    #[tokio::test]
    async fn run_hermes_acp_with_fake_cli_emits_observations() {
        use prost::Message;
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let hermes = dir.path().join("fake-hermes");
        let mut file = std::fs::File::create(&hermes).unwrap();
        writeln!(
            file,
            r#"#!/bin/sh
i=0
while IFS= read -r line; do
  i=$((i + 1))
  case "$i" in
    1)
      echo '{{"jsonrpc":"2.0","id":1,"result":{{"ok":true}}}}'
      ;;
    2)
      echo '{{"jsonrpc":"2.0","id":2,"result":{{"sessionId":"s-1"}}}}'
      ;;
    3)
      echo '{{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"s-1","update":{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":"event text"}}}}}}}}'
      echo '{{"jsonrpc":"2.0","id":3,"result":{{"text":"final text"}}}}'
      ;;
    4)
      echo '{{"jsonrpc":"2.0","id":4,"result":{{"stopReason":"end_turn","usage":{{"inputTokens":1,"outputTokens":2}}}}}}'
      exit 0
      ;;
  esac
done
"#
        )
        .unwrap();
        drop(file);
        let mut perms = std::fs::metadata(&hermes).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&hermes, perms).unwrap();

        let mut req = req();
        req.tool = hermes.to_string_lossy().to_string();
        req.cwd = dir.path().to_string_lossy().to_string();
        req.env.insert(EXECUTABLE_ENV.to_string(), req.tool.clone());
        req.env.remove(MODEL_ENV);

        let (tx, mut rx) = mpsc::unbounded_channel::<Envelope>();
        let (_cancel_tx, cancel_rx) = mpsc::channel(1);
        let (exit_code, error) =
            run_hermes_acp("device-1".to_string(), req, tx, cancel_rx, None).await;

        assert_eq!(exit_code, 0, "{error}");
        assert!(error.is_empty());

        let mut stdout = String::new();
        let mut finished = false;
        while let Ok(envelope) = rx.try_recv() {
            let bytes = envelope.encode_to_vec();
            let envelope = Envelope::decode(bytes.as_slice()).unwrap();
            match envelope.payload {
                Some(envelope::Payload::JobEvent(event)) => {
                    if let Some(job_event::Event::StdoutChunk(chunk)) = event.event {
                        stdout.push_str(&String::from_utf8_lossy(&chunk));
                    }
                }
                Some(envelope::Payload::JobFinished(finish)) => {
                    finished = finish.exit_code == 0;
                }
                _ => {}
            }
        }

        assert!(finished);
        assert!(stdout.contains("\"kind\":\"agent_session\""), "{stdout}");
        assert!(stdout.contains("event text"), "{stdout}");
        assert!(stdout.contains("final text"), "{stdout}");
    }
}
