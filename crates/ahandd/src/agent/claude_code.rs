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

const INPUT_FORMAT_ENV: &str = "AHAND_INPUT_FORMAT";
const OUTPUT_FORMAT_ENV: &str = "AHAND_OUTPUT_FORMAT";
const EXECUTABLE_ENV: &str = "AHAND_AGENT_EXECUTABLE";
const PROMPT_ENV: &str = "AHAND_AGENT_PROMPT";
const MODEL_ENV: &str = "AHAND_AGENT_MODEL";
const SESSION_ENV: &str = "AHAND_AGENT_SESSION_ID";
const MAX_TURNS_ENV: &str = "AHAND_AGENT_MAX_TURNS";
const SYSTEM_PROMPT_ENV: &str = "AHAND_AGENT_SYSTEM_PROMPT";
const PERMISSION_MODE_ENV: &str = "AHAND_AGENT_PERMISSION_MODE";
const INSTRUCTIONS_ENV: &str = "AHAND_AGENT_INSTRUCTIONS";

pub fn is_claude_code_job(req: &JobRequest) -> bool {
    ahand_protocol::resolve_job_input_format(req) == ahand_protocol::INPUT_FORMAT_CLAUDE_STREAM_JSON
        && ahand_protocol::resolve_job_output_format(req)
            == ahand_protocol::OUTPUT_FORMAT_CLAUDE_STREAM_JSON
        || req
            .env
            .get(INPUT_FORMAT_ENV)
            .is_some_and(|value| value == ahand_protocol::INPUT_FORMAT_CLAUDE_STREAM_JSON)
            && req
                .env
                .get(OUTPUT_FORMAT_ENV)
                .is_some_and(|value| value == ahand_protocol::OUTPUT_FORMAT_CLAUDE_STREAM_JSON)
}

#[derive(Debug, Clone)]
struct ClaudeCodeConfig {
    executable: String,
    cwd: String,
    env: HashMap<String, String>,
    prompt: String,
    model: Option<String>,
    session_id: Option<String>,
    max_turns: Option<String>,
    system_prompt: Option<String>,
    permission_mode: Option<String>,
    instructions: Option<String>,
}

impl ClaudeCodeConfig {
    fn from_job(req: &JobRequest) -> Result<Self, String> {
        let executable = req
            .env
            .get(EXECUTABLE_ENV)
            .filter(|value| !value.trim().is_empty())
            .cloned()
            .or_else(|| (!req.tool.trim().is_empty()).then(|| req.tool.clone()))
            .ok_or_else(|| {
                "claude-code requires AHAND_AGENT_EXECUTABLE or JobRequest.tool".to_string()
            })?;

        let prompt = req
            .env
            .get(PROMPT_ENV)
            .filter(|value| !value.trim().is_empty())
            .cloned()
            .ok_or_else(|| "claude-code requires AHAND_AGENT_PROMPT".to_string())?;

        Ok(Self {
            executable,
            cwd: req.cwd.clone(),
            env: req.env.clone(),
            prompt,
            model: env_value(req, MODEL_ENV),
            session_id: env_value(req, SESSION_ENV),
            max_turns: env_value(req, MAX_TURNS_ENV),
            system_prompt: env_value(req, SYSTEM_PROMPT_ENV),
            permission_mode: env_value(req, PERMISSION_MODE_ENV),
            instructions: env_value(req, INSTRUCTIONS_ENV),
        })
    }
}

fn env_value(req: &JobRequest, key: &str) -> Option<String> {
    req.env
        .get(key)
        .filter(|value| !value.trim().is_empty())
        .cloned()
}

