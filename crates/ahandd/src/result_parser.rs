use ahand_protocol::JobRequest;
use serde_json::{Value, json};

pub struct CodexFormatter {
    job_id: String,
    agent_id: String,
    thread_id: Option<String>,
    runtime: RuntimeContext,
    seq: u64,
    buffer: Vec<u8>,
}

#[derive(Clone)]
struct RuntimeContext {
    execution_mode: &'static str,
    result_parser: String,
    format: String,
    cwd: String,
    tool: String,
    args: Vec<String>,
}

impl CodexFormatter {
    pub fn maybe_new(req: &JobRequest) -> Option<Self> {
        let format = ahand_protocol::resolve_job_format(req);
        let parser = ahand_protocol::resolve_job_result_parser(req);
        if format != ahand_protocol::FORMAT_CODEX
            || parser != ahand_protocol::RESULT_PARSER_CODEX_JSONL
        {
            return None;
        }

        let job_id = req.job_id.clone();
        Some(Self {
            agent_id: format!("{job_id}:codex"),
            job_id,
            thread_id: None,
            runtime: RuntimeContext {
                execution_mode: execution_mode_name(ahand_protocol::resolve_job_execution_mode(
                    req,
                )),
                result_parser: parser.to_string(),
                format: format.to_string(),
                cwd: req.cwd.clone(),
                tool: req.tool.clone(),
                args: req.args.clone(),
            },
            seq: 0,
            buffer: Vec::new(),
        })
    }

    pub fn push_stdout(&mut self, chunk: &[u8]) -> Vec<Value> {
        self.buffer.extend_from_slice(chunk);
        let mut records = Vec::new();

        while let Some(pos) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let mut line = self.buffer.drain(..=pos).collect::<Vec<_>>();
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.push_line(&line, &mut records);
        }

