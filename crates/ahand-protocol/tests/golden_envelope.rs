//! Wire-format golden fixtures for `ahand_protocol::Envelope`.
//!
//! For every variant of `envelope::Payload`, this file:
//!
//!   1. Builds a fully-populated `Envelope` from hand-picked, stable values.
//!   2. Encodes it with prost.
//!   3. Compares the bytes against a `.bin` fixture under `tests/fixtures/`.
//!   4. Round-trips: `decode(fixture) → re-encode → fixture` (catches
//!      non-canonical encodings like map iteration order drift).
//!
//! The `.bin` files are committed to the repository. On a normal checkout
//! the helper just reads them; a missing file is treated as a hard error
//! (do **not** silently regenerate — see the panic message in
//! `assert_golden` for why). When a proto change is **intentional**,
//! regenerate with:
//!
//!     AHAND_FIXTURE_REGENERATE=1 cargo test -p ahand-protocol --test golden_envelope
//!
//! See `tests/fixtures/README.md` for the full workflow.
//!
//! Exhaustiveness: [`payload_fixture_name`] matches every arm of
//! `envelope::Payload`. Adding a new variant to `envelope.proto` regenerates
//! the enum, the match becomes non-exhaustive, and this file fails to compile
//! — forcing whoever bumped the proto to add a fixture too. The companion
//! `every_payload_variant_has_a_fixture_file` test then checks that every
//! mapped name has a `.bin` file on disk, so the compile-time lock can't be
//! satisfied by adding an arm without writing the matching golden.

use ahand_protocol::{
    ApprovalRequest, ApprovalResponse, BootstrapAuth, BrowserRequest, BrowserResponse, CancelJob,
    Ed25519Auth, Envelope, Heartbeat, Hello, HelloAccepted, HelloChallenge, JobEvent, JobFinished,
    JobRejected, JobRequest, PolicyQuery, PolicyState, PolicyUpdate, RefusalContext, SessionMode,
    SessionQuery, SessionState, SetSessionMode, StdinChunk, TerminalResize, UpdateCommand,
    UpdateState, UpdateStatus, UpdateSuggestion, envelope, hello, job_event,
};
use prost::Message;
use std::path::{Path, PathBuf};

// Stable values used in every fixture so the encoded bytes change only when
// the wire format actually changes — not when a developer tweaks an unrelated
// constant.
const FX_DEVICE_ID: &str = "device-golden";
const FX_TRACE_ID: &str = "trace-golden";
const FX_MSG_ID: &str = "msg-golden";
const FX_SEQ: u64 = 7;
const FX_ACK: u64 = 8;
const FX_TS_MS: u64 = 1_700_000_000_000;
const FX_JOB_ID: &str = "job-golden";

fn base_envelope(payload: envelope::Payload) -> Envelope {
    Envelope {
        device_id: FX_DEVICE_ID.into(),
        trace_id: FX_TRACE_ID.into(),
        msg_id: FX_MSG_ID.into(),
        seq: FX_SEQ,
        ack: FX_ACK,
        ts_ms: FX_TS_MS,
        payload: Some(payload),
    }
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn regenerate_enabled() -> bool {
    matches!(
        std::env::var("AHAND_FIXTURE_REGENERATE").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE")
    )
}

fn assert_golden(name: &str, env: &Envelope) {
    let encoded = env.encode_to_vec();
    let path = fixture_dir().join(format!("{name}.bin"));

    if regenerate_enabled() {
        std::fs::create_dir_all(path.parent().unwrap())
            .unwrap_or_else(|e| panic!("create fixtures dir: {e}"));
        std::fs::write(&path, &encoded)
            .unwrap_or_else(|e| panic!("write fixture {}: {e}", path.display()));
    } else if !path.exists() {
        // The `.bin` files are committed. A missing file means either:
        //   * a fresh variant was added but its fixture was never generated, OR
        //   * a developer accidentally deleted the file.
        // In both cases, silently regenerating from the current encoder would
        // canonicalise whatever shape the encoder happens to produce — including
        // a latent bug. Force the fix to be explicit and visible in `git status`.
        panic!(
            "fixture file `{}` is missing. \
             If a new envelope::Payload variant was just added, regenerate with: \
             `AHAND_FIXTURE_REGENERATE=1 cargo test -p ahand-protocol --test golden_envelope` \
             and review the new `.bin` file before committing.",
            path.display(),
        );
    }

    let golden =
        std::fs::read(&path).unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()));

    assert_eq!(
        encoded,
        golden,
        "wire-format drift in `{name}` envelope (golden = {} bytes, encoded = {} bytes). \
         If this change is intentional, regenerate fixtures with: \
         AHAND_FIXTURE_REGENERATE=1 cargo test -p ahand-protocol --test golden_envelope",
        golden.len(),
        encoded.len(),
    );

    // Decode → re-encode round trip. This catches non-canonical encodings
    // (e.g. HashMap iteration order) that would otherwise let the same
    // logical message produce two different byte strings.
    let decoded = Envelope::decode(golden.as_slice())
        .unwrap_or_else(|e| panic!("decode `{name}` fixture: {e}"));
    let re_encoded = decoded.encode_to_vec();
    assert_eq!(
        re_encoded, golden,
        "decode→re-encode is not byte-stable for `{name}`. \
         Likely cause: a field uses a non-deterministic container (e.g. HashMap \
         with multiple entries). Either reduce the fixture to one entry or switch \
         the proto field to a deterministic representation."
    );
}