pub async fn run_claude_code<T>(
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
    info!(job_id = %job_id, "starting Claude Code job");

    if let Some(store) = &store {
        store.start_run(&job_id, &req);
    }

    let config = match ClaudeCodeConfig::from_job(&req) {
        Ok(config) => config,
        Err(error) => return finish(&device_id, &job_id, -1, &error, &tx, &store),
    };

    if let Err(error) = prepare_context(&config, &job_id, &store) {
        return finish(&device_id, &job_id, -1, &error, &tx, &store);
    }

    let mut child = match spawn_claude(&config).await {
        Ok(child) => child,
        Err(error) => return finish(&device_id, &job_id, -1, &error, &tx, &store),
    };

    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill().await;
        return finish(
            &device_id,
            &job_id,
            -1,
            "failed to open Claude stdin",
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
            "failed to open Claude stdout",
            &tx,
            &store,
        );
    };

    let stdin_message = user_message(&config.prompt);
    append_json_line(&store, &job_id, "claude-stdin.jsonl", &stdin_message);
    let mut stdin_line = match serde_json::to_vec(&stdin_message) {
        Ok(line) => line,
        Err(error) => {
            let _ = child.kill().await;
            return finish(
                &device_id,
                &job_id,
                -1,
                &format!("failed to encode Claude prompt: {error}"),
                &tx,
                &store,
            );
        }
    };
    stdin_line.push(b'\n');
    if let Err(error) = stdin.write_all(&stdin_line).await {
        let _ = child.kill().await;
        return finish(
            &device_id,
            &job_id,
            -1,
            &format!("failed to write Claude prompt: {error}"),
            &tx,
            &store,
        );
    }
    drop(stdin);

    let stdout_tx = tx.clone();
    let stdout_store = store.clone();
    let stdout_device_id = device_id.clone();
    let stdout_job_id = job_id.clone();
    let formatter = ClaudeCodeFormatter::new(&req, &config);
    let stdout_handle = tokio::spawn(async move {
        read_stdout(
            stdout,
            formatter,
            stdout_tx,
            stdout_store,
            stdout_device_id,
            stdout_job_id,
        )
        .await
    });

    let stderr_tail = Arc::new(Mutex::new(String::new()));
    let stderr = child.stderr.take();
    let stderr_tx = tx.clone();
    let stderr_store = store.clone();
    let stderr_device_id = device_id.clone();
    let stderr_job_id = job_id.clone();
    let stderr_tail_reader = stderr_tail.clone();
    let stderr_handle = tokio::spawn(async move {
        if let Some(stderr) = stderr {
            read_stderr(
                stderr,
                stderr_tx,
                stderr_store,
                stderr_device_id,
                stderr_job_id,
                stderr_tail_reader,
            )
            .await;
        }
    });

    let timeout = (req.timeout_ms > 0).then(|| std::time::Duration::from_millis(req.timeout_ms));
    let wait = child.wait();
    let wait_result = match timeout {
        Some(timeout) => {
            tokio::select! {
                _ = cancel_rx.recv() => Err("cancelled".to_string()),
                result = tokio::time::timeout(timeout, wait) => {
                    match result {
                        Ok(Ok(status)) => Ok(status),
                        Ok(Err(error)) => Err(format!("failed to wait for Claude: {error}")),
                        Err(_) => Err("timeout".to_string()),
                    }
                }
            }
        }
        None => {
            tokio::select! {
                _ = cancel_rx.recv() => Err("cancelled".to_string()),
                result = wait => result.map_err(|error| format!("failed to wait for Claude: {error}")),
            }
        }
    };

    if let Err(error) = wait_result {
        let _ = child.kill().await;
        stdout_handle.abort();
        stderr_handle.abort();
        return finish(&device_id, &job_id, -1, &error, &tx, &store);
    }

    let status = wait_result.expect("handled error above");
    let stdout_outcome = match stdout_handle.await {
        Ok(outcome) => outcome,
        Err(error) => ClaudeStdoutOutcome {
            error: Some(format!("Claude stdout task failed: {error}")),
        },
    };
    let _ = stderr_handle.await;

    if let Some(error) = stdout_outcome.error {
        return finish(&device_id, &job_id, -1, &error, &tx, &store);
    }
    if !status.success() {
        let tail = stderr_tail
            .lock()
            .ok()
            .map(|tail| tail.clone())
            .unwrap_or_default();
        let error = if tail.trim().is_empty() {
            format!("Claude exited with status {status}")
        } else {
            format!("Claude exited with status {status}: {}", tail.trim())
        };
        return finish(
            &device_id,
            &job_id,
            status.code().unwrap_or(-1),
            &error,
            &tx,
            &store,
        );
    }

    finish(&device_id, &job_id, 0, "", &tx, &store)
}