        records
    }

    pub fn finish(&mut self) -> Vec<Value> {
        if self.buffer.is_empty() {
            return Vec::new();
        }
        let line = std::mem::take(&mut self.buffer);
        let mut records = Vec::new();
        self.push_line(&line, &mut records);
        records
    }

    fn push_line(&mut self, line: &[u8], records: &mut Vec<Value>) {
        if line.iter().all(u8::is_ascii_whitespace) {
            return;
        }

        match serde_json::from_slice::<Value>(line) {
            Ok(raw) => records.extend(self.format_event(raw)),
            Err(error) => records.push(self.record(
                "parse_error",
                json!({}),
                json!({
                    "message": error.to_string(),
                    "line": String::from_utf8_lossy(line),
                }),
                json!({
                    "source": "stdout",
                    "parser": "codex-jsonl",
                    "parserVersion": 1,
                    "line": String::from_utf8_lossy(line),
                    "parseError": error.to_string(),
                }),
            )),
        }
    }

    fn format_event(&mut self, raw: Value) -> Vec<Value> {
        let event_type = raw.get("type").and_then(Value::as_str).unwrap_or("");
        match event_type {
            "thread.started" => {
                if let Some(thread_id) = raw.get("thread_id").and_then(Value::as_str) {
                    self.thread_id = Some(thread_id.to_string());
                }
                vec![self.record("agent_session", json!({}), json!({}), self.raw(raw))]
            }
            "turn.started" => vec![self.record(
                "llm_call_start",
                json!({
                    "model": {
                        "provider": "openai",
                        "id": "unknown",
                    },
                    "messages": [],
                    "tools": [],
                    "availability": {
                        "messages": "unobserved",
                        "tools": "unobserved",
                    },
                }),
                json!({}),
                self.raw(raw),
            )],
            "turn.completed" => vec![self.record(
                "llm_call_end",
                json!({}),
                json!({
                    "usage": normalize_usage(raw.get("usage")),
                }),
                self.raw(raw),
            )],
            "item.started" => self.format_item(raw, true),
            "item.completed" => self.format_item(raw, false),
            "error" => vec![
                self.record(
                    "error",
                    json!({}),
                    json!({
                        "isError": true,
                    }),
                    self.raw(raw.clone()),
                )
                .with_error(
                    raw.get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("Codex error"),
                ),
            ],
            _ => vec![self.record("raw", json!({}), json!({}), self.raw(raw))],
        }
    }

    fn format_item(&mut self, raw: Value, started: bool) -> Vec<Value> {
        let Some(item) = raw.get("item") else {
            return vec![self.record("raw", json!({}), json!({}), self.raw(raw))];
        };
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");

        match (started, item_type) {
            (true, "command_execution") => vec![self.record(
                "tool_call_start",
                json!({}),
                tool_call_json(item, "started"),
                self.raw(raw),
            )],
            (false, "command_execution") => {
                let mut records = Vec::new();
                let output = item
                    .get("aggregated_output")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if !output.is_empty() {
                    records.push(self.record(
                        "tool_call_output",
                        json!({}),
                        tool_call_json(item, "completed"),
                        self.raw(raw.clone()),
                    ));
                }

                let status = if item.get("exit_code").and_then(Value::as_i64) == Some(0) {
                    "completed"
                } else {
                    "failed"
                };
                records.push(self.record(
                    "tool_call_end",
                    json!({}),
                    tool_call_json(item, status),
                    self.raw(raw),
                ));
                records
            }
            (false, "agent_message") => vec![self.record(
                "llm_call_delta",
                json!({}),
                json!({
                    "responseText": item.get("text").and_then(Value::as_str).unwrap_or(""),
                }),
                self.raw(raw),
            )],
            _ => vec![self.record("raw", json!({}), json!({}), self.raw(raw))],
        }
    }

    fn record(&mut self, kind: &str, llm_request: Value, payload: Value, raw: Value) -> Value {
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
                "format": self.runtime.format,
                "cwd": self.runtime.cwd,
                "tool": self.runtime.tool,
                "args": self.runtime.args,
            },
            "raw": raw,
        });

        if !llm_request
            .as_object()
            .is_none_or(serde_json::Map::is_empty)
        {
            record["llmRequest"] = llm_request;
        }

        match kind {
            "llm_call_delta" | "llm_call_end" => record["llmResponse"] = payload,
            "tool_call_start" | "tool_call_output" | "tool_call_end" => {
                record["toolCall"] = payload;
            }
            "parse_error" => record["error"] = payload,
            _ => {}
        }

        record
    }

    fn agent_json(&self) -> Value {
        let mut agent = json!({
            "agentId": self.agent_id,
            "agentKind": "codex",
            "model": {
                "provider": "openai",
                "id": "unknown",
            },
        });
        if let Some(thread_id) = &self.thread_id {
            agent["agentThreadId"] = json!(thread_id);
        }
        agent
    }

    fn raw(&self, raw: Value) -> Value {
        json!({
            "source": "stdout",
            "parser": "codex-jsonl",
            "parserVersion": 1,
            "json": raw,
        })
    }
}

trait WithError {
    fn with_error(self, message: &str) -> Self;
}

impl WithError for Value {
    fn with_error(mut self, message: &str) -> Self {
        self["error"] = json!({ "message": message });
        self
    }
}

fn tool_call_json(item: &Value, status: &str) -> Value {
    let command = item.get("command").and_then(Value::as_str).unwrap_or("");
    let mut tool_call = json!({
        "toolCallId": item.get("id").and_then(Value::as_str).unwrap_or(""),
        "toolName": command,
        "toolKind": "shell",
        "input": {
            "command": command,
        },
        "status": status,
    });

    if let Some(output) = item.get("aggregated_output").and_then(Value::as_str)
        && !output.is_empty()
    {
        tool_call["outputText"] = json!(output);
    }

    if let Some(exit_code) = item.get("exit_code").and_then(Value::as_i64) {
        tool_call["exitCode"] = json!(exit_code);
    }

    tool_call
}