// ── Fixtures, one per envelope::Payload variant ─────────────────────────

#[test]
fn golden_hello_challenge() {
    let env = base_envelope(envelope::Payload::HelloChallenge(HelloChallenge {
        nonce: vec![0x42; 16],
        issued_at_ms: FX_TS_MS,
    }));
    assert_golden("hello_challenge", &env);
}

#[test]
fn golden_hello_ed25519() {
    let env = base_envelope(envelope::Payload::Hello(Hello {
        version: "0.1.2".into(),
        hostname: "goldenhost".into(),
        os: "linux".into(),
        capabilities: vec!["exec".into(), "browser".into()],
        last_ack: 7,
        auth: Some(hello::Auth::Ed25519(Ed25519Auth {
            public_key: vec![0x01; 32],
            signature: vec![0x02; 64],
            signed_at_ms: 1_700_000_000_123,
        })),
    }));
    assert_golden("hello_ed25519", &env);
}

#[test]
fn golden_hello_bootstrap() {
    let env = base_envelope(envelope::Payload::Hello(Hello {
        version: "0.1.2".into(),
        hostname: "goldenhost".into(),
        os: "linux".into(),
        capabilities: vec!["exec".into()],
        last_ack: 7,
        auth: Some(hello::Auth::Bootstrap(BootstrapAuth {
            bearer_token: "bootstrap-golden".into(),
            public_key: vec![0x03; 32],
            signature: vec![0x04; 64],
            signed_at_ms: 1_700_000_000_456,
        })),
    }));
    assert_golden("hello_bootstrap", &env);
}

#[test]
fn golden_hello_accepted() {
    let env = base_envelope(envelope::Payload::HelloAccepted(HelloAccepted {
        auth_method: "ed25519".into(),
        update_suggestion: Some(UpdateSuggestion {
            update_id: "upd-1".into(),
            target_version: "0.2.0".into(),
            download_url: "https://releases.example/aHand-0.2.0.tar.gz".into(),
            checksum_sha256: "deadbeef".into(),
            signature: vec![0x05; 64],
            release_notes: "bug fixes".into(),
        }),
    }));
    assert_golden("hello_accepted", &env);
}

#[test]
fn golden_job_request() {
    // NOTE: `env` is `map<string,string>` which prost models as a `HashMap`.
    // We deliberately use a single entry — multi-entry maps have
    // non-deterministic iteration order and would break byte-equality.
    let mut env_map = std::collections::HashMap::new();
    env_map.insert("PATH".to_string(), "/usr/bin:/bin".to_string());

    let env = base_envelope(envelope::Payload::JobRequest(JobRequest {
        job_id: FX_JOB_ID.into(),
        tool: "git".into(),
        args: vec!["status".into(), "-sb".into()],
        cwd: "/tmp/repo".into(),
        env: env_map,
        timeout_ms: 30_000,
        interactive: false,
    }));
    assert_golden("job_request", &env);
}