async fn spawn_claude(config: &ClaudeCodeConfig) -> Result<tokio::process::Child, String> {
    let mut cmd = Command::new(&config.executable);
    cmd.arg("-p")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--input-format")
        .arg("stream-json")
        .arg("--verbose")
        .arg("--strict-mcp-config")
        .arg("--disallowedTools")
        .arg("AskUserQuestion")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    if let Some(model) = &config.model {
        cmd.arg("--model").arg(model);
    }
    if let Some(max_turns) = &config.max_turns {
        cmd.arg("--max-turns").arg(max_turns);
    }
    if let Some(system_prompt) = &config.system_prompt {
        cmd.arg("--append-system-prompt").arg(system_prompt);
    }
    if let Some(session_id) = &config.session_id {
        cmd.arg("--resume").arg(session_id);
    }
    if let Some(permission_mode) = &config.permission_mode {
        cmd.arg("--permission-mode").arg(permission_mode);
    }

    if !config.cwd.is_empty() {
        cmd.current_dir(&config.cwd);
    }
    for (key, value) in &config.env {
        if is_filtered_claude_env(key) {
            continue;
        }
        cmd.env(key, value);
    }

    cmd.spawn()
        .map_err(|error| format!("failed to spawn Claude Code: {error}"))
}

fn is_filtered_claude_env(key: &str) -> bool {
    key == "CLAUDECODE" || key.starts_with("CLAUDECODE_") || key.starts_with("CLAUDE_CODE_")
}

fn prepare_context(
    config: &ClaudeCodeConfig,
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
    let primary = cwd.join("CLAUDE.md");
    let path = if primary.exists() {
        cwd.join("CLAUDE.ahand.md")
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
        .map_err(|error| format!("failed to write Claude context {}: {error}", path.display()))?;

    append_json_line(
        store,
        job_id,
        "context.jsonl",
        &json!({
            "kind": "claude_context_file",
            "path": path.display().to_string(),
            "source": INSTRUCTIONS_ENV,
        }),
    );
    Ok(())
}

fn user_message(prompt: &str) -> Value {
    json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [
                {
                    "type": "text",
                    "text": prompt,
                }
            ],
        },
    })
}

struct ClaudeStdoutOutcome {
    error: Option<String>,
}

async fn read_stdout<T>(
    stdout: tokio::process::ChildStdout,
    mut formatter: ClaudeCodeFormatter,
    tx: T,
    store: Option<Arc<RunStore>>,
    device_id: String,
    job_id: String,
) -> ClaudeStdoutOutcome
where
    T: EnvelopeSink,
{
    let mut lines = BufReader::new(stdout).lines();
    let mut error = None;
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if let Some(store) = &store {
                    let mut raw = line.as_bytes().to_vec();
                    raw.push(b'\n');
                    store.append_artifact(&job_id, "claude-events.jsonl", &raw);
                }
                let records = formatter.push_line(line.as_bytes());
                if let Some(result_error) = formatter.result_error.clone() {
                    error = Some(result_error);
                }
                emit_records(&device_id, &job_id, &tx, &store, records);
            }
            Ok(None) => break,
            Err(read_error) => {
                warn!(job_id = %job_id, error = %read_error, "failed to read Claude stdout");
                error = Some(format!("failed to read Claude stdout: {read_error}"));
                break;
            }
        }
    }
    emit_records(&device_id, &job_id, &tx, &store, formatter.finish_records());
    if let Some(store) = &store {
        store.write_json_artifact(
            &job_id,
            "claude-result.json",
            &json!({
                "sessionId": formatter.session_id,
                "usage": formatter.usage,
                "isError": formatter.result_error.is_some(),
                "error": formatter.result_error,
            }),
        );
    }
    ClaudeStdoutOutcome { error }
}

async fn read_stderr<T>(
    stderr: tokio::process::ChildStderr,
    tx: T,
    store: Option<Arc<RunStore>>,
    device_id: String,
    job_id: String,
    stderr_tail: Arc<Mutex<String>>,
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
                    && let Ok(mut tail) = stderr_tail.lock()
                {
                    tail.push_str(text);
                    if tail.len() > 8192 {
                        let keep_from = tail.len() - 8192;
                        tail.drain(..keep_from);
                    }
                }
                let _ = tx.send(make_event_envelope(
                    &device_id,
                    &job_id,
                    None,
                    Some(chunk.to_vec()),
                ));
            }
            Err(error) => {
                warn!(job_id = %job_id, error = %error, "failed to read Claude stderr");
                break;
            }
        }
    }
}

