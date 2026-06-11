//! Integration tests for Task 9: POST /api/control/app-tool invocation.
//!
//! Covers the acceptance-criteria matrix from the plan:
//!   - happy path: POST dispatches AppToolRequest to device, device replies
//!     ResultJson, HTTP 200 with parsed result
//!   - daemon-error passthrough: device replies Error, HTTP 200 with error body
//!   - offline device: 409 DEVICE_OFFLINE fast-fail (no device attached)
//!   - unknown device: 404 DEVICE_NOT_FOUND
//!   - wrong owner: 403 FORBIDDEN
//!   - device_ids allowlist excluded: 403 FORBIDDEN
//!   - wrong scope: 403 FORBIDDEN
//!   - hub timeout: 504 TIMEOUT with timeoutMs=1000 and silent device
//!   - pending map cleaned after timeout (second request still works)
//!   - audit entries written for happy, daemon-error, timeout, offline cases

mod support;

use std::time::Duration;

use ahand_hub_core::audit::AuditFilter;
use ahand_protocol::{AppToolError, AppToolResponse, Envelope, app_tool_response, envelope};
use futures_util::{SinkExt, StreamExt};
use prost::Message;
use reqwest::StatusCode;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use support::{
    attach_owned_device, mint_cp_jwt, mint_cp_jwt_with_options, spawn_server_with_state, test_state,
};

// Re-export for use in clamping tests.
use ahand_hub::app_tool_service::{DEFAULT_TIMEOUT_MS, MAX_TIMEOUT_MS, MIN_TIMEOUT_MS};

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Helper: POST to /api/control/app-tool with a bearer token + JSON body.
async fn post_invoke(
    server: &support::TestServer,
    token: &str,
    body: serde_json::Value,
) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}/api/control/app-tool", server.http_base_url()))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap()
}

/// Read the next AppToolRequest envelope from a device WS, skipping
/// unrelated frames (e.g. ACK-only frames, App Tool Update acks).
async fn recv_app_tool_request(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> ahand_protocol::AppToolRequest {
    while let Some(message) = socket.next().await {
        let msg = message.unwrap();
        if let WsMessage::Binary(data) = msg {
            let env = Envelope::decode(data.as_ref()).unwrap();
            if let Some(envelope::Payload::AppToolRequest(req)) = env.payload {
                return req;
            }
        }
    }
    panic!("device socket closed before AppToolRequest arrived");
}

/// Send an AppToolResponse from the device back to the hub.
async fn send_app_tool_response(
    device_id: &str,
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    response: AppToolResponse,
) {
    let env = Envelope {
        device_id: device_id.into(),
        msg_id: format!("app-tool-resp-{}", response.tool_call_id),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::AppToolResponse(response)),
        ..Default::default()
    };
    socket
        .send(WsMessage::Binary(env.encode_to_vec().into()))
        .await
        .unwrap();
}