fn normalize_usage(usage: Option<&Value>) -> Value {
    let Some(usage) = usage else {
        return json!({});
    };

    json!({
        "inputTokens": usage.get("input_tokens").and_then(Value::as_u64),
        "cachedInputTokens": usage.get("cached_input_tokens").and_then(Value::as_u64),
        "outputTokens": usage.get("output_tokens").and_then(Value::as_u64),
        "reasoningOutputTokens": usage.get("reasoning_output_tokens").and_then(Value::as_u64),
    })
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

#[cfg(test)]
mod tests {
    use super::CodexFormatter;
    use ahand_protocol::{ExecutionMode, JobRequest};

    fn req(format: &str, parser: &str) -> JobRequest {
        JobRequest {
            job_id: "job-1".to_string(),
            tool: "codex".to_string(),
            args: vec!["exec".to_string(), "--json".to_string()],
            cwd: "/repo".to_string(),
            execution_mode: ExecutionMode::PipeStream as i32,
            result_parser: parser.to_string(),
            format: format.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn codex_formatter_is_opt_in() {
        assert!(CodexFormatter::maybe_new(&req("raw", "codex-jsonl")).is_none());
        assert!(CodexFormatter::maybe_new(&req("codex", "raw")).is_none());
        assert!(CodexFormatter::maybe_new(&req("codex", "codex-jsonl")).is_some());
    }

    #[test]
    fn parses_codex_jsonl_into_observations() {
        let mut formatter = CodexFormatter::maybe_new(&req("codex", "codex-jsonl")).unwrap();
        let input = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"thread-1\"}\n",
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"item.completed\",\"item\":{\"id\":\"item-1\",\"type\":\"agent_message\",\"text\":\"hello\"}}\n",
            "{\"type\":\"item.started\",\"item\":{\"id\":\"item-2\",\"type\":\"command_execution\",\"command\":\"git status\",\"aggregated_output\":\"\",\"exit_code\":null,\"status\":\"in_progress\"}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"id\":\"item-2\",\"type\":\"command_execution\",\"command\":\"git status\",\"aggregated_output\":\"clean\\n\",\"exit_code\":0,\"status\":\"completed\"}}\n",
            "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":1,\"cached_input_tokens\":2,\"output_tokens\":3,\"reasoning_output_tokens\":4}}\n",
        );

        let records = formatter.push_stdout(input.as_bytes());
        let kinds = records
            .iter()
            .map(|record| record["kind"].as_str().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(
            kinds,
            vec![
                "agent_session",
                "llm_call_start",
                "llm_call_delta",
                "tool_call_start",
                "tool_call_output",
                "tool_call_end",
                "llm_call_end",
            ]
        );
        assert_eq!(records[0]["agent"]["agentThreadId"], "thread-1");
        assert_eq!(records[2]["llmResponse"]["responseText"], "hello");
        assert_eq!(records[4]["toolCall"]["outputText"], "clean\n");
        assert_eq!(records[6]["llmResponse"]["usage"]["inputTokens"], 1);
    }

    #[test]
    fn handles_chunk_boundaries_and_final_line_without_newline() {
        let mut formatter = CodexFormatter::maybe_new(&req("codex", "codex-jsonl")).unwrap();
        let records = formatter.push_stdout(b"{\"type\":\"turn.started\"}\n{\"type\":\"item.com");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["kind"], "llm_call_start");

        let records = formatter.push_stdout(
            b"pleted\",\"item\":{\"id\":\"item-1\",\"type\":\"agent_message\",\"text\":\"ok\"}}",
        );
        assert!(records.is_empty());

        let records = formatter.finish();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["kind"], "llm_call_delta");
        assert_eq!(records[0]["llmResponse"]["responseText"], "ok");
    }

    #[test]
    fn parse_error_becomes_observation() {
        let mut formatter = CodexFormatter::maybe_new(&req("codex", "codex-jsonl")).unwrap();
        let records = formatter.push_stdout(b"not-json\n");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["kind"], "parse_error");
        assert!(records[0]["error"]["message"].as_str().is_some());
    }
}