struct ClaudeCodeFormatter {
    job_id: String,
    agent_id: String,
    session_id: Option<String>,
    model: Option<String>,
    runtime: RuntimeContext,
    seq: u64,
    usage: HashMap<String, TokenUsage>,
    result_error: Option<String>,
}

#[derive(Clone)]
struct RuntimeContext {
    execution_mode: &'static str,
    result_parser: String,
    output_format: String,
    cwd: String,
    tool: String,
    args: Vec<String>,
}

#[derive(Default, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TokenUsage {
    input_tokens: u64,
    output_tokens: u64,
    cached_read_tokens: u64,
    cached_write_tokens: u64,
}

impl ClaudeCodeFormatter {
    fn new(req: &JobRequest, config: &ClaudeCodeConfig) -> Self {
        let job_id = req.job_id.clone();
        Self {
            agent_id: format!("{job_id}:claude-code"),
            job_id,
            session_id: config.session_id.clone(),
            model: config.model.clone(),
            runtime: RuntimeContext {
                execution_mode: execution_mode_name(ahand_protocol::resolve_job_execution_mode(
                    req,
                )),
                result_parser: ahand_protocol::resolve_job_result_parser(req).to_string(),
                output_format: ahand_protocol::resolve_job_output_format(req).to_string(),
                cwd: req.cwd.clone(),
                tool: config.executable.clone(),
                args: vec![
                    "-p".to_string(),
                    "--output-format".to_string(),
                    "stream-json".to_string(),
                    "--input-format".to_string(),
                    "stream-json".to_string(),
                ],
            },
            seq: 0,
            usage: HashMap::new(),
            result_error: None,
        }
    }

    fn push_line(&mut self, line: &[u8]) -> Vec<Value> {
        if line.iter().all(u8::is_ascii_whitespace) {
            return Vec::new();
        }
        match serde_json::from_slice::<Value>(line) {
            Ok(raw) => self.format_event(raw),
            Err(error) => vec![self.record(
                "parse_error",
                json!({
                    "message": error.to_string(),
                    "line": String::from_utf8_lossy(line),
                }),
                json!({
                    "source": "stdout",
                    "parser": "claude-stream-json",
                    "parserVersion": 1,
                    "line": String::from_utf8_lossy(line),
                    "parseError": error.to_string(),
                }),
            )],
        }
    }

    fn format_event(&mut self, raw: Value) -> Vec<Value> {
        let event_type = raw.get("type").and_then(Value::as_str).unwrap_or("");
        match event_type {
            "system" => self.format_system(raw),
            "assistant" => self.format_assistant(raw),
            "user" => self.format_user(raw),
            "result" => self.format_result(raw),
            "log" => vec![self.record("status", log_payload(&raw), self.raw(raw))],
            _ => vec![self.record("raw", json!({}), self.raw(raw))],
        }
    }

    fn format_system(&mut self, raw: Value) -> Vec<Value> {
        if let Some(session_id) = raw.get("session_id").and_then(Value::as_str) {
            self.session_id = Some(session_id.to_string());
        }
        vec![
            self.record("agent_session", json!({}), self.raw(raw.clone())),
            self.record(
                "status",
                json!({
                    "status": "running",
                    "subtype": raw.get("subtype").and_then(Value::as_str),
                }),
                self.raw(raw),
            ),
        ]
    }

