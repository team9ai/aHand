//! Integration tests for `DaemonHandle::register_app_tool` /
//! `unregister_app_tool` and the resulting `AppToolsUpdate` snapshots that
//! the daemon pushes to the hub over its WebSocket connection.
//!
//! Test matrix:
//!   * Initial empty snapshot sent right after Hello.
//!   * Registering a tool causes a snapshot with the new tool.
//!   * Unregistering removes the tool and increments the revision.
//!   * Tools registered before the connection is established arrive in the
//!     initial snapshot (or at least with strictly increasing revisions —
//!     see note in `registration_racing_connect_yields_monotonic_revisions`).
//!   * Reconnect after a hub-initiated close re-sends the snapshot.
//!
//! Also covers AppToolRequest dispatch (Task 5):
//!   * Happy path: result_json returned.
//!   * Unknown tool → TOOL_NOT_FOUND.
//!   * Invalid JSON args / non-object args → INVALID_ARGS.
//!   * Handler sleeping past clamped timeout → EXECUTION_TIMEOUT.
//!   * Panicking handler → HANDLER_PANIC; daemon survives.
//!   * 5 concurrent slow calls → exactly one CONCURRENCY_LIMIT.
//!   * Duplicate tool_call_id while running → ignored; after completion →
//!     cached response re-sent with identical payload.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ahand_protocol::app_tool_response;
use ahandd::{AppToolDef, AppToolHandler, DaemonConfig, DaemonStatus, spawn};
use serde_json::json;
use tempfile::TempDir;

mod mock_hub;

// ── helpers ──────────────────────────────────────────────────────────────────

fn echo_handler() -> AppToolHandler {
    Arc::new(|args| Box::pin(async move { Ok(json!({ "echo": args })) }))
}

fn demo_echo_def() -> AppToolDef {
    AppToolDef {
        name: "demo_echo".to_string(),
        description: "Echoes its arguments back".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "message": { "type": "string" }
            }
        }),
        requires_approval: false,
    }
}