/// Poll the audit store until at least one entry matching `action` and
/// `resource_id` appears, or time out.
async fn poll_audit(
    state: &ahand_hub::state::AppState,
    action: &str,
    resource_id: &str,
    timeout: Duration,
) -> Vec<ahand_hub_core::audit::AuditEntry> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let entries = state
            .audit_store
            .query(AuditFilter {
                action: Some(action.into()),
                resource_id: Some(resource_id.into()),
                ..Default::default()
            })
            .await
            .expect("audit query");
        if !entries.is_empty() {
            return entries;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "expected at least one audit entry action={action:?} resource_id={resource_id:?} \
                 after {timeout:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ────────────────────────────────────────────────────────────
// Happy path
// ────────────────────────────────────────────────────────────

#[tokio::test]
async fn happy_path_dispatches_and_returns_result() {
    let state = test_state().await;
    let server = spawn_server_with_state(state).await;

    // Attach a device owned by "user-invoke".
    let mut socket = attach_owned_device(&server, "inv-dev-1", "user-invoke").await;

    let token = mint_cp_jwt("user-invoke");

    // Spawn the device side: wait for AppToolRequest, reply with ResultJson.
    let device_task = tokio::spawn(async move {
        let req = recv_app_tool_request(&mut socket).await;
        assert_eq!(req.name, "list_documents");
        assert_eq!(req.args_json, r#"{"limit":5}"#);
        assert_eq!(req.timeout_ms, 30_000);
        // tool_call_id is a UUID — just verify it's non-empty.
        assert!(!req.tool_call_id.is_empty());
        let tool_call_id = req.tool_call_id.clone();
        send_app_tool_response(
            "inv-dev-1",
            &mut socket,
            AppToolResponse {
                tool_call_id,
                result: Some(app_tool_response::Result::ResultJson(
                    r#"{"docs":["a.txt","b.txt"]}"#.into(),
                )),
            },
        )
        .await;
        req.tool_call_id
    });

    let resp = post_invoke(
        &server,
        &token,
        serde_json::json!({
            "deviceId": "inv-dev-1",
            "name": "list_documents",
            "args": {"limit": 5},
            "timeoutMs": 30_000,
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "expected 200 OK");

    let body: serde_json::Value = resp.json().await.unwrap();
    let tool_call_id_from_device = device_task.await.unwrap();
    assert_eq!(body["toolCallId"], tool_call_id_from_device);
    assert_eq!(body["result"]["docs"][0], "a.txt");
    assert!(body.get("error").is_none() || body["error"].is_null());

    server.shutdown().await;
}

// ────────────────────────────────────────────────────────────
// Offline fast-fail → 409
// ────────────────────────────────────────────────────────────

#[tokio::test]
async fn offline_device_returns_409_fast() {
    use ahand_hub_core::traits::DeviceAdminStore;
    use ed25519_dalek::SigningKey;

    let state = test_state().await;
    let server = spawn_server_with_state(state).await;

    // Pre-register the device but do NOT attach a WS — device is offline.
    let verifying = SigningKey::from_bytes(&[9u8; 32])
        .verifying_key()
        .to_bytes();
    server
        .state()
        .devices
        .pre_register("offline-dev", &verifying, "user-offline")
        .await
        .unwrap();

    let token = mint_cp_jwt("user-offline");

    let start = std::time::Instant::now();
    let resp = post_invoke(
        &server,
        &token,
        serde_json::json!({
            "deviceId": "offline-dev",
            "name": "some_tool",
        }),
    )
    .await;
    let elapsed = start.elapsed();

    assert_eq!(resp.status(), StatusCode::CONFLICT, "expected 409 CONFLICT");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "DEVICE_OFFLINE");
    // Must return quickly — well under 2 s even on a slow CI runner.
    assert!(
        elapsed < Duration::from_secs(2),
        "offline fast-fail took too long: {elapsed:?}"
    );

    server.shutdown().await;
}

// ────────────────────────────────────────────────────────────
// Daemon-error passthrough
// ────────────────────────────────────────────────────────────

#[tokio::test]
async fn daemon_error_returned_as_200_with_error_body() {
    let state = test_state().await;
    let server = spawn_server_with_state(state).await;

    let mut socket = attach_owned_device(&server, "inv-dev-2", "user-invoke2").await;

    let token = mint_cp_jwt("user-invoke2");

    let device_task = tokio::spawn(async move {
        let req = recv_app_tool_request(&mut socket).await;
        let tool_call_id = req.tool_call_id.clone();
        send_app_tool_response(
            "inv-dev-2",
            &mut socket,
            AppToolResponse {
                tool_call_id: tool_call_id.clone(),
                result: Some(app_tool_response::Result::Error(AppToolError {
                    code: "APPROVAL_DENIED".into(),
                    message: "user declined the request".into(),
                })),
            },
        )
        .await;
        tool_call_id
    });

    let resp = post_invoke(
        &server,
        &token,
        serde_json::json!({
            "deviceId": "inv-dev-2",
            "name": "restricted_tool",
        }),
    )
    .await;
    // Daemon errors → HTTP 200 (body carries the error).
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "expected 200 OK for daemon error"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    let _tool_call_id = device_task.await.unwrap();
    assert_eq!(body["error"]["code"], "APPROVAL_DENIED");
    assert_eq!(body["error"]["message"], "user declined the request");
    assert!(body.get("result").is_none() || body["result"].is_null());

    server.shutdown().await;
}

// ────────────────────────────────────────────────────────────
// Unknown device → 404
// ────────────────────────────────────────────────────────────

#[tokio::test]
async fn unknown_device_returns_404() {
    let state = test_state().await;
    let server = spawn_server_with_state(state).await;

    let token = mint_cp_jwt("user-unknown");

    let resp = post_invoke(
        &server,
        &token,
        serde_json::json!({
            "deviceId": "does-not-exist",
            "name": "some_tool",
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "expected 404");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "DEVICE_NOT_FOUND");

    server.shutdown().await;
}

// ────────────────────────────────────────────────────────────
// Auth / ownership / scope → 403
// ────────────────────────────────────────────────────────────

#[tokio::test]
async fn wrong_owner_returns_403() {
    let state = test_state().await;
    let server = spawn_server_with_state(state).await;

    // Device owned by "user-a".
    let _socket = attach_owned_device(&server, "auth-dev-1", "user-a").await;
    // JWT is for "user-b".
    let token = mint_cp_jwt("user-b");

    let resp = post_invoke(
        &server,
        &token,
        serde_json::json!({
            "deviceId": "auth-dev-1",
            "name": "some_tool",
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "expected 403");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "FORBIDDEN");

    server.shutdown().await;
}

#[tokio::test]
async fn device_not_in_allowlist_returns_403() {
    let state = test_state().await;
    let server = spawn_server_with_state(state).await;

    let _socket = attach_owned_device(&server, "auth-dev-2", "user-c").await;
    // Token is scoped to a different device list.
    let token = mint_cp_jwt_with_options(
        "user-c",
        "jobs:execute",
        Some(vec!["some-other-device".into()]),
    );

    let resp = post_invoke(
        &server,
        &token,
        serde_json::json!({
            "deviceId": "auth-dev-2",
            "name": "some_tool",
        }),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "expected 403 for allowlist"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn wrong_scope_returns_403() {
    let state = test_state().await;
    let server = spawn_server_with_state(state).await;

    let _socket = attach_owned_device(&server, "auth-dev-3", "user-d").await;
    let token = mint_cp_jwt_with_options("user-d", "jobs:read", None);

    let resp = post_invoke(
        &server,
        &token,
        serde_json::json!({
            "deviceId": "auth-dev-3",
            "name": "some_tool",
        }),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "expected 403 for wrong scope"
    );

    server.shutdown().await;
}

// ────────────────────────────────────────────────────────────
// Hub timeout → 504, pending map cleaned
// ────────────────────────────────────────────────────────────

#[tokio::test]
async fn timeout_returns_504_and_pending_map_cleaned() {
    let state = test_state().await;
    let server = spawn_server_with_state(state.clone()).await;

    // Attach device but it will never reply.
    let _socket = attach_owned_device(&server, "timeout-dev", "user-timeout").await;
    let token = mint_cp_jwt("user-timeout");

    let start = std::time::Instant::now();
    let resp = post_invoke(
        &server,
        &token,
        serde_json::json!({
            "deviceId": "timeout-dev",
            "name": "slow_tool",
            "timeoutMs": 1000,
        }),
    )
    .await;
    let elapsed = start.elapsed();

    assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT, "expected 504");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "TIMEOUT");

    // Should complete in ~3s (1s timeout + 2s grace). Be generous for CI.
    assert!(
        elapsed < Duration::from_secs(8),
        "timeout request took too long: {elapsed:?}"
    );

    // Pending map must be clean — the RAII guard drops when invoke() returns.
    assert_eq!(
        server.state().app_tool_pending.len(),
        0,
        "app_tool_pending should be empty after timeout"
    );

    server.shutdown().await;
}

// ────────────────────────────────────────────────────────────
// Audit entries
// ────────────────────────────────────────────────────────────

#[tokio::test]
async fn audit_entry_written_for_happy_path() {
    let state = test_state().await;
    let server = spawn_server_with_state(state.clone()).await;

    let mut socket = attach_owned_device(&server, "audit-dev-1", "user-audit1").await;
    let token = mint_cp_jwt("user-audit1");

    let device_task = tokio::spawn(async move {
        let req = recv_app_tool_request(&mut socket).await;
        let tcid = req.tool_call_id.clone();
        send_app_tool_response(
            "audit-dev-1",
            &mut socket,
            AppToolResponse {
                tool_call_id: tcid.clone(),
                result: Some(app_tool_response::Result::ResultJson("{}".into())),
            },
        )
        .await;
        tcid
    });

    let resp = post_invoke(
        &server,
        &token,
        serde_json::json!({
            "deviceId": "audit-dev-1",
            "name": "audit_tool",
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let _tcid = device_task.await.unwrap();

    let entries = poll_audit(
        server.state(),
        "app_tool.invoked",
        "audit-dev-1",
        Duration::from_secs(3),
    )
    .await;
    assert!(!entries.is_empty());
    let entry = &entries[0];
    assert_eq!(entry.action, "app_tool.invoked");
    assert_eq!(entry.resource_type, "device");
    assert_eq!(entry.resource_id, "audit-dev-1");
    assert_eq!(entry.actor, "user-audit1");
    assert_eq!(entry.detail["name"], "audit_tool");
    assert_eq!(entry.detail["outcome"], "ok");

    server.shutdown().await;
}

#[tokio::test]
async fn audit_entry_written_for_daemon_error() {
    let state = test_state().await;
    let server = spawn_server_with_state(state.clone()).await;

    let mut socket = attach_owned_device(&server, "audit-dev-2", "user-audit2").await;
    let token = mint_cp_jwt("user-audit2");

    tokio::spawn(async move {
        let req = recv_app_tool_request(&mut socket).await;
        let tcid = req.tool_call_id.clone();
        send_app_tool_response(
            "audit-dev-2",
            &mut socket,
            AppToolResponse {
                tool_call_id: tcid,
                result: Some(app_tool_response::Result::Error(AppToolError {
                    code: "TOOL_NOT_FOUND".into(),
                    message: "no such tool".into(),
                })),
            },
        )
        .await;
    });

    let resp = post_invoke(
        &server,
        &token,
        serde_json::json!({
            "deviceId": "audit-dev-2",
            "name": "missing_tool",
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let entries = poll_audit(
        server.state(),
        "app_tool.invoked",
        "audit-dev-2",
        Duration::from_secs(3),
    )
    .await;
    let entry = &entries[0];
    assert_eq!(entry.detail["outcome"], "daemon_error:TOOL_NOT_FOUND");
    assert_eq!(entry.detail["name"], "missing_tool");

    server.shutdown().await;
}

#[tokio::test]
async fn audit_entry_written_for_offline() {
    use ahand_hub_core::traits::DeviceAdminStore;
    use ed25519_dalek::SigningKey;

    let state = test_state().await;
    let server = spawn_server_with_state(state.clone()).await;

    let verifying = SigningKey::from_bytes(&[42u8; 32])
        .verifying_key()
        .to_bytes();
    server
        .state()
        .devices
        .pre_register("audit-offline-dev", &verifying, "user-audit-offline")
        .await
        .unwrap();

    let token = mint_cp_jwt("user-audit-offline");
    let resp = post_invoke(
        &server,
        &token,
        serde_json::json!({
            "deviceId": "audit-offline-dev",
            "name": "some_tool",
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    let entries = poll_audit(
        server.state(),
        "app_tool.invoked",
        "audit-offline-dev",
        Duration::from_secs(3),
    )
    .await;
    let entry = &entries[0];
    assert_eq!(entry.detail["outcome"], "offline");
    assert_eq!(entry.detail["name"], "some_tool");

    server.shutdown().await;
}

#[tokio::test]
async fn audit_entry_written_for_timeout() {
    let state = test_state().await;
    let server = spawn_server_with_state(state.clone()).await;

    // Attach device. It will receive the AppToolRequest but intentionally
    // never reply, causing the hub to time out.
    let mut socket = attach_owned_device(&server, "audit-timeout-dev", "user-audit-timeout").await;
    let token = mint_cp_jwt("user-audit-timeout");

    // Concurrently: device captures the tool_call_id from the request but
    // does NOT send a response, so the hub times out.
    let device_task = tokio::spawn(async move {
        let req = recv_app_tool_request(&mut socket).await;
        // Keep socket alive (do not drop) so the hub sees a live connection;
        // the oneshot simply never resolves.
        (req.tool_call_id, socket)
    });

    let resp = post_invoke(
        &server,
        &token,
        serde_json::json!({
            "deviceId": "audit-timeout-dev",
            "name": "slow_tool",
            "timeoutMs": 1000,
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);

    // The device task should have captured the tool_call_id already (the
    // hub dispatches before waiting, so the device receives it immediately).
    let (dispatched_tool_call_id, _socket) = device_task.await.unwrap();

    let entries = poll_audit(
        server.state(),
        "app_tool.invoked",
        "audit-timeout-dev",
        Duration::from_secs(5),
    )
    .await;
    let entry = &entries[0];
    assert_eq!(entry.detail["outcome"], "timeout");
    assert_eq!(entry.detail["name"], "slow_tool");

    // The audit entry must carry the real UUID, not a placeholder.
    let audit_tool_call_id = entry.detail["toolCallId"]
        .as_str()
        .expect("toolCallId must be a string");
    uuid::Uuid::parse_str(audit_tool_call_id)
        .expect("toolCallId in audit entry must be a valid UUID");
    assert_eq!(
        audit_tool_call_id, dispatched_tool_call_id,
        "audit toolCallId must equal the UUID the device received"
    );

    server.shutdown().await;
}

// ────────────────────────────────────────────────────────────
// Timeout clamping: MIN / MAX / default
// ────────────────────────────────────────────────────────────

/// Verifies that the hub applies the [MIN, MAX] clamp to the caller-supplied
/// `timeoutMs`, and that omitting `timeoutMs` sends the default.
///
/// Three invocations share one device socket (the device replies immediately
/// each time so the handler doesn't block on the timeout):
///   1. timeoutMs=500  → device receives timeout_ms == MIN_TIMEOUT_MS (1 000)
///   2. timeoutMs=400000 → device receives timeout_ms == MAX_TIMEOUT_MS (300 000)
///   3. omitted timeoutMs  → device receives timeout_ms == DEFAULT_TIMEOUT_MS (60 000)
#[tokio::test]
async fn timeout_clamping_is_applied_before_dispatch() {
    let state = test_state().await;
    let server = spawn_server_with_state(state).await;

    let mut socket = attach_owned_device(&server, "clamp-dev", "user-clamp").await;
    let token = mint_cp_jwt("user-clamp");

    // Helper closure: POST the request then immediately have the device
    // capture and echo back so the handler resolves successfully.
    async fn invoke_and_capture(
        server: &support::TestServer,
        token: &str,
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        body: serde_json::Value,
    ) -> u32 {
        let device_id = body["deviceId"].as_str().unwrap().to_string();
        let device_id_clone = device_id.clone();

        // Spawn device side: capture timeout_ms, reply with ResultJson.
        let device_task = tokio::spawn({
            // We can't move the socket into spawn, so we drive it manually below.
            // Instead, use a channel to coordinate.
            // (We poll the socket inline after posting.)
            async move { () }
        });
        // Drop placeholder task immediately.
        drop(device_task);

        // Post from HTTP side in a separate task so we can drive the socket
        // concurrently.
        let post_fut = post_invoke(server, token, body);

        // Drive both concurrently: first await the HTTP request (which blocks
        // until device responds), but we need the device side to run too.
        // Use tokio::join! with the device-side driven inline.
        let captured_timeout_ms = tokio::sync::OnceCell::<u32>::new();
        let captured_timeout_ms_clone = &captured_timeout_ms;

        let (resp, received_timeout_ms) = tokio::join!(post_fut, async {
            let req = recv_app_tool_request(socket).await;
            let tm = req.timeout_ms;
            let tcid = req.tool_call_id.clone();
            send_app_tool_response(
                &device_id_clone,
                socket,
                AppToolResponse {
                    tool_call_id: tcid,
                    result: Some(app_tool_response::Result::ResultJson("{}".into())),
                },
            )
            .await;
            tm
        });

        let _ = captured_timeout_ms_clone.set(received_timeout_ms);
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "expected 200 OK for clamping test"
        );
        received_timeout_ms
    }

    // 1. Below minimum → clamped up to MIN_TIMEOUT_MS.
    let tm1 = invoke_and_capture(
        &server,
        &token,
        &mut socket,
        serde_json::json!({
            "deviceId": "clamp-dev",
            "name": "tool_a",
            "timeoutMs": 500,
        }),
    )
    .await;
    assert_eq!(
        tm1, MIN_TIMEOUT_MS as u32,
        "500ms should be clamped to MIN ({MIN_TIMEOUT_MS})"
    );

    // 2. Above maximum → clamped down to MAX_TIMEOUT_MS.
    let tm2 = invoke_and_capture(
        &server,
        &token,
        &mut socket,
        serde_json::json!({
            "deviceId": "clamp-dev",
            "name": "tool_b",
            "timeoutMs": 400_000u64,
        }),
    )
    .await;
    assert_eq!(
        tm2, MAX_TIMEOUT_MS as u32,
        "400 000ms should be clamped to MAX ({MAX_TIMEOUT_MS})"
    );

    // 3. Omitted → default.
    let tm3 = invoke_and_capture(
        &server,
        &token,
        &mut socket,
        serde_json::json!({
            "deviceId": "clamp-dev",
            "name": "tool_c",
        }),
    )
    .await;
    assert_eq!(
        tm3, DEFAULT_TIMEOUT_MS as u32,
        "omitted timeoutMs should default to DEFAULT ({DEFAULT_TIMEOUT_MS})"
    );

    server.shutdown().await;
}

// NOTE: Oversized result payloads are not capped in this stage — the hub
// passes through whatever the daemon sends as result_json without a size
// check. This is a known limitation documented for a follow-up task.