#[test]
fn golden_job_event_stdout() {
    let env = base_envelope(envelope::Payload::JobEvent(JobEvent {
        job_id: FX_JOB_ID.into(),
        event: Some(job_event::Event::StdoutChunk(b"hello\n".to_vec())),
    }));
    assert_golden("job_event_stdout", &env);
}

#[test]
fn golden_job_event_stderr() {
    let env = base_envelope(envelope::Payload::JobEvent(JobEvent {
        job_id: FX_JOB_ID.into(),
        event: Some(job_event::Event::StderrChunk(b"oops\n".to_vec())),
    }));
    assert_golden("job_event_stderr", &env);
}

#[test]
fn golden_job_event_progress() {
    let env = base_envelope(envelope::Payload::JobEvent(JobEvent {
        job_id: FX_JOB_ID.into(),
        event: Some(job_event::Event::Progress(42)),
    }));
    assert_golden("job_event_progress", &env);
}

#[test]
fn golden_job_event_unset() {
    // The inner `event` oneof is optional. Lock down the wire shape of an
    // unset oneof so a future change that makes it required (or that adds
    // a default-non-zero variant) is caught.
    let env = base_envelope(envelope::Payload::JobEvent(JobEvent {
        job_id: FX_JOB_ID.into(),
        event: None,
    }));
    assert_golden("job_event_unset", &env);
}

#[test]
fn golden_job_finished() {
    let env = base_envelope(envelope::Payload::JobFinished(JobFinished {
        job_id: FX_JOB_ID.into(),
        exit_code: 0,
        error: String::new(),
    }));
    assert_golden("job_finished", &env);
}

#[test]
fn golden_job_rejected() {
    let env = base_envelope(envelope::Payload::JobRejected(JobRejected {
        job_id: FX_JOB_ID.into(),
        reason: "session in strict mode".into(),
    }));
    assert_golden("job_rejected", &env);
}

#[test]
fn golden_cancel_job() {
    let env = base_envelope(envelope::Payload::CancelJob(CancelJob {
        job_id: FX_JOB_ID.into(),
    }));
    assert_golden("cancel_job", &env);
}

#[test]
fn golden_approval_request() {
    let env = base_envelope(envelope::Payload::ApprovalRequest(ApprovalRequest {
        job_id: FX_JOB_ID.into(),
        tool: "git".into(),
        args: vec!["push".into(), "origin".into(), "main".into()],
        cwd: "/tmp/repo".into(),
        reason: "strict mode requires approval".into(),
        detected_domains: vec!["github.com".into()],
        expires_ms: 1_700_000_060_000,
        caller_uid: "cloud".into(),
        previous_refusals: vec![RefusalContext {
            tool: "rm".into(),
            reason: "too risky".into(),
            refused_at_ms: 1_699_999_900_000,
        }],
    }));
    assert_golden("approval_request", &env);
}

#[test]
fn golden_approval_response() {
    let env = base_envelope(envelope::Payload::ApprovalResponse(ApprovalResponse {
        job_id: FX_JOB_ID.into(),
        approved: true,
        remember: false,
        reason: String::new(),
    }));
    assert_golden("approval_response", &env);
}

#[test]
fn golden_policy_query() {
    let env = base_envelope(envelope::Payload::PolicyQuery(PolicyQuery {}));
    assert_golden("policy_query", &env);
}

#[test]
fn golden_policy_state() {
    let env = base_envelope(envelope::Payload::PolicyState(PolicyState {
        allowed_tools: vec!["git".into(), "rg".into()],
        denied_tools: vec!["rm".into()],
        denied_paths: vec!["/etc".into()],
        allowed_domains: vec!["github.com".into()],
        approval_timeout_secs: 60,
    }));
    assert_golden("policy_state", &env);
}

#[test]
fn golden_policy_update() {
    let env = base_envelope(envelope::Payload::PolicyUpdate(PolicyUpdate {
        add_allowed_tools: vec!["jq".into()],
        remove_allowed_tools: vec!["sed".into()],
        add_denied_tools: vec!["curl".into()],
        remove_denied_tools: vec!["wget".into()],
        add_allowed_domains: vec!["example.com".into()],
        remove_allowed_domains: vec!["evil.example".into()],
        add_denied_paths: vec!["/var".into()],
        remove_denied_paths: vec!["/opt".into()],
        approval_timeout_secs: 120,
    }));
    assert_golden("policy_update", &env);
}