    fn format_assistant(&mut self, raw: Value) -> Vec<Value> {
        let message = raw.get("message").unwrap_or(&raw);
        if let Some(model) = message.get("model").and_then(Value::as_str) {
            self.model = Some(model.to_string());
        }
        let mut records = Vec::new();
        if let Some(items) = message.get("content").and_then(Value::as_array) {
            for item in items {
                match item.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text" => records.push(self.record(
                        "llm_call_delta",
                        json!({
                            "responseText": item.get("text").and_then(Value::as_str).unwrap_or(""),
                        }),
                        self.raw(raw.clone()),
                    )),
                    "thinking" => records.push(self.record(
                        "llm_call_delta",
                        json!({
                            "channel": "thinking",
                            "responseText": item.get("text").and_then(Value::as_str).unwrap_or(""),
                        }),
                        self.raw(raw.clone()),
                    )),
                    "tool_use" => records.push(self.record(
                        "tool_call_start",
                        json!({
                            "toolCallId": item.get("id").and_then(Value::as_str).unwrap_or(""),
                            "toolName": item.get("name").and_then(Value::as_str).unwrap_or(""),
                            "toolKind": "claude-code",
                            "input": item.get("input").cloned().unwrap_or(Value::Null),
                            "status": "started",
                        }),
                        self.raw(raw.clone()),
                    )),
                    _ => records.push(self.record("raw", json!({}), self.raw(raw.clone()))),
                }
            }
        }
        if let Some(usage) = message.get("usage") {
            self.add_usage(message.get("model").and_then(Value::as_str), usage);
            records.push(self.record(
                "llm_call_end",
                json!({ "usage": self.usage_json() }),
                self.raw(raw),
            ));
        }
        records
    }

    fn format_user(&mut self, raw: Value) -> Vec<Value> {
        let message = raw.get("message").unwrap_or(&raw);
        let mut records = Vec::new();
        if let Some(items) = message.get("content").and_then(Value::as_array) {
            for item in items {
                if item.get("type").and_then(Value::as_str) != Some("tool_result") {
                    records.push(self.record("raw", json!({}), self.raw(raw.clone())));
                    continue;
                }
                let is_error = item
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                records.push(self.record(
                    "tool_call_output",
                    json!({
                        "toolCallId": item.get("tool_use_id").and_then(Value::as_str).unwrap_or(""),
                        "toolName": "",
                        "toolKind": "claude-code",
                        "status": if is_error { "failed" } else { "completed" },
                        "outputText": content_to_text(item.get("content")),
                        "output": item.get("content").cloned().unwrap_or(Value::Null),
                    }),
                    self.raw(raw.clone()),
                ));
            }
        }
        records
    }

    fn format_result(&mut self, raw: Value) -> Vec<Value> {
        if let Some(session_id) = raw.get("session_id").and_then(Value::as_str) {
            self.session_id = Some(session_id.to_string());
        }
        let result_text = raw.get("result").and_then(Value::as_str).unwrap_or("");
        let is_error = raw
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut records = Vec::new();
        if !result_text.is_empty() {
            records.push(self.record(
                "llm_call_delta",
                json!({ "responseText": result_text }),
                self.raw(raw.clone()),
            ));
        }
        let payload = json!({
            "usage": self.usage_json(),
            "durationMs": raw.get("duration_ms").and_then(Value::as_f64),
            "numTurns": raw.get("num_turns").and_then(Value::as_i64),
            "stopReason": raw.get("subtype").and_then(Value::as_str),
        });
        records.push(self.record("llm_call_end", payload, self.raw(raw.clone())));
        if is_error {
            let message = if result_text.is_empty() {
                "Claude Code result reported an error".to_string()
            } else {
                result_text.to_string()
            };
            self.result_error = Some(message.clone());
            records.push(self.record(
                "error",
                json!({
                    "message": message,
                    "isError": true,
                    "source": "result",
                }),
                self.raw(raw),
            ));
        }
        records
    }

    fn add_usage(&mut self, model: Option<&str>, usage: &Value) {
        let key = model
            .or(self.model.as_deref())
            .unwrap_or("unknown")
            .to_string();
        let entry = self.usage.entry(key).or_default();
        entry.input_tokens += usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        entry.output_tokens += usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        entry.cached_read_tokens += usage
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        entry.cached_write_tokens += usage
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
    }

    fn usage_json(&self) -> Value {
        let mut by_model = serde_json::Map::new();
        let mut total = TokenUsage::default();
        for (model, usage) in &self.usage {
            total.input_tokens += usage.input_tokens;
            total.output_tokens += usage.output_tokens;
            total.cached_read_tokens += usage.cached_read_tokens;
            total.cached_write_tokens += usage.cached_write_tokens;
            by_model.insert(model.clone(), json!(usage));
        }
        json!({
            "inputTokens": total.input_tokens,
            "outputTokens": total.output_tokens,
            "cachedReadTokens": total.cached_read_tokens,
            "cachedWriteTokens": total.cached_write_tokens,
            "byModel": by_model,
        })
    }

    fn finish_records(&mut self) -> Vec<Value> {
        if self.usage.is_empty() {
            Vec::new()
        } else {
            vec![self.record(
                "llm_call_end",
                json!({ "usage": self.usage_json() }),
                self.raw(json!({ "source": "finish" })),
            )]
        }
    }

    fn record(&mut self, kind: &str, payload: Value, raw: Value) -> Value {
        self.seq += 1;
        let mut record = json!({
            "schemaVersion": 1,
            "jobId": self.job_id,
            "seq": self.seq,
            "kind": kind,
            "agent": self.agent_json(),
            "time": {
                "observedAtMs": now_ms(),
            },
            "runtime": {
                "jobId": self.job_id,
                "executionMode": self.runtime.execution_mode,
                "resultParser": self.runtime.result_parser,
                "outputFormat": self.runtime.output_format,
                "cwd": self.runtime.cwd,
                "tool": self.runtime.tool,
                "args": self.runtime.args,
            },
            "raw": raw,
        });
        match kind {
            "llm_call_delta" | "llm_call_end" => record["llmResponse"] = payload,
            "tool_call_start" | "tool_call_output" | "tool_call_end" => {
                record["toolCall"] = payload;
            }
            "error" | "parse_error" => record["error"] = payload,
            "status" => record["status"] = payload,
            _ => {}
        }
        record
    }

    fn agent_json(&self) -> Value {
        let mut agent = json!({
            "agentId": self.agent_id,
            "agentKind": "claude-code",
            "model": {
                "provider": "anthropic",
                "id": self.model.as_deref().unwrap_or("unknown"),
            },
        });
        if let Some(session_id) = &self.session_id {
            agent["agentSessionId"] = json!(session_id);
        }
        agent
    }

    fn raw(&self, raw: Value) -> Value {
        json!({
            "source": "stdout",
            "parser": "claude-stream-json",
            "parserVersion": 1,
            "json": raw,
        })
    }
}