/// Wait until the daemon reaches `Online`, then return the status receiver.
async fn wait_online(handle: &ahandd::DaemonHandle) {
    let mut status = handle.subscribe_status();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(*status.borrow(), DaemonStatus::Online { .. }) {
                break;
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .expect("daemon did not reach Online within 5s");
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// After the Hello handshake the daemon should push an initial `AppToolsUpdate`
/// with revision=0 and an empty tool list.  After `register_app_tool` is
/// called, a second snapshot with revision=1 and the new tool should arrive.
#[tokio::test]
async fn snapshot_sent_after_hello_and_on_register() {
    let mock = mock_hub::start_accepting().await;
    let tmp = TempDir::new().unwrap();
    let config = DaemonConfig::builder(mock.ws_url(), mock.valid_jwt(), tmp.path())
        .heartbeat_interval(Duration::from_secs(60)) // keep heartbeats out of the way
        .build();

    let handle = spawn(config).await.expect("spawn ok");
    wait_online(&handle).await;

    // The initial snapshot (revision 0, no tools) should arrive quickly.
    let updates = mock
        .wait_for_app_tools_updates(1, Duration::from_secs(5))
        .await
        .expect("initial AppToolsUpdate not received within 5s");

    let initial = &updates[0];
    assert_eq!(initial.revision, 0, "initial revision must be 0");
    assert!(
        initial.tools.is_empty(),
        "initial snapshot must have no tools"
    );

    // Register the demo_echo tool.
    handle
        .register_app_tool(demo_echo_def(), echo_handler())
        .await
        .expect("register_app_tool should succeed");

    // A second snapshot with revision=1 and the new tool should arrive.
    let updates = mock
        .wait_for_app_tools_updates(2, Duration::from_secs(5))
        .await
        .expect("second AppToolsUpdate (after register) not received within 5s");

    let after_register = &updates[1];
    assert_eq!(
        after_register.revision, 1,
        "revision after register must be 1"
    );
    assert_eq!(
        after_register.tools.len(),
        1,
        "snapshot must contain 1 tool"
    );

    let tool = &after_register.tools[0];
    assert_eq!(tool.name, "demo_echo");
    assert_eq!(tool.description, "Echoes its arguments back");
    assert!(
        !tool.input_schema_json.is_empty(),
        "input_schema_json must be populated"
    );
    assert!(!tool.requires_approval);

    handle.shutdown().await.expect("shutdown clean");
}

/// register then unregister: final snapshot must have revision=2 and be empty.
#[tokio::test]
async fn unregister_pushes_snapshot_without_tool() {
    let mock = mock_hub::start_accepting().await;
    let tmp = TempDir::new().unwrap();
    let config = DaemonConfig::builder(mock.ws_url(), mock.valid_jwt(), tmp.path())
        .heartbeat_interval(Duration::from_secs(60))
        .build();

    let handle = spawn(config).await.expect("spawn ok");
    wait_online(&handle).await;

    // Wait for initial snapshot.
    mock.wait_for_app_tools_updates(1, Duration::from_secs(5))
        .await
        .expect("initial snapshot not received within 5s");

    // Register.
    handle
        .register_app_tool(demo_echo_def(), echo_handler())
        .await
        .expect("register ok");

    // Wait for revision=1 snapshot.
    mock.wait_for_app_tools_updates(2, Duration::from_secs(5))
        .await
        .expect("snapshot after register not received within 5s");

    // Unregister.
    let existed = handle.unregister_app_tool("demo_echo").await;
    assert!(
        existed,
        "unregister_app_tool should return true for an existing tool"
    );

    // Wait for revision=2 snapshot (empty tools).
    let updates = mock
        .wait_for_app_tools_updates(3, Duration::from_secs(5))
        .await
        .expect("snapshot after unregister not received within 5s");

    let final_snap = &updates[2];
    assert_eq!(
        final_snap.revision, 2,
        "revision after unregister must be 2"
    );
    assert!(
        final_snap.tools.is_empty(),
        "snapshot after unregister must be empty"
    );

    handle.shutdown().await.expect("shutdown clean");
}

/// Registering immediately after `spawn` may race the first connection attempt.
/// Regardless of timing, all received revisions must be strictly increasing and
/// the last snapshot must contain `demo_echo`.
///
/// Note: Because `DaemonHandle` is only available after `spawn()` returns,
/// strictly-before-connect registration is not constructible via the public
/// API. Instead, this test registers immediately after spawn (which may
/// race with the first connection attempt) and verifies that:
///   - Each received revision is strictly increasing (no duplicate revisions).
///   - Every snapshot sent carries a consistent tools list.
///
/// If the tool was registered before the connection completed, the initial
/// snapshot will have revision=1 (not 0). If registered after, revisions will
/// be 0, then 1. Either way, revisions must be strictly increasing.
#[tokio::test]
async fn registration_racing_connect_yields_monotonic_revisions() {
    let mock = mock_hub::start_accepting().await;
    let tmp = TempDir::new().unwrap();
    let config = DaemonConfig::builder(mock.ws_url(), mock.valid_jwt(), tmp.path())
        .heartbeat_interval(Duration::from_secs(60))
        .build();

    let handle = spawn(config).await.expect("spawn ok");

    // Register immediately — may race connection, intentionally.
    handle
        .register_app_tool(demo_echo_def(), echo_handler())
        .await
        .expect("register ok");

    wait_online(&handle).await;

    // Poll until a snapshot containing demo_echo appears.
    let found_snap = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let updates = mock.captured_app_tools_updates();
            if let Some(snap) = updates
                .iter()
                .find(|u| u.tools.iter().any(|t| t.name == "demo_echo"))
            {
                return snap.clone();
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("snapshot containing demo_echo not received within 5s");

    assert_eq!(
        found_snap.tools.len(),
        1,
        "snapshot must contain the registered tool"
    );
    assert_eq!(found_snap.tools[0].name, "demo_echo");

    // Revisions across all received snapshots must be strictly increasing.
    let updates = mock.captured_app_tools_updates();
    for window in updates.windows(2) {
        assert!(
            window[1].revision > window[0].revision,
            "revisions must be strictly increasing: got {} then {}",
            window[0].revision,
            window[1].revision
        );
    }

    handle.shutdown().await.expect("shutdown clean");
}

/// After the mock hub drops the first connection, the daemon reconnects.
/// The new Hello handshake must be followed by a fresh `AppToolsUpdate`
/// snapshot containing the same tool (same content, same revision).
#[tokio::test]
async fn snapshot_resent_after_reconnect() {
    // Drop after the first AppToolsUpdate (the initial empty snapshot), which
    // will force a reconnect. After registering a tool, the second connection's
    // initial snapshot must contain the tool.
    let mock = mock_hub::start_accepting_drop_after_n_snapshots(1).await;
    let tmp = TempDir::new().unwrap();
    let config = DaemonConfig::builder(mock.ws_url(), mock.valid_jwt(), tmp.path())
        .heartbeat_interval(Duration::from_secs(60))
        .build();

    let handle = spawn(config).await.expect("spawn ok");
    wait_online(&handle).await;

    // Wait for the initial snapshot to be received AND for the mock to drop
    // the connection (the mock drops after 1 snapshot). We can detect the
    // drop by waiting for the daemon to transition back to Connecting.
    let updates_after_first_conn = mock
        .wait_for_app_tools_updates(1, Duration::from_secs(5))
        .await
        .expect("initial snapshot not received within 5s");
    assert_eq!(
        updates_after_first_conn[0].revision, 0,
        "first snapshot should be rev=0"
    );

    // Wait for the daemon to reconnect (status goes to Connecting).
    let mut status = handle.subscribe_status();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(*status.borrow(), DaemonStatus::Connecting) {
                break;
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .expect("daemon did not reconnect within 5s");

    // Register a tool while the daemon is reconnecting (during the 1s backoff).
    handle
        .register_app_tool(demo_echo_def(), echo_handler())
        .await
        .expect("register ok");

    // Wait for the daemon to go Online again (second connection established).
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(*status.borrow(), DaemonStatus::Online { .. }) {
                break;
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .expect("daemon did not come back Online within 5s");

    // After the second Hello handshake, the daemon must re-send its current
    // snapshot which now includes demo_echo.
    let found_tool_snapshot = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let updates = mock.captured_app_tools_updates();
            // Look for a snapshot that contains demo_echo.
            for update in &updates {
                if update.tools.iter().any(|t| t.name == "demo_echo") {
                    return update.clone();
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("snapshot containing demo_echo not received within 5s after reconnect");

    assert_eq!(found_tool_snapshot.tools.len(), 1);
    assert_eq!(found_tool_snapshot.tools[0].name, "demo_echo");

    handle.shutdown().await.expect("shutdown clean");
}

// ── Task 5: AppToolRequest dispatch tests ─────────────────────────────────────

/// Helper: build a DaemonConfig with AutoAccept mode and no heartbeat noise.
fn dispatch_config(ws_url: String, jwt: String, tmp: &TempDir) -> DaemonConfig {
    DaemonConfig::builder(ws_url, jwt, tmp.path())
        .heartbeat_interval(Duration::from_secs(60))
        .build()
}

/// Helper: register a simple echo tool that returns `{"result": args}`.
async fn register_echo(handle: &ahandd::DaemonHandle, name: &str) {
    let def = AppToolDef {
        name: name.to_string(),
        description: "Echo tool".into(),
        input_schema: json!({"type": "object", "properties": {}}),
        requires_approval: false,
    };
    let handler: AppToolHandler =
        Arc::new(|args| Box::pin(async move { Ok(json!({"result": args})) }));
    handle
        .register_app_tool(def, handler)
        .await
        .expect("register_app_tool ok");
}

/// Helper: spawn a daemon connected to a fresh mock hub and wait until online.
/// Also drains the initial AppToolsUpdate snapshot so the inject channel is
/// ready. Returns `(mock, daemon_handle, tmp_dir)` — keep `tmp_dir` alive for
/// the duration of the test.
async fn setup_dispatch_daemon() -> (mock_hub::Mock, ahandd::DaemonHandle, TempDir) {
    let mock = mock_hub::start_accepting().await;
    let tmp = TempDir::new().unwrap();
    let handle = spawn(dispatch_config(mock.ws_url(), mock.valid_jwt(), &tmp))
        .await
        .expect("spawn ok");
    wait_online(&handle).await;
    // Drain the initial empty snapshot so inject_tx is live.
    mock.wait_for_app_tools_updates(1, Duration::from_secs(5))
        .await
        .expect("initial AppToolsUpdate not received within 5s");
    (mock, handle, tmp)
}

/// Happy path: AppToolRequest → handler runs with parsed args → AppToolResponse result_json.
#[tokio::test]
async fn happy_path_invocation() {
    let (mock, handle, _tmp) = setup_dispatch_daemon().await;

    register_echo(&handle, "echo_tool").await;

    // Wait for the tool's snapshot (revision=1) so inject channel is set up.
    mock.wait_for_app_tools_updates(2, Duration::from_secs(5))
        .await
        .expect("tool snapshot");

    mock.send_app_tool_request("call-1", "echo_tool", r#"{"key":"val"}"#, 5000)
        .expect("send ok");

    let responses = mock
        .wait_for_app_tool_responses(1, Duration::from_secs(5))
        .await
        .expect("AppToolResponse not received within 5s");

    let resp = &responses[0];
    assert_eq!(resp.tool_call_id, "call-1");
    match &resp.result {
        Some(app_tool_response::Result::ResultJson(json_str)) => {
            let v: serde_json::Value = serde_json::from_str(json_str).expect("valid json");
            assert_eq!(v["result"]["key"], "val", "handler should echo the args");
        }
        other => panic!("expected ResultJson, got {other:?}"),
    }

    handle.shutdown().await.expect("shutdown clean");
}

/// Unknown tool → TOOL_NOT_FOUND error code.
#[tokio::test]
async fn unknown_tool_returns_tool_not_found() {
    let (mock, handle, _tmp) = setup_dispatch_daemon().await;

    mock.send_app_tool_request("call-missing", "no_such_tool", r#"{}"#, 5000)
        .expect("send ok");

    let responses = mock
        .wait_for_app_tool_responses(1, Duration::from_secs(5))
        .await
        .expect("AppToolResponse not received within 5s");

    let resp = &responses[0];
    assert_eq!(resp.tool_call_id, "call-missing");
    match &resp.result {
        Some(app_tool_response::Result::Error(err)) => {
            assert_eq!(err.code, "TOOL_NOT_FOUND", "expected TOOL_NOT_FOUND code");
            assert!(
                err.message.contains("no_such_tool"),
                "error message should mention the tool name: {}",
                err.message
            );
        }
        other => panic!("expected Error, got {other:?}"),
    }

    handle.shutdown().await.expect("shutdown clean");
}

/// Non-JSON args_json → INVALID_ARGS; valid JSON but non-object → INVALID_ARGS.
#[tokio::test]
async fn invalid_args_returns_invalid_args() {
    let (mock, handle, _tmp) = setup_dispatch_daemon().await;

    register_echo(&handle, "echo2").await;

    // Wait for the tool's snapshot (revision=1).
    mock.wait_for_app_tools_updates(2, Duration::from_secs(5))
        .await
        .expect("tool snapshot");

    // Case 1: args_json is not valid JSON.
    mock.send_app_tool_request("call-bad-json", "echo2", "not json!", 5000)
        .expect("send ok");

    // Case 2: args_json is valid JSON but not an object (it's an array).
    mock.send_app_tool_request("call-bad-obj", "echo2", r#"[1,2,3]"#, 5000)
        .expect("send ok");

    let responses = mock
        .wait_for_app_tool_responses(2, Duration::from_secs(5))
        .await
        .expect("AppToolResponses not received within 5s");

    let codes: Vec<&str> = responses
        .iter()
        .map(|r| match &r.result {
            Some(app_tool_response::Result::Error(e)) => e.code.as_str(),
            _ => "NOT_ERROR",
        })
        .collect();

    assert!(
        codes.iter().all(|&c| c == "INVALID_ARGS"),
        "both responses must be INVALID_ARGS, got: {codes:?}"
    );

    handle.shutdown().await.expect("shutdown clean");
}

/// Handler sleeping past clamped timeout → EXECUTION_TIMEOUT.
/// Uses timeout_ms=1000, handler sleeps 30s (far beyond the timeout), so
/// receiving EXECUTION_TIMEOUT within the wait window proves the response
/// preceded handler completion. The daemon shutdown does not wait for the
/// detached handler task, so the test tears down promptly (in ~1-2s).
#[tokio::test]
async fn timeout_returns_execution_timeout() {
    let (mock, handle, _tmp) = setup_dispatch_daemon().await;

    let def = AppToolDef {
        name: "slow_tool".into(),
        description: "Sleeps for 30s".into(),
        input_schema: json!({"type": "object", "properties": {}}),
        requires_approval: false,
    };
    let handler: AppToolHandler = Arc::new(|_args| {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(json!({"done": true}))
        })
    });
    handle
        .register_app_tool(def, handler)
        .await
        .expect("register ok");

    // Wait for the tool's snapshot (revision=1).
    mock.wait_for_app_tools_updates(2, Duration::from_secs(5))
        .await
        .expect("tool snapshot");

    // Request with timeout_ms=1000 (will be clamped to MIN 1000ms = 1s).
    mock.send_app_tool_request("call-timeout", "slow_tool", r#"{}"#, 1000)
        .expect("send ok");

    // Should receive EXECUTION_TIMEOUT shortly after the 1s timeout fires.
    let responses = mock
        .wait_for_app_tool_responses(1, Duration::from_secs(8))
        .await
        .expect("AppToolResponse not received within 8s");

    let resp = &responses[0];
    assert_eq!(resp.tool_call_id, "call-timeout");
    match &resp.result {
        Some(app_tool_response::Result::Error(err)) => {
            assert_eq!(
                err.code, "EXECUTION_TIMEOUT",
                "expected EXECUTION_TIMEOUT, got: {}",
                err.code
            );
            assert!(
                err.message.contains("1000ms"),
                "message should mention timeout value: {}",
                err.message
            );
        }
        other => panic!("expected EXECUTION_TIMEOUT error, got {other:?}"),
    }

    handle.shutdown().await.expect("shutdown clean");
}

/// Panicking handler → HANDLER_PANIC error; daemon survives and subsequent
/// calls succeed.
#[tokio::test]
async fn panic_isolated_daemon_survives() {
    let (mock, handle, _tmp) = setup_dispatch_daemon().await;

    // Register the panicking tool.
    let panic_def = AppToolDef {
        name: "panic_tool".into(),
        description: "Always panics".into(),
        input_schema: json!({"type": "object", "properties": {}}),
        requires_approval: false,
    };
    let panic_handler: AppToolHandler = Arc::new(|_args| {
        Box::pin(async move {
            panic!("intentional test panic");
        })
    });
    handle
        .register_app_tool(panic_def, panic_handler)
        .await
        .expect("register panic_tool ok");

    // Also register a healthy tool for the survivability check.
    register_echo(&handle, "healthy_tool").await;

    // Wait until the latest snapshot contains both tools (handles potential
    // batching or interleaving of the two register calls).
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let updates = mock.captured_app_tools_updates();
            if let Some(snap) = updates.last() {
                if snap.tools.len() >= 2 {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("snapshot with both tools not received within 5s");

    // Fire the panicking tool.
    mock.send_app_tool_request("call-panic", "panic_tool", r#"{}"#, 5000)
        .expect("send ok");

    let responses = mock
        .wait_for_app_tool_responses(1, Duration::from_secs(5))
        .await
        .expect("panic response not received within 5s");

    let resp = &responses[0];
    assert_eq!(resp.tool_call_id, "call-panic");
    match &resp.result {
        Some(app_tool_response::Result::Error(err)) => {
            assert_eq!(err.code, "HANDLER_PANIC", "expected HANDLER_PANIC");
        }
        other => panic!("expected HANDLER_PANIC, got {other:?}"),
    }

    // Daemon should still be alive — fire a healthy call.
    mock.send_app_tool_request("call-after-panic", "healthy_tool", r#"{"x":1}"#, 5000)
        .expect("send ok");

    let responses = mock
        .wait_for_app_tool_responses(2, Duration::from_secs(5))
        .await
        .expect("healthy response after panic not received within 5s");

    let healthy_resp = &responses[1];
    assert_eq!(healthy_resp.tool_call_id, "call-after-panic");
    assert!(
        matches!(
            &healthy_resp.result,
            Some(app_tool_response::Result::ResultJson(_))
        ),
        "healthy call should succeed after panic, got: {:?}",
        healthy_resp.result
    );

    handle.shutdown().await.expect("shutdown clean");
}

/// 5 concurrent slow calls → 4 succeed + exactly 1 CONCURRENCY_LIMIT immediately.
/// MAX_CONCURRENT_APP_TOOLS is 4.
#[tokio::test]
async fn concurrency_limit_fifth_call() {
    let (mock, handle, _tmp) = setup_dispatch_daemon().await;

    // Register a slow tool that sleeps ~2.5s.
    let def = AppToolDef {
        name: "slow_concurrent".into(),
        description: "Slow concurrent tool".into(),
        input_schema: json!({"type": "object", "properties": {}}),
        requires_approval: false,
    };
    let handler: AppToolHandler = Arc::new(|_args| {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(2500)).await;
            Ok(json!({"done": true}))
        })
    });
    handle
        .register_app_tool(def, handler)
        .await
        .expect("register ok");

    // Wait for the tool's snapshot (revision=1).
    mock.wait_for_app_tools_updates(2, Duration::from_secs(5))
        .await
        .expect("tool snapshot");

    // Fire 5 requests back-to-back before any complete.
    for i in 0..5 {
        mock.send_app_tool_request(
            format!("call-concurrent-{i}"),
            "slow_concurrent",
            r#"{}"#,
            10_000,
        )
        .expect("send ok");
    }

    // Wait for all 5 responses with generous timeout (slow_tool ~2.5s + buffer).
    let responses = mock
        .wait_for_app_tool_responses(5, Duration::from_secs(15))
        .await
        .expect("5 responses not received within 15s");

    let concurrency_errors = responses
        .iter()
        .filter(|r| {
            matches!(
                &r.result,
                Some(app_tool_response::Result::Error(e)) if e.code == "CONCURRENCY_LIMIT"
            )
        })
        .count();
    let successes = responses
        .iter()
        .filter(|r| matches!(&r.result, Some(app_tool_response::Result::ResultJson(_))))
        .count();

    assert_eq!(
        concurrency_errors,
        1,
        "exactly 1 CONCURRENCY_LIMIT expected, got {concurrency_errors} out of 5 responses: {:?}",
        responses.iter().map(|r| &r.result).collect::<Vec<_>>()
    );
    assert_eq!(
        successes, 4,
        "exactly 4 successes expected, got {successes}"
    );

    handle.shutdown().await.expect("shutdown clean");
}

/// Duplicate tool_call_id while running → silently ignored.
/// After completion → cached response re-sent with identical payload.
/// Also verifies handler runs only once (AtomicUsize counter).
#[tokio::test]
async fn duplicate_call_id_replays_cached_response() {
    let (mock, handle, _tmp) = setup_dispatch_daemon().await;

    // Register a tool that tracks how many times it ran.
    let run_count = Arc::new(AtomicUsize::new(0));
    let run_count_clone = Arc::clone(&run_count);
    let def = AppToolDef {
        name: "counted_tool".into(),
        description: "Counts invocations".into(),
        input_schema: json!({"type": "object", "properties": {}}),
        requires_approval: false,
    };
    let handler: AppToolHandler = Arc::new(move |_args| {
        let counter = Arc::clone(&run_count_clone);
        Box::pin(async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(json!({"invocation_count": 1}))
        })
    });
    handle
        .register_app_tool(def, handler)
        .await
        .expect("register ok");

    // Wait for the tool's snapshot (revision=1).
    mock.wait_for_app_tools_updates(2, Duration::from_secs(5))
        .await
        .expect("tool snapshot");

    // First invocation.
    mock.send_app_tool_request("call-dup-1", "counted_tool", r#"{}"#, 5000)
        .expect("send ok");

    // Wait for the first response.
    let responses = mock
        .wait_for_app_tool_responses(1, Duration::from_secs(5))
        .await
        .expect("first response not received within 5s");
    let first_result = responses[0].result.clone();

    // Send the same tool_call_id again — should replay cached response.
    mock.send_app_tool_request("call-dup-1", "counted_tool", r#"{}"#, 5000)
        .expect("send ok");

    let responses = mock
        .wait_for_app_tool_responses(2, Duration::from_secs(5))
        .await
        .expect("second response not received within 5s");

    // Both responses must carry identical result payloads.
    assert_eq!(
        responses[1].result, responses[0].result,
        "replayed response must be identical to the original"
    );
    assert_eq!(responses[0].result, first_result);

    // Handler should have run exactly once.
    assert_eq!(
        run_count.load(Ordering::SeqCst),
        1,
        "handler should run exactly once; got {}",
        run_count.load(Ordering::SeqCst)
    );

    handle.shutdown().await.expect("shutdown clean");
}

/// Duplicate tool_call_id while running → silently ignored; exactly ONE
/// response total arrives; handler runs only once (AtomicUsize).
///
/// The handler sleeps ~2s so the second request arrives while the first is
/// still in-flight. The wait window (5s) exceeds handler duration so we
/// always see the single success response. Daemon shutdown does NOT wait for
/// the detached handler task, so the test completes promptly.
#[tokio::test]
async fn duplicate_call_id_while_running_is_ignored() {
    let (mock, handle, _tmp) = setup_dispatch_daemon().await;

    let run_count = Arc::new(AtomicUsize::new(0));
    let run_count_clone = Arc::clone(&run_count);

    let def = AppToolDef {
        name: "slow_once".into(),
        description: "Slow tool that counts invocations".into(),
        input_schema: json!({"type": "object", "properties": {}}),
        requires_approval: false,
    };
    let handler: AppToolHandler = Arc::new(move |_args| {
        let counter = Arc::clone(&run_count_clone);
        Box::pin(async move {
            counter.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(2)).await;
            Ok(json!({"done": true}))
        })
    });
    handle
        .register_app_tool(def, handler)
        .await
        .expect("register ok");

    // Wait for the tool's snapshot to arrive (revision=1).
    mock.wait_for_app_tools_updates(2, Duration::from_secs(5))
        .await
        .expect("snapshot with slow_once not received within 5s");

    // Send the same tool_call_id twice immediately, while the first is running.
    mock.send_app_tool_request("dup-running-1", "slow_once", r#"{}"#, 10_000)
        .expect("first send ok");
    mock.send_app_tool_request("dup-running-1", "slow_once", r#"{}"#, 10_000)
        .expect("second send ok (duplicate)");

    // Wait longer than handler duration (2s) so the one response arrives.
    let responses = mock
        .wait_for_app_tool_responses(1, Duration::from_secs(5))
        .await
        .expect("exactly one AppToolResponse not received within 5s");

    // Allow a small settle window for any spurious second response.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let all_responses = mock.captured_app_tool_responses();
    assert_eq!(
        all_responses.len(),
        1,
        "expected exactly 1 response for duplicate call_id, got {}",
        all_responses.len()
    );

    // The single response must be success (not an error).
    assert!(
        matches!(
            &responses[0].result,
            Some(app_tool_response::Result::ResultJson(_))
        ),
        "expected ResultJson, got: {:?}",
        responses[0].result
    );

    // Handler ran exactly once.
    assert_eq!(
        run_count.load(Ordering::SeqCst),
        1,
        "handler should run exactly once; got {}",
        run_count.load(Ordering::SeqCst)
    );

    handle.shutdown().await.expect("shutdown clean");
}