#[test]
fn golden_set_session_mode() {
    let env = base_envelope(envelope::Payload::SetSessionMode(SetSessionMode {
        caller_uid: "uid:501".into(),
        mode: SessionMode::Strict as i32,
        trust_timeout_mins: 30,
    }));
    assert_golden("set_session_mode", &env);
}

#[test]
fn golden_session_state() {
    let env = base_envelope(envelope::Payload::SessionState(SessionState {
        caller_uid: "uid:501".into(),
        mode: SessionMode::Trust as i32,
        trust_expires_ms: 1_700_003_600_000,
        trust_timeout_mins: 60,
    }));
    assert_golden("session_state", &env);
}

#[test]
fn golden_session_query() {
    let env = base_envelope(envelope::Payload::SessionQuery(SessionQuery {
        caller_uid: "uid:501".into(),
    }));
    assert_golden("session_query", &env);
}

#[test]
fn golden_browser_request() {
    let env = base_envelope(envelope::Payload::BrowserRequest(BrowserRequest {
        request_id: "req-1".into(),
        session_id: "sess-1".into(),
        action: "open".into(),
        params_json: "{\"url\":\"https://example.com\"}".into(),
        timeout_ms: 30_000,
    }));
    assert_golden("browser_request", &env);
}

#[test]
fn golden_browser_response() {
    let env = base_envelope(envelope::Payload::BrowserResponse(BrowserResponse {
        request_id: "req-1".into(),
        session_id: "sess-1".into(),
        success: true,
        result_json: "{\"ok\":true}".into(),
        error: String::new(),
        binary_data: vec![0xff, 0xd8, 0xff, 0xe0],
        binary_mime: "image/jpeg".into(),
    }));
    assert_golden("browser_response", &env);
}

#[test]
fn golden_update_command() {
    let env = base_envelope(envelope::Payload::UpdateCommand(UpdateCommand {
        update_id: "upd-1".into(),
        target_version: "0.2.0".into(),
        download_url: "https://releases.example/aHand-0.2.0.tar.gz".into(),
        checksum_sha256: "deadbeef".into(),
        signature: vec![0x06; 64],
        max_retries: 3,
    }));
    assert_golden("update_command", &env);
}

#[test]
fn golden_update_status() {
    let env = base_envelope(envelope::Payload::UpdateStatus(UpdateStatus {
        update_id: "upd-1".into(),
        state: UpdateState::Downloading as i32,
        current_version: "0.1.0".into(),
        target_version: "0.2.0".into(),
        progress: 42,
        error: String::new(),
    }));
    assert_golden("update_status", &env);
}

#[test]
fn golden_stdin_chunk() {
    let env = base_envelope(envelope::Payload::StdinChunk(StdinChunk {
        job_id: FX_JOB_ID.into(),
        data: b"input\n".to_vec(),
    }));
    assert_golden("stdin_chunk", &env);
}

#[test]
fn golden_terminal_resize() {
    let env = base_envelope(envelope::Payload::TerminalResize(TerminalResize {
        job_id: FX_JOB_ID.into(),
        cols: 80,
        rows: 24,
    }));
    assert_golden("terminal_resize", &env);
}

#[test]
fn golden_heartbeat() {
    let env = base_envelope(envelope::Payload::Heartbeat(Heartbeat {
        sent_at_ms: FX_TS_MS,
        daemon_version: "0.1.2".into(),
    }));
    assert_golden("heartbeat", &env);
}

