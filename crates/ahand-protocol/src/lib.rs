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
