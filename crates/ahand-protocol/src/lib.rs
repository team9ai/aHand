pub mod ahand {
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/ahand.v1.rs"));
    }
}

pub use ahand::v1::*;

pub const RESULT_PARSER_RAW: &str = "raw";
pub const RESULT_PARSER_CODEX_JSONL: &str = "codex-jsonl";
pub const RESULT_PARSER_CLAUDE_STREAM_JSON: &str = "claude-stream-json";

pub const FORMAT_RAW: &str = "raw";
pub const FORMAT_CODEX: &str = "codex";
pub const FORMAT_CLAUDE_CODE: &str = "claude-code";

pub const INPUT_FORMAT_RAW: &str = "raw";
pub const INPUT_FORMAT_TEXT: &str = "text";
pub const INPUT_FORMAT_CLAUDE_STREAM_JSON: &str = "claude-stream-json";
pub const INPUT_FORMAT_HERMES_ACP_JSON_RPC: &str = "hermes-acp-json-rpc";

pub const OUTPUT_FORMAT_RAW: &str = "raw";
pub const OUTPUT_FORMAT_CODEX_JSONL: &str = "codex-jsonl";
pub const OUTPUT_FORMAT_CLAUDE_STREAM_JSON: &str = "claude-stream-json";
pub const OUTPUT_FORMAT_HERMES_ACP_JSON_RPC: &str = "hermes-acp-json-rpc";

pub fn resolve_job_execution_mode(job: &JobRequest) -> ExecutionMode {
    match ExecutionMode::try_from(job.execution_mode).unwrap_or(ExecutionMode::Unspecified) {
        ExecutionMode::Unspecified => {
            if job.interactive {
                ExecutionMode::Pty
            } else {
                ExecutionMode::Batch
            }
        }
        mode => mode,
    }
}

pub fn resolve_job_result_parser(job: &JobRequest) -> &str {
    let parser = job.result_parser.trim();
    if parser.is_empty() {
        RESULT_PARSER_RAW
    } else {
        parser
    }
}

pub fn is_known_result_parser(parser: &str) -> bool {
    matches!(
        parser,
        RESULT_PARSER_RAW | RESULT_PARSER_CODEX_JSONL | RESULT_PARSER_CLAUDE_STREAM_JSON
    )
}

pub fn resolve_job_input_format(job: &JobRequest) -> &str {
    let format = job.input_format.trim();
    if format.is_empty() {
        INPUT_FORMAT_RAW
    } else {
        format
    }
}

pub fn is_known_input_format(format: &str) -> bool {
    matches!(
        format,
        INPUT_FORMAT_RAW
            | INPUT_FORMAT_TEXT
            | INPUT_FORMAT_CLAUDE_STREAM_JSON
            | INPUT_FORMAT_HERMES_ACP_JSON_RPC
    )
}

pub fn resolve_job_output_format(job: &JobRequest) -> &str {
    let output_format = job.output_format.trim();
    if !output_format.is_empty() {
        return output_format;
    }
    match resolve_job_format(job) {
        FORMAT_CODEX => OUTPUT_FORMAT_CODEX_JSONL,
        FORMAT_CLAUDE_CODE => OUTPUT_FORMAT_CLAUDE_STREAM_JSON,
        _ => OUTPUT_FORMAT_RAW,
    }
}

pub fn is_known_output_format(format: &str) -> bool {
    matches!(
        format,
        OUTPUT_FORMAT_RAW
            | OUTPUT_FORMAT_CODEX_JSONL
            | OUTPUT_FORMAT_CLAUDE_STREAM_JSON
            | OUTPUT_FORMAT_HERMES_ACP_JSON_RPC
    )
}

pub fn resolve_job_format(job: &JobRequest) -> &str {
    let format = job.format.trim();
    if format.is_empty() {
        FORMAT_RAW
    } else {
        format
    }
}

pub fn is_known_format(format: &str) -> bool {
    matches!(format, FORMAT_RAW | FORMAT_CODEX | FORMAT_CLAUDE_CODE)
}

pub fn execution_mode_interactive_compat(mode: ExecutionMode) -> bool {
    matches!(mode, ExecutionMode::Pty)
}

pub fn build_hello_auth_payload(
    device_id: &str,
    hello: &Hello,
    signed_at_ms: u64,
    challenge_nonce: &[u8],
) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(b"ahand-hub\0hello-auth\0");
    push_field(&mut payload, device_id.as_bytes());
    push_field(&mut payload, hello.version.as_bytes());
    push_field(&mut payload, hello.hostname.as_bytes());
    push_field(&mut payload, hello.os.as_bytes());
    payload.extend_from_slice(
        &u32::try_from(hello.capabilities.len())
            .expect("capability count should fit into u32")
            .to_le_bytes(),
    );
    for capability in &hello.capabilities {
        push_field(&mut payload, capability.as_bytes());
    }
    payload.extend_from_slice(&hello.last_ack.to_le_bytes());
    payload.extend_from_slice(&signed_at_ms.to_le_bytes());
    push_field(&mut payload, challenge_nonce);
    payload
}

fn push_field(payload: &mut Vec<u8>, value: &[u8]) {
    payload.extend_from_slice(
        &u32::try_from(value.len())
            .expect("field length should fit into u32")
            .to_le_bytes(),
    );
    payload.extend_from_slice(value);
}