// ── Exhaustiveness lock ─────────────────────────────────────────────────
//
// Every arm of `envelope::Payload` must map to a fixture name AND that
// `.bin` file must exist on disk. Two layers, on purpose:
//
//   * The exhaustive `match` below is checked at *compile time*. If a new
//     variant is added to `envelope.proto`, prost regenerates `Payload`,
//     the match becomes non-exhaustive, and this file fails to compile.
//   * `every_payload_variant_has_a_fixture_file` is checked at *runtime*.
//     If someone adds a match arm without writing the matching `golden_*`
//     test (so the `.bin` was never produced), the test fails with a
//     pointer to the regeneration command.
//
// Together they prevent both "new variant on the wire" and "match arm
// without a fixture" from slipping in. See `tests/fixtures/README.md` for
// the full add-a-variant workflow.
fn payload_fixture_name(p: &envelope::Payload) -> &'static str {
    use envelope::Payload::*;
    match p {
        HelloChallenge(_) => "hello_challenge",
        Hello(_) => "hello_ed25519",
        JobRequest(_) => "job_request",
        JobEvent(_) => "job_event_stdout",
        JobFinished(_) => "job_finished",
        JobRejected(_) => "job_rejected",
        CancelJob(_) => "cancel_job",
        ApprovalRequest(_) => "approval_request",
        ApprovalResponse(_) => "approval_response",
        PolicyQuery(_) => "policy_query",
        PolicyState(_) => "policy_state",
        PolicyUpdate(_) => "policy_update",
        SetSessionMode(_) => "set_session_mode",
        SessionState(_) => "session_state",
        SessionQuery(_) => "session_query",
        BrowserRequest(_) => "browser_request",
        BrowserResponse(_) => "browser_response",
        HelloAccepted(_) => "hello_accepted",
        UpdateCommand(_) => "update_command",
        UpdateStatus(_) => "update_status",
        StdinChunk(_) => "stdin_chunk",
        TerminalResize(_) => "terminal_resize",
        Heartbeat(_) => "heartbeat",
    }
}

#[test]
fn every_payload_variant_has_a_fixture_file() {
    // In regeneration mode the per-variant `golden_*` tests are concurrently
    // writing the fixtures we'd be checking — there's a window where files
    // legitimately don't exist yet. The compile-time match is enough during
    // regeneration; runtime existence is what we verify on normal runs.
    if regenerate_enabled() {
        return;
    }

    // One probe per variant. The list is independent of `payload_fixture_name`
    // so reordering or renaming an arm there can't accidentally make this
    // test miss a variant.
    let probes: &[envelope::Payload] = &[
        envelope::Payload::HelloChallenge(HelloChallenge::default()),
        envelope::Payload::Hello(Hello::default()),
        envelope::Payload::JobRequest(JobRequest::default()),
        envelope::Payload::JobEvent(JobEvent::default()),
        envelope::Payload::JobFinished(JobFinished::default()),
        envelope::Payload::JobRejected(JobRejected::default()),
        envelope::Payload::CancelJob(CancelJob::default()),
        envelope::Payload::ApprovalRequest(ApprovalRequest::default()),
        envelope::Payload::ApprovalResponse(ApprovalResponse::default()),
        envelope::Payload::PolicyQuery(PolicyQuery {}),
        envelope::Payload::PolicyState(PolicyState::default()),
        envelope::Payload::PolicyUpdate(PolicyUpdate::default()),
        envelope::Payload::SetSessionMode(SetSessionMode::default()),
        envelope::Payload::SessionState(SessionState::default()),
        envelope::Payload::SessionQuery(SessionQuery::default()),
        envelope::Payload::BrowserRequest(BrowserRequest::default()),
        envelope::Payload::BrowserResponse(BrowserResponse::default()),
        envelope::Payload::HelloAccepted(HelloAccepted::default()),
        envelope::Payload::UpdateCommand(UpdateCommand::default()),
        envelope::Payload::UpdateStatus(UpdateStatus::default()),
        envelope::Payload::StdinChunk(StdinChunk::default()),
        envelope::Payload::TerminalResize(TerminalResize::default()),
        envelope::Payload::Heartbeat(Heartbeat::default()),
    ];

    let mut missing: Vec<String> = Vec::new();
    for probe in probes {
        let name = payload_fixture_name(probe);
        let path = fixture_dir().join(format!("{name}.bin"));
        if !path.exists() {
            missing.push(format!("  - {} (variant: {})", path.display(), name));
        }
    }

    assert!(
        missing.is_empty(),
        "envelope::Payload variants are missing fixture files:\n{}\n\
         Add a `golden_<variant>` test with stable values, then regenerate with: \
         `AHAND_FIXTURE_REGENERATE=1 cargo test -p ahand-protocol --test golden_envelope`",
        missing.join("\n"),
    );
}