fn log_payload(raw: &Value) -> Value {
    let log = raw.get("log").unwrap_or(raw);
    json!({
        "status": "log",
        "level": log.get("level").and_then(Value::as_str).unwrap_or("info"),
        "message": log.get("message").and_then(Value::as_str).unwrap_or(""),
    })
}

fn content_to_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| {
                item.as_str()
                    .map(str::to_string)
                    .or_else(|| item.get("text").and_then(Value::as_str).map(str::to_string))
            })
            .collect::<Vec<_>>()
            .join(""),
        Some(other) => other.to_string(),
        None => String::new(),
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

fn execution_mode_name(mode: ahand_protocol::ExecutionMode) -> &'static str {
    match mode {
        ahand_protocol::ExecutionMode::Unspecified => "unspecified",
        ahand_protocol::ExecutionMode::Batch => "batch",
        ahand_protocol::ExecutionMode::Pty => "pty",
        ahand_protocol::ExecutionMode::PipeStream => "pipe_stream",
    }
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
    format!("claude-code-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ahand_protocol::ExecutionMode;

    fn req() -> JobRequest {
        JobRequest {
            job_id: "job-1".to_string(),
            tool: "/bin/claude".to_string(),
            cwd: "/repo".to_string(),
            env: HashMap::from([
                (
                    INPUT_FORMAT_ENV.to_string(),
                    ahand_protocol::INPUT_FORMAT_CLAUDE_STREAM_JSON.to_string(),
                ),
                (
                    OUTPUT_FORMAT_ENV.to_string(),
                    ahand_protocol::OUTPUT_FORMAT_CLAUDE_STREAM_JSON.to_string(),
                ),
                (PROMPT_ENV.to_string(), "hello".to_string()),
                (MODEL_ENV.to_string(), "claude-sonnet".to_string()),
            ]),
            execution_mode: ExecutionMode::PipeStream as i32,
            result_parser: ahand_protocol::RESULT_PARSER_CLAUDE_STREAM_JSON.to_string(),
            format: ahand_protocol::FORMAT_CLAUDE_CODE.to_string(),
            input_format: ahand_protocol::INPUT_FORMAT_CLAUDE_STREAM_JSON.to_string(),
            output_format: ahand_protocol::OUTPUT_FORMAT_CLAUDE_STREAM_JSON.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn recognizes_claude_code_jobs_by_explicit_env() {
        assert!(is_claude_code_job(&req()));
        let mut other = req();
        other.env.remove(INPUT_FORMAT_ENV);
        other.input_format.clear();
        assert!(!is_claude_code_job(&other));
    }

    #[test]
    fn formatter_maps_claude_stream_json() {
        let req = req();
        let config = ClaudeCodeConfig::from_job(&req).unwrap();
        let mut formatter = ClaudeCodeFormatter::new(&req, &config);

        let records =
            formatter.push_line(br#"{"type":"system","subtype":"init","session_id":"s-1"}"#);
        assert_eq!(records[0]["kind"], "agent_session");
        assert_eq!(records[0]["agent"]["agentSessionId"], "s-1");

        let records = formatter.push_line(
            br#"{"type":"assistant","message":{"role":"assistant","model":"claude-sonnet","content":[{"type":"text","text":"hi"},{"type":"thinking","text":"plan"},{"type":"tool_use","id":"toolu-1","name":"Bash","input":{"command":"ls"}}],"usage":{"input_tokens":2,"output_tokens":3,"cache_read_input_tokens":1,"cache_creation_input_tokens":4}}}"#,
        );
        let kinds = records
            .iter()
            .map(|record| record["kind"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                "llm_call_delta",
                "llm_call_delta",
                "tool_call_start",
                "llm_call_end"
            ]
        );
        assert_eq!(records[0]["llmResponse"]["responseText"], "hi");
        assert_eq!(records[1]["llmResponse"]["channel"], "thinking");
        assert_eq!(records[2]["toolCall"]["toolName"], "Bash");
        assert_eq!(records[3]["llmResponse"]["usage"]["inputTokens"], 2);

        let records = formatter.push_line(
            br#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu-1","content":"ok\n"}]}}"#,
        );
        assert_eq!(records[0]["kind"], "tool_call_output");
        assert_eq!(records[0]["toolCall"]["outputText"], "ok\n");

        let records = formatter.push_line(
            br#"{"type":"result","subtype":"success","session_id":"s-1","result":"done","is_error":false,"duration_ms":10,"num_turns":1}"#,
        );
        assert!(
            records
                .iter()
                .any(|record| record["kind"] == "llm_call_end")
        );
        assert!(formatter.result_error.is_none());
    }

    #[test]
    fn result_error_marks_formatter_error() {
        let req = req();
        let config = ClaudeCodeConfig::from_job(&req).unwrap();
        let mut formatter = ClaudeCodeFormatter::new(&req, &config);
        let records = formatter
            .push_line(br#"{"type":"result","subtype":"error","result":"bad","is_error":true}"#);
        assert_eq!(formatter.result_error.as_deref(), Some("bad"));
        assert!(records.iter().any(|record| record["kind"] == "error"));
    }

    #[tokio::test]
    async fn run_claude_code_with_fake_cli_emits_observations() {
        use prost::Message;
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("fake-claude");
        let mut file = std::fs::File::create(&claude).unwrap();
        writeln!(
            file,
            r#"#!/bin/sh
line="$(cat)"
case "$line" in
  *'"type":"user"'*|*'"type": "user"'*) ;;
  *) echo "missing user message" >&2; exit 2 ;;
esac
echo '{{"type":"system","subtype":"init","session_id":"s-1"}}'
echo '{{"type":"assistant","message":{{"role":"assistant","model":"claude-sonnet","content":[{{"type":"text","text":"hello"}}],"usage":{{"input_tokens":1,"output_tokens":2}}}}}}'
echo '{{"type":"result","subtype":"success","session_id":"s-1","result":"final","is_error":false,"duration_ms":1,"num_turns":1}}'
"#
        )
        .unwrap();
        drop(file);
        let mut perms = std::fs::metadata(&claude).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&claude, perms).unwrap();

        let mut req = req();
        req.tool = claude.to_string_lossy().to_string();
        req.cwd = dir.path().to_string_lossy().to_string();
        req.env.insert(EXECUTABLE_ENV.to_string(), req.tool.clone());

        let (tx, mut rx) = mpsc::unbounded_channel::<Envelope>();
        let (_cancel_tx, cancel_rx) = mpsc::channel(1);
        let (exit_code, error) =
            run_claude_code("device-1".to_string(), req, tx, cancel_rx, None).await;

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
        assert!(stdout.contains("hello"), "{stdout}");
        assert!(stdout.contains("final"), "{stdout}");
    }
}
