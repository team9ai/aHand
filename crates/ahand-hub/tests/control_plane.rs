//! Integration tests for Task 1.4: control-plane REST + SSE surface
//! (`/api/control/*`).
//!
//! Covers the acceptance-criteria matrix from the plan:
//!   - happy path: POST → SSE streams stdout/stderr/progress/finished
//!   - 403 on external_user_id mismatch
//!   - 403 on device with no external_user_id
//!   - 404 on device offline
//!   - 404 on unknown job id
//!   - 404 on another user's job id (no cross-user oracle)
//!   - 400 on missing `tool`
//!   - 400 on malformed body
//!   - 429 on rate-limit breach
//!   - idempotent correlation_id → same job_id, no re-dispatch
//!   - SSE client disconnect cleans up subscribers, no leak
//!   - two concurrent SSE clients receive all events (broadcast semantics)
//!   - large stdout chunk (>1MB) delivered intact
//!   - device without matching external_user_id hit via cancel → 404
//!   - missing JWT → 401

mod support;

use std::time::Duration;

use ahand_hub_core::traits::DeviceAdminStore;
use ahand_protocol::envelope;
use futures_util::StreamExt;
use prost::Message;
use reqwest::StatusCode;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use support::{
    read_hello_accepted, read_hello_challenge, signed_hello, spawn_server_with_state, test_state,
    TestServer,
};

const JWT_SECRET: &str = "service-test-secret";

/// Register a control-plane-ready device owned by `external_user_id`,
/// attach a live WS daemon, and return a handle to the socket + the
/// daemon's device id. The caller is responsible for dropping the
/// socket when done.
async fn attach_owned_device(
    server: &TestServer,
    device_id: &str,
    external_user_id: &str,
) -> tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
> {
    use ed25519_dalek::SigningKey;
    // Seed the device row with the expected public key and
    // external_user_id BEFORE the daemon says hello — this mirrors
    // the "admin pre-register → daemon hello" flow.
    let verifying = SigningKey::from_bytes(&[7u8; 32]).verifying_key().to_bytes();
    server
        .state()
        .devices
        .pre_register(device_id, &verifying, external_user_id)
        .await
        .unwrap();
    use futures_util::SinkExt;
    let (mut socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();
    let challenge = read_hello_challenge(&mut socket).await;
    let hello = signed_hello(device_id, &challenge.nonce);
    socket
        .send(WsMessage::Binary(hello.encode_to_vec().into()))
        .await
        .unwrap();
    let _ = read_hello_accepted(&mut socket).await;
    // Small grace so the hub finishes registering the WS connection.
    tokio::time::sleep(Duration::from_millis(50)).await;
    socket
}

/// Mint a control-plane JWT directly via AuthService (avoids round-tripping
/// through admin API).
fn mint_cp_jwt(_server: &TestServer, external_user_id: &str) -> String {
    use ahand_hub_core::auth::mint_control_plane_jwt;
    let (token, _) = mint_control_plane_jwt(
        JWT_SECRET.as_bytes(),
        external_user_id,
        "jobs:execute",
        None,
        Duration::from_secs(60),
    )
    .unwrap();
    token
}

/// Mint a control-plane JWT with custom scope and/or device_ids allowlist.
fn mint_cp_jwt_with_options(
    external_user_id: &str,
    scope: &str,
    device_ids: Option<Vec<String>>,
) -> String {
    use ahand_hub_core::auth::mint_control_plane_jwt;
    let (token, _) = mint_control_plane_jwt(
        JWT_SECRET.as_bytes(),
        external_user_id,
        scope,
        device_ids,
        Duration::from_secs(60),
    )
    .unwrap();
    token
}

/// Read the next JobRequest envelope from a daemon WS, asserting
/// envelope shape. Returns the inner JobRequest.
async fn recv_job_request(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> ahand_protocol::JobRequest {
    while let Some(message) = socket.next().await {
        let msg = message.unwrap();
        if let WsMessage::Binary(data) = msg {
            let envelope = ahand_protocol::Envelope::decode(data.as_ref()).unwrap();
            if let Some(envelope::Payload::JobRequest(job)) = envelope.payload {
                return job;
            }
        }
    }
    panic!("device socket closed before JobRequest arrived");
}

async fn recv_cancel(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> ahand_protocol::CancelJob {
    while let Some(message) = socket.next().await {
        let msg = message.unwrap();
        if let WsMessage::Binary(data) = msg {
            let envelope = ahand_protocol::Envelope::decode(data.as_ref()).unwrap();
            if let Some(envelope::Payload::CancelJob(c)) = envelope.payload {
                return c;
            }
        }
    }
    panic!("device socket closed before CancelJob arrived");
}

async fn send_envelope(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    envelope: ahand_protocol::Envelope,
) {
    use futures_util::SinkExt;
    socket
        .send(WsMessage::Binary(envelope.encode_to_vec().into()))
        .await
        .unwrap();
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// Post a CreateJob with a given JWT. Returns the full response.
async fn post_create_job(
    server: &TestServer,
    token: &str,
    body: serde_json::Value,
) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}/api/control/jobs", server.http_base_url()))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn create_job_happy_path_dispatches_and_streams_events() {
    let server = spawn_server_with_state(test_state().await).await;
    let mut device = attach_owned_device(&server, "cp-dev-1", "user-cp").await;
    let token = mint_cp_jwt(&server, "user-cp");

    let resp = post_create_job(
        &server,
        &token,
        serde_json::json!({
            "device_id": "cp-dev-1",
            "tool": "echo",
            "args": ["hello"],
            "timeout_ms": 30_000,
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let body: serde_json::Value = resp.json().await.unwrap();
    let job_id = body["job_id"].as_str().unwrap().to_string();
    assert!(!job_id.is_empty());

    // Daemon should now see a JobRequest with our ulid.
    let received = recv_job_request(&mut device).await;
    assert_eq!(received.job_id, job_id);
    assert_eq!(received.tool, "echo");
    assert_eq!(received.args, vec!["hello".to_string()]);
    assert_eq!(received.timeout_ms, 30_000);

    // Stream the job in parallel with emitting events from the fake
    // daemon. We open the SSE, then race a task that emits stdout,
    // progress, and finished.
    let stream_task = tokio::spawn({
        let server_url = server.http_base_url().to_string();
        let token = token.clone();
        let job_id = job_id.clone();
        async move {
            let resp = reqwest::Client::new()
                .get(format!("{server_url}/api/control/jobs/{job_id}/stream"))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            assert!(
                resp.headers()
                    .get("content-type")
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with("text/event-stream")
            );
            let mut stream = resp.bytes_stream();
            let mut body = String::new();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.unwrap();
                body.push_str(&String::from_utf8_lossy(&chunk));
                if body.contains("event: finished") {
                    break;
                }
            }
            body
        }
    });

    // Give the SSE subscriber a moment to attach to the broadcast
    // before we start emitting events (avoids dropping early frames).
    tokio::time::sleep(Duration::from_millis(100)).await;

    send_envelope(
        &mut device,
        ahand_protocol::Envelope {
            device_id: "cp-dev-1".into(),
            msg_id: "ev-1".into(),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::JobEvent(ahand_protocol::JobEvent {
                job_id: job_id.clone(),
                event: Some(ahand_protocol::job_event::Event::StdoutChunk(
                    b"hi\nworld".to_vec(),
                )),
            })),
            ..Default::default()
        },
    )
    .await;
    send_envelope(
        &mut device,
        ahand_protocol::Envelope {
            device_id: "cp-dev-1".into(),
            msg_id: "ev-2".into(),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::JobEvent(ahand_protocol::JobEvent {
                job_id: job_id.clone(),
                event: Some(ahand_protocol::job_event::Event::Progress(42)),
            })),
            ..Default::default()
        },
    )
    .await;
    send_envelope(
        &mut device,
        ahand_protocol::Envelope {
            device_id: "cp-dev-1".into(),
            msg_id: "ev-3".into(),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::JobFinished(ahand_protocol::JobFinished {
                job_id: job_id.clone(),
                exit_code: 0,
                error: String::new(),
            })),
            ..Default::default()
        },
    )
    .await;

    let body = tokio::time::timeout(Duration::from_secs(5), stream_task)
        .await
        .unwrap()
        .unwrap();
    assert!(body.contains("event: stdout"), "body was: {body}");
    // The chunk must appear intact (the JSON escapes the internal \n
    // so the double-newline pattern below is the frame delimiter).
    assert!(
        body.contains(r#"data: {"chunk":"hi\nworld"}"#),
        "stdout chunk mis-encoded: {body}"
    );
    assert!(body.contains("event: progress"), "body was: {body}");
    assert!(body.contains(r#""percent":42"#), "body was: {body}");
    assert!(body.contains("event: finished"), "body was: {body}");
    assert!(body.contains(r#""exitCode":0"#), "body was: {body}");

    // After the terminal event, the tracker entry should be gone —
    // a follow-up stream request must 404.
    let gone = reqwest::Client::new()
        .get(format!(
            "{}/api/control/jobs/{}/stream",
            server.http_base_url(),
            job_id
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(gone.status(), StatusCode::NOT_FOUND);

    drop(device);
    server.shutdown().await;
}

#[tokio::test]
async fn missing_authorization_returns_401() {
    let server = spawn_server_with_state(test_state().await).await;
    let resp = reqwest::Client::new()
        .post(format!("{}/api/control/jobs", server.http_base_url()))
        .json(&serde_json::json!({ "device_id": "x", "tool": "echo" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    server.shutdown().await;
}

#[tokio::test]
async fn malformed_jwt_returns_401() {
    let server = spawn_server_with_state(test_state().await).await;
    let resp = post_create_job(
        &server,
        "not-a-real-jwt",
        serde_json::json!({ "device_id": "x", "tool": "echo" }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    server.shutdown().await;
}

#[tokio::test]
async fn device_jwt_is_rejected_as_control_plane_token() {
    use ahand_hub_core::auth::mint_device_jwt;
    let server = spawn_server_with_state(test_state().await).await;
    let (device_token, _) = mint_device_jwt(
        JWT_SECRET.as_bytes(),
        "dev-1",
        "user-x",
        Duration::from_secs(60),
    )
    .unwrap();
    let resp = post_create_job(
        &server,
        &device_token,
        serde_json::json!({ "device_id": "x", "tool": "echo" }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    server.shutdown().await;
}

#[tokio::test]
async fn mismatched_external_user_id_returns_403() {
    let server = spawn_server_with_state(test_state().await).await;
    let _device = attach_owned_device(&server, "cp-dev-403", "user-owner").await;
    let token = mint_cp_jwt(&server, "user-attacker");

    let resp = post_create_job(
        &server,
        &token,
        serde_json::json!({
            "device_id": "cp-dev-403",
            "tool": "echo",
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "FORBIDDEN");
    server.shutdown().await;
}

#[tokio::test]
async fn device_without_external_user_id_returns_403() {
    // `device-1` is seeded with external_user_id=None in test_state(),
    // so any control-plane JWT should 403.
    let server = spawn_server_with_state(test_state().await).await;
    let token = mint_cp_jwt(&server, "user-whoever");
    let resp = post_create_job(
        &server,
        &token,
        serde_json::json!({ "device_id": "device-1", "tool": "echo" }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    server.shutdown().await;
}

#[tokio::test]
async fn unknown_device_returns_404() {
    let server = spawn_server_with_state(test_state().await).await;
    let token = mint_cp_jwt(&server, "user-x");
    let resp = post_create_job(
        &server,
        &token,
        serde_json::json!({ "device_id": "ghost", "tool": "echo" }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "DEVICE_NOT_FOUND");
    server.shutdown().await;
}

#[tokio::test]
async fn offline_device_returns_404() {
    let server = spawn_server_with_state(test_state().await).await;
    // Seed an owned device that does NOT have a WS attached.
    server
        .state()
        .devices
        .pre_register("cp-offline", b"pubkey-bytes-ignored", "user-off")
        .await
        .unwrap();
    let token = mint_cp_jwt(&server, "user-off");
    let resp = post_create_job(
        &server,
        &token,
        serde_json::json!({ "device_id": "cp-offline", "tool": "echo" }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "DEVICE_OFFLINE");
    server.shutdown().await;
}

#[tokio::test]
async fn missing_tool_returns_400() {
    let server = spawn_server_with_state(test_state().await).await;
    let _device = attach_owned_device(&server, "cp-missing-tool", "user-m").await;
    let token = mint_cp_jwt(&server, "user-m");
    // Missing `tool` field entirely.
    let resp = post_create_job(
        &server,
        &token,
        serde_json::json!({ "device_id": "cp-missing-tool" }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // Empty `tool`.
    let empty = post_create_job(
        &server,
        &token,
        serde_json::json!({ "device_id": "cp-missing-tool", "tool": "   " }),
    )
    .await;
    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);
    server.shutdown().await;
}

#[tokio::test]
async fn malformed_body_returns_400() {
    let server = spawn_server_with_state(test_state().await).await;
    let token = mint_cp_jwt(&server, "user-x");
    let resp = reqwest::Client::new()
        .post(format!("{}/api/control/jobs", server.http_base_url()))
        .bearer_auth(&token)
        .body("{not json")
        .header("content-type", "application/json")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    server.shutdown().await;
}

#[tokio::test]
async fn missing_device_id_returns_400() {
    let server = spawn_server_with_state(test_state().await).await;
    let token = mint_cp_jwt(&server, "user-x");
    let resp = post_create_job(
        &server,
        &token,
        serde_json::json!({ "device_id": "", "tool": "echo" }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    server.shutdown().await;
}

#[tokio::test]
async fn duplicate_correlation_id_returns_same_job_id() {
    let server = spawn_server_with_state(test_state().await).await;
    let mut device = attach_owned_device(&server, "cp-corr", "user-corr").await;
    let token = mint_cp_jwt(&server, "user-corr");

    let first = post_create_job(
        &server,
        &token,
        serde_json::json!({
            "device_id": "cp-corr",
            "tool": "sleep",
            "args": ["5"],
            "correlation_id": "idem-1",
        }),
    )
    .await;
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    let first_body: serde_json::Value = first.json().await.unwrap();
    let first_id = first_body["job_id"].as_str().unwrap().to_string();

    // Drain the first JobRequest so the second call won't see it on
    // the socket.
    let _received = recv_job_request(&mut device).await;

    let second = post_create_job(
        &server,
        &token,
        serde_json::json!({
            "device_id": "cp-corr",
            "tool": "sleep",
            "args": ["5"],
            "correlation_id": "idem-1",
        }),
    )
    .await;
    // Idempotent re-post returns 200 (not 202) with the same job_id.
    assert_eq!(second.status(), StatusCode::OK);
    let second_body: serde_json::Value = second.json().await.unwrap();
    assert_eq!(second_body["job_id"].as_str(), Some(first_id.as_str()));

    // The daemon should NOT have received a second JobRequest. We
    // probe the socket briefly; if a second request arrives within
    // 200ms we fail the test.
    let no_second = tokio::time::timeout(
        Duration::from_millis(200),
        recv_job_request(&mut device),
    )
    .await;
    assert!(
        no_second.is_err(),
        "duplicate correlation_id unexpectedly re-dispatched a job"
    );

    drop(device);
    server.shutdown().await;
}

#[tokio::test]
async fn correlation_id_per_user_does_not_collide() {
    // Same correlation id, different users → different jobs.
    let server = spawn_server_with_state(test_state().await).await;
    let mut a_device = attach_owned_device(&server, "cp-corr-a", "user-a").await;
    let mut b_device = attach_owned_device(&server, "cp-corr-b", "user-b").await;
    let a_token = mint_cp_jwt(&server, "user-a");
    let b_token = mint_cp_jwt(&server, "user-b");

    let a_resp = post_create_job(
        &server,
        &a_token,
        serde_json::json!({
            "device_id": "cp-corr-a",
            "tool": "echo",
            "correlation_id": "shared",
        }),
    )
    .await;
    assert_eq!(a_resp.status(), StatusCode::ACCEPTED);
    let a_id = a_resp.json::<serde_json::Value>().await.unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_string();

    let b_resp = post_create_job(
        &server,
        &b_token,
        serde_json::json!({
            "device_id": "cp-corr-b",
            "tool": "echo",
            "correlation_id": "shared",
        }),
    )
    .await;
    assert_eq!(b_resp.status(), StatusCode::ACCEPTED);
    let b_id = b_resp.json::<serde_json::Value>().await.unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_string();

    assert_ne!(a_id, b_id);
    let _ = recv_job_request(&mut a_device).await;
    let _ = recv_job_request(&mut b_device).await;

    drop(a_device);
    drop(b_device);
    server.shutdown().await;
}

#[tokio::test]
async fn rate_limit_returns_429() {
    let server = spawn_server_with_state(test_state().await).await;
    let _device = attach_owned_device(&server, "cp-rl", "user-rl").await;
    let token = mint_cp_jwt(&server, "user-rl");

    // Default limiter: burst=100, rps=10. Blast ~150 rapid requests
    // and assert at least one comes back 429.
    let mut statuses = Vec::new();
    for i in 0..150 {
        let resp = post_create_job(
            &server,
            &token,
            serde_json::json!({
                "device_id": "cp-rl",
                "tool": "echo",
                "correlation_id": format!("burst-{i}"),
            }),
        )
        .await;
        statuses.push(resp.status());
        if resp.status() == StatusCode::TOO_MANY_REQUESTS {
            break;
        }
    }
    assert!(
        statuses
            .iter()
            .any(|s| *s == StatusCode::TOO_MANY_REQUESTS),
        "expected at least one 429 in {statuses:?}"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn stream_unknown_job_returns_404() {
    let server = spawn_server_with_state(test_state().await).await;
    let token = mint_cp_jwt(&server, "user-x");
    let resp = reqwest::Client::new()
        .get(format!(
            "{}/api/control/jobs/nosuchjob/stream",
            server.http_base_url()
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    server.shutdown().await;
}

#[tokio::test]
async fn stream_other_users_job_returns_404_not_403() {
    let server = spawn_server_with_state(test_state().await).await;
    let mut device = attach_owned_device(&server, "cp-xuser", "user-owner").await;
    let owner_token = mint_cp_jwt(&server, "user-owner");
    let resp = post_create_job(
        &server,
        &owner_token,
        serde_json::json!({ "device_id": "cp-xuser", "tool": "echo" }),
    )
    .await;
    let job_id = resp.json::<serde_json::Value>().await.unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let _ = recv_job_request(&mut device).await;

    let attacker_token = mint_cp_jwt(&server, "user-attacker");
    let stream = reqwest::Client::new()
        .get(format!(
            "{}/api/control/jobs/{}/stream",
            server.http_base_url(),
            job_id
        ))
        .bearer_auth(&attacker_token)
        .send()
        .await
        .unwrap();
    assert_eq!(stream.status(), StatusCode::NOT_FOUND);

    let cancel = reqwest::Client::new()
        .post(format!(
            "{}/api/control/jobs/{}/cancel",
            server.http_base_url(),
            job_id
        ))
        .bearer_auth(&attacker_token)
        .send()
        .await
        .unwrap();
    assert_eq!(cancel.status(), StatusCode::NOT_FOUND);

    drop(device);
    server.shutdown().await;
}

#[tokio::test]
async fn cancel_routes_cancel_envelope_and_returns_202() {
    let server = spawn_server_with_state(test_state().await).await;
    let mut device = attach_owned_device(&server, "cp-cancel", "user-c").await;
    let token = mint_cp_jwt(&server, "user-c");

    let resp = post_create_job(
        &server,
        &token,
        serde_json::json!({
            "device_id": "cp-cancel",
            "tool": "sleep",
            "args": ["30"],
        }),
    )
    .await;
    let job_id = resp.json::<serde_json::Value>().await.unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let _ = recv_job_request(&mut device).await;

    let cancel = reqwest::Client::new()
        .post(format!(
            "{}/api/control/jobs/{}/cancel",
            server.http_base_url(),
            job_id
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(cancel.status(), StatusCode::ACCEPTED);

    let envelope = recv_cancel(&mut device).await;
    assert_eq!(envelope.job_id, job_id);

    drop(device);
    server.shutdown().await;
}

#[tokio::test]
async fn cancel_unknown_job_returns_404() {
    let server = spawn_server_with_state(test_state().await).await;
    let token = mint_cp_jwt(&server, "user-x");
    let resp = reqwest::Client::new()
        .post(format!(
            "{}/api/control/jobs/ghost/cancel",
            server.http_base_url()
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    server.shutdown().await;
}

#[tokio::test]
async fn two_concurrent_sse_clients_receive_all_events() {
    let server = spawn_server_with_state(test_state().await).await;
    let mut device = attach_owned_device(&server, "cp-fanout", "user-f").await;
    let token = mint_cp_jwt(&server, "user-f");

    let job_id = post_create_job(
        &server,
        &token,
        serde_json::json!({ "device_id": "cp-fanout", "tool": "echo" }),
    )
    .await
    .json::<serde_json::Value>()
    .await
    .unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let _ = recv_job_request(&mut device).await;

    let spawn_sse = {
        let server_url = server.http_base_url().to_string();
        let token = token.clone();
        let job_id = job_id.clone();
        move || {
            let server_url = server_url.clone();
            let token = token.clone();
            let job_id = job_id.clone();
            tokio::spawn(async move {
                let resp = reqwest::Client::new()
                    .get(format!("{server_url}/api/control/jobs/{job_id}/stream"))
                    .bearer_auth(&token)
                    .send()
                    .await
                    .unwrap();
                let mut stream = resp.bytes_stream();
                let mut body = String::new();
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.unwrap();
                    body.push_str(&String::from_utf8_lossy(&chunk));
                    if body.contains("event: finished") {
                        break;
                    }
                }
                body
            })
        }
    };
    let a = spawn_sse();
    let b = spawn_sse();

    tokio::time::sleep(Duration::from_millis(120)).await;

    send_envelope(
        &mut device,
        ahand_protocol::Envelope {
            device_id: "cp-fanout".into(),
            msg_id: "ev".into(),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::JobEvent(ahand_protocol::JobEvent {
                job_id: job_id.clone(),
                event: Some(ahand_protocol::job_event::Event::StdoutChunk(
                    b"broadcast".to_vec(),
                )),
            })),
            ..Default::default()
        },
    )
    .await;
    send_envelope(
        &mut device,
        ahand_protocol::Envelope {
            device_id: "cp-fanout".into(),
            msg_id: "fin".into(),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::JobFinished(ahand_protocol::JobFinished {
                job_id: job_id.clone(),
                exit_code: 0,
                error: String::new(),
            })),
            ..Default::default()
        },
    )
    .await;

    let body_a = tokio::time::timeout(Duration::from_secs(5), a)
        .await
        .unwrap()
        .unwrap();
    let body_b = tokio::time::timeout(Duration::from_secs(5), b)
        .await
        .unwrap()
        .unwrap();
    for body in [&body_a, &body_b] {
        assert!(body.contains("broadcast"), "body missing stdout: {body}");
        assert!(body.contains("event: finished"), "body missing finished: {body}");
    }

    drop(device);
    server.shutdown().await;
}

#[tokio::test]
async fn sse_client_disconnect_cleans_up_on_terminal_event() {
    // Open an SSE stream, drop it early, and assert the tracker
    // entry is still there (broadcast::Sender outlives dropped
    // receivers). Then finalize via the daemon and assert the tracker
    // is empty.
    let server = spawn_server_with_state(test_state().await).await;
    let mut device = attach_owned_device(&server, "cp-dc", "user-d").await;
    let token = mint_cp_jwt(&server, "user-d");

    let job_id = post_create_job(
        &server,
        &token,
        serde_json::json!({ "device_id": "cp-dc", "tool": "echo" }),
    )
    .await
    .json::<serde_json::Value>()
    .await
    .unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let _ = recv_job_request(&mut device).await;

    // Open SSE, receive nothing, then drop.
    {
        let resp = reqwest::Client::new()
            .get(format!(
                "{}/api/control/jobs/{}/stream",
                server.http_base_url(),
                job_id
            ))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        // Drop without reading.
        drop(resp);
    }
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Entry still present because sender owns it until a terminal event.
    assert!(server.state().control_jobs.get(&job_id).is_some());

    // Finalize.
    send_envelope(
        &mut device,
        ahand_protocol::Envelope {
            device_id: "cp-dc".into(),
            msg_id: "fin".into(),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::JobFinished(ahand_protocol::JobFinished {
                job_id: job_id.clone(),
                exit_code: 0,
                error: String::new(),
            })),
            ..Default::default()
        },
    )
    .await;

    // Wait for the event to be processed.
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        if server.state().control_jobs.get(&job_id).is_none() {
            break;
        }
    }
    assert!(
        server.state().control_jobs.get(&job_id).is_none(),
        "tracker entry leaked after terminal event"
    );

    drop(device);
    server.shutdown().await;
}

#[tokio::test]
async fn many_sse_disconnects_do_not_leak_entries() {
    // 100 open-and-drop cycles on the same job id. The tracker
    // holds ONE entry (broadcast receivers come and go). This
    // exercises the "no memory growth after 100 disconnects"
    // criterion — finalize at the end and assert tracker is empty.
    let server = spawn_server_with_state(test_state().await).await;
    let mut device = attach_owned_device(&server, "cp-stress", "user-s").await;
    let token = mint_cp_jwt(&server, "user-s");

    let job_id = post_create_job(
        &server,
        &token,
        serde_json::json!({ "device_id": "cp-stress", "tool": "echo" }),
    )
    .await
    .json::<serde_json::Value>()
    .await
    .unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let _ = recv_job_request(&mut device).await;

    for _ in 0..100 {
        let resp = reqwest::Client::new()
            .get(format!(
                "{}/api/control/jobs/{}/stream",
                server.http_base_url(),
                job_id
            ))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        drop(resp);
    }

    assert_eq!(server.state().control_jobs.len(), 1);

    send_envelope(
        &mut device,
        ahand_protocol::Envelope {
            device_id: "cp-stress".into(),
            msg_id: "fin".into(),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::JobFinished(ahand_protocol::JobFinished {
                job_id: job_id.clone(),
                exit_code: 0,
                error: String::new(),
            })),
            ..Default::default()
        },
    )
    .await;

    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        if server.state().control_jobs.is_empty() {
            break;
        }
    }
    assert!(server.state().control_jobs.is_empty());

    drop(device);
    server.shutdown().await;
}

#[tokio::test]
async fn large_stdout_chunk_delivered_intact() {
    // Synthetic 1.5 MB chunk containing newlines, to verify that JSON
    // escaping prevents any mis-split on `\n\n`.
    let server = spawn_server_with_state(test_state().await).await;
    let mut device = attach_owned_device(&server, "cp-big", "user-big").await;
    let token = mint_cp_jwt(&server, "user-big");

    let job_id = post_create_job(
        &server,
        &token,
        serde_json::json!({ "device_id": "cp-big", "tool": "echo" }),
    )
    .await
    .json::<serde_json::Value>()
    .await
    .unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let _ = recv_job_request(&mut device).await;

    // Build a chunk that is 1.5 MB of ASCII with `\n` sprinkled every
    // 128 bytes, including an explicit `\n\n` sequence to stress the
    // SSE delimiter logic.
    let mut chunk = Vec::with_capacity(1_500_000);
    while chunk.len() < 1_500_000 {
        chunk.extend_from_slice(b"abcdefg\n");
        if chunk.len() % 1024 == 0 {
            chunk.push(b'\n');
        }
    }
    let chunk_len = chunk.len();

    let stream_task = {
        let server_url = server.http_base_url().to_string();
        let token = token.clone();
        let job_id = job_id.clone();
        tokio::spawn(async move {
            let resp = reqwest::Client::new()
                .get(format!("{server_url}/api/control/jobs/{job_id}/stream"))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap();
            let mut stream = resp.bytes_stream();
            let mut body = Vec::new();
            while let Some(c) = stream.next().await {
                let c = c.unwrap();
                body.extend_from_slice(&c);
                // Stop once finished arrives.
                if std::str::from_utf8(&body)
                    .map(|s| s.contains("event: finished"))
                    .unwrap_or(false)
                {
                    break;
                }
            }
            body
        })
    };

    tokio::time::sleep(Duration::from_millis(120)).await;

    send_envelope(
        &mut device,
        ahand_protocol::Envelope {
            device_id: "cp-big".into(),
            msg_id: "big".into(),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::JobEvent(ahand_protocol::JobEvent {
                job_id: job_id.clone(),
                event: Some(ahand_protocol::job_event::Event::StdoutChunk(
                    chunk.clone(),
                )),
            })),
            ..Default::default()
        },
    )
    .await;
    send_envelope(
        &mut device,
        ahand_protocol::Envelope {
            device_id: "cp-big".into(),
            msg_id: "fin".into(),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::JobFinished(ahand_protocol::JobFinished {
                job_id: job_id.clone(),
                exit_code: 0,
                error: String::new(),
            })),
            ..Default::default()
        },
    )
    .await;

    let body = tokio::time::timeout(Duration::from_secs(30), stream_task)
        .await
        .unwrap()
        .unwrap();
    let body_str = std::str::from_utf8(&body).expect("SSE body must be UTF-8");

    // Parse the stdout event's data line as JSON and reconstruct the
    // chunk. SSE has one frame per `\n\n`; find the `event: stdout`
    // block and extract its `data:` line.
    let stdout_block = body_str
        .split("\n\n")
        .find(|block| block.contains("event: stdout"))
        .unwrap_or_else(|| panic!("no stdout block in body of len {}", body_str.len()));
    let data_line = stdout_block
        .lines()
        .find(|l| l.starts_with("data: "))
        .expect("stdout block had no data line");
    let json_payload = &data_line[6..];
    let parsed: serde_json::Value = serde_json::from_str(json_payload).expect("valid JSON payload");
    let decoded_chunk = parsed["chunk"].as_str().expect("chunk is a string").to_string();
    assert_eq!(
        decoded_chunk.len(),
        chunk_len,
        "round-tripped chunk length mismatch"
    );

    drop(device);
    server.shutdown().await;
}

#[tokio::test]
async fn rejected_envelope_finalizes_as_error() {
    // Daemon-side policy rejects the job → SSE surface should emit
    // an `error` event and remove the tracker entry.
    let server = spawn_server_with_state(test_state().await).await;
    let mut device = attach_owned_device(&server, "cp-reject", "user-r").await;
    let token = mint_cp_jwt(&server, "user-r");

    let job_id = post_create_job(
        &server,
        &token,
        serde_json::json!({ "device_id": "cp-reject", "tool": "curl" }),
    )
    .await
    .json::<serde_json::Value>()
    .await
    .unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let _ = recv_job_request(&mut device).await;

    let stream_task = {
        let server_url = server.http_base_url().to_string();
        let token = token.clone();
        let job_id = job_id.clone();
        tokio::spawn(async move {
            let resp = reqwest::Client::new()
                .get(format!("{server_url}/api/control/jobs/{job_id}/stream"))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap();
            let mut stream = resp.bytes_stream();
            let mut body = String::new();
            while let Some(c) = stream.next().await {
                body.push_str(&String::from_utf8_lossy(&c.unwrap()));
                if body.contains("event: error") {
                    break;
                }
            }
            body
        })
    };

    tokio::time::sleep(Duration::from_millis(120)).await;

    send_envelope(
        &mut device,
        ahand_protocol::Envelope {
            device_id: "cp-reject".into(),
            msg_id: "rej".into(),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::JobRejected(ahand_protocol::JobRejected {
                job_id: job_id.clone(),
                reason: "policy-denied".into(),
            })),
            ..Default::default()
        },
    )
    .await;

    let body = tokio::time::timeout(Duration::from_secs(5), stream_task)
        .await
        .unwrap()
        .unwrap();
    assert!(body.contains("event: error"), "body was {body}");
    assert!(body.contains(r#""code":"rejected""#), "body was {body}");
    assert!(body.contains(r#""message":"policy-denied""#), "body was {body}");

    drop(device);
    server.shutdown().await;
}

#[tokio::test]
async fn exit_code_non_zero_reports_error_event() {
    let server = spawn_server_with_state(test_state().await).await;
    let mut device = attach_owned_device(&server, "cp-exit", "user-e").await;
    let token = mint_cp_jwt(&server, "user-e");

    let job_id = post_create_job(
        &server,
        &token,
        serde_json::json!({ "device_id": "cp-exit", "tool": "false" }),
    )
    .await
    .json::<serde_json::Value>()
    .await
    .unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let _ = recv_job_request(&mut device).await;

    let stream_task = {
        let server_url = server.http_base_url().to_string();
        let token = token.clone();
        let job_id = job_id.clone();
        tokio::spawn(async move {
            let resp = reqwest::Client::new()
                .get(format!("{server_url}/api/control/jobs/{job_id}/stream"))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap();
            let mut stream = resp.bytes_stream();
            let mut body = String::new();
            while let Some(c) = stream.next().await {
                body.push_str(&String::from_utf8_lossy(&c.unwrap()));
                if body.contains("event: error") {
                    break;
                }
            }
            body
        })
    };
    tokio::time::sleep(Duration::from_millis(120)).await;

    send_envelope(
        &mut device,
        ahand_protocol::Envelope {
            device_id: "cp-exit".into(),
            msg_id: "fin".into(),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::JobFinished(ahand_protocol::JobFinished {
                job_id: job_id.clone(),
                exit_code: 42,
                error: String::new(),
            })),
            ..Default::default()
        },
    )
    .await;

    let body = tokio::time::timeout(Duration::from_secs(5), stream_task)
        .await
        .unwrap()
        .unwrap();
    assert!(body.contains("event: error"), "body was {body}");
    assert!(body.contains(r#""code":"exec_failed""#), "body was {body}");

    drop(device);
    server.shutdown().await;
}

#[tokio::test]
async fn cancelled_finish_reports_cancelled_error_code() {
    let server = spawn_server_with_state(test_state().await).await;
    let mut device = attach_owned_device(&server, "cp-canc-code", "user-cc").await;
    let token = mint_cp_jwt(&server, "user-cc");

    let job_id = post_create_job(
        &server,
        &token,
        serde_json::json!({ "device_id": "cp-canc-code", "tool": "sleep" }),
    )
    .await
    .json::<serde_json::Value>()
    .await
    .unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let _ = recv_job_request(&mut device).await;

    let stream_task = {
        let server_url = server.http_base_url().to_string();
        let token = token.clone();
        let job_id = job_id.clone();
        tokio::spawn(async move {
            let resp = reqwest::Client::new()
                .get(format!("{server_url}/api/control/jobs/{job_id}/stream"))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap();
            let mut stream = resp.bytes_stream();
            let mut body = String::new();
            while let Some(c) = stream.next().await {
                body.push_str(&String::from_utf8_lossy(&c.unwrap()));
                if body.contains("event: error") {
                    break;
                }
            }
            body
        })
    };
    tokio::time::sleep(Duration::from_millis(120)).await;

    send_envelope(
        &mut device,
        ahand_protocol::Envelope {
            device_id: "cp-canc-code".into(),
            msg_id: "fin".into(),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::JobFinished(ahand_protocol::JobFinished {
                job_id: job_id.clone(),
                exit_code: -1,
                error: "cancelled".into(),
            })),
            ..Default::default()
        },
    )
    .await;

    let body = tokio::time::timeout(Duration::from_secs(5), stream_task)
        .await
        .unwrap()
        .unwrap();
    assert!(body.contains(r#""code":"cancelled""#), "body was {body}");

    drop(device);
    server.shutdown().await;
}

#[tokio::test]
async fn stderr_and_progress_with_message_render_correctly() {
    // Covers the stderr + progress SSE render branches explicitly.
    // (Progress-with-message is reserved for future use but we
    // exercise the branch via the unit test on ControlJobEvent; here
    // we cover the stderr wire path only.)
    let server = spawn_server_with_state(test_state().await).await;
    let mut device = attach_owned_device(&server, "cp-err-ch", "user-ec").await;
    let token = mint_cp_jwt(&server, "user-ec");

    let job_id = post_create_job(
        &server,
        &token,
        serde_json::json!({ "device_id": "cp-err-ch", "tool": "echo" }),
    )
    .await
    .json::<serde_json::Value>()
    .await
    .unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let _ = recv_job_request(&mut device).await;

    let stream_task = {
        let server_url = server.http_base_url().to_string();
        let token = token.clone();
        let job_id = job_id.clone();
        tokio::spawn(async move {
            let resp = reqwest::Client::new()
                .get(format!("{server_url}/api/control/jobs/{job_id}/stream"))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap();
            let mut stream = resp.bytes_stream();
            let mut body = String::new();
            while let Some(c) = stream.next().await {
                body.push_str(&String::from_utf8_lossy(&c.unwrap()));
                if body.contains("event: finished") {
                    break;
                }
            }
            body
        })
    };
    tokio::time::sleep(Duration::from_millis(120)).await;

    send_envelope(
        &mut device,
        ahand_protocol::Envelope {
            device_id: "cp-err-ch".into(),
            msg_id: "e1".into(),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::JobEvent(ahand_protocol::JobEvent {
                job_id: job_id.clone(),
                event: Some(ahand_protocol::job_event::Event::StderrChunk(
                    b"boom".to_vec(),
                )),
            })),
            ..Default::default()
        },
    )
    .await;
    send_envelope(
        &mut device,
        ahand_protocol::Envelope {
            device_id: "cp-err-ch".into(),
            msg_id: "fin".into(),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::JobFinished(ahand_protocol::JobFinished {
                job_id: job_id.clone(),
                exit_code: 0,
                error: String::new(),
            })),
            ..Default::default()
        },
    )
    .await;

    let body = tokio::time::timeout(Duration::from_secs(5), stream_task)
        .await
        .unwrap()
        .unwrap();
    assert!(body.contains("event: stderr"), "body was {body}");
    assert!(body.contains(r#""chunk":"boom""#), "body was {body}");

    drop(device);
    server.shutdown().await;
}

#[tokio::test]
async fn sse_late_joiner_after_terminal_event_gets_empty_stream() {
    // Test that a client connecting AFTER the job finishes gets a 404.
    // finalize() removes the tracker entry when a terminal event is
    // published; any subsequent GET /stream will find no entry and
    // return 404 immediately rather than hanging or panicking.
    // This is the documented behavior for late-joiner clients — the
    // SDK layer (CloudClient::spawn) is responsible for connecting
    // the SSE stream before or concurrent with the POST /jobs call.
    let server = spawn_server_with_state(test_state().await).await;
    let mut device = attach_owned_device(&server, "cp-late", "user-late").await;
    let token = mint_cp_jwt(&server, "user-late");

    // Dispatch the job.
    let job_id = post_create_job(
        &server,
        &token,
        serde_json::json!({ "device_id": "cp-late", "tool": "echo" }),
    )
    .await
    .json::<serde_json::Value>()
    .await
    .unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let _ = recv_job_request(&mut device).await;

    // Finalize the job via a terminal event BEFORE the client connects to SSE.
    send_envelope(
        &mut device,
        ahand_protocol::Envelope {
            device_id: "cp-late".into(),
            msg_id: "fin-late".into(),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::JobFinished(ahand_protocol::JobFinished {
                job_id: job_id.clone(),
                exit_code: 0,
                error: String::new(),
            })),
            ..Default::default()
        },
    )
    .await;

    // Wait for the tracker entry to be removed by the finalize path.
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        if server.state().control_jobs.get(&job_id).is_none() {
            break;
        }
    }
    assert!(
        server.state().control_jobs.get(&job_id).is_none(),
        "tracker entry should be gone after terminal event"
    );

    // Now connect SSE as a late joiner — job entry is already cleaned up.
    // Expect 404: the tracker has no entry for this job_id.
    let resp = reqwest::Client::new()
        .get(format!(
            "{}/api/control/jobs/{}/stream",
            server.http_base_url(),
            job_id
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "late-joiner should get 404 (job entry cleaned up after finalize), got {}",
        resp.status()
    );

    drop(device);
    server.shutdown().await;
}

// ── R2-1: device_ids allowlist enforcement ────────────────────────────────────

#[tokio::test]
async fn create_job_rejects_device_not_in_allowlist() {
    // Token scoped to "other-device" but posting to "dev-al-1" → 403.
    let server = spawn_server_with_state(test_state().await).await;
    let _device = attach_owned_device(&server, "dev-al-1", "user-allow").await;
    let token = mint_cp_jwt_with_options(
        "user-allow",
        "jobs:execute",
        Some(vec!["other-device".to_string()]),
    );
    let resp = post_create_job(
        &server,
        &token,
        serde_json::json!({ "device_id": "dev-al-1", "tool": "echo" }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "FORBIDDEN");
    server.shutdown().await;
}

#[tokio::test]
async fn create_job_allows_device_in_allowlist() {
    // Token scoped to exactly "dev-al-2" — posting to that device → 202.
    let server = spawn_server_with_state(test_state().await).await;
    let _device = attach_owned_device(&server, "dev-al-2", "user-al2").await;
    let token = mint_cp_jwt_with_options(
        "user-al2",
        "jobs:execute",
        Some(vec!["dev-al-2".to_string()]),
    );
    let resp = post_create_job(
        &server,
        &token,
        serde_json::json!({ "device_id": "dev-al-2", "tool": "echo" }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    server.shutdown().await;
}

// ── R3-3: allowlist enforcement on stream_job and cancel_job ─────────────────

#[tokio::test]
async fn stream_job_rejects_device_not_in_allowlist() {
    // Mint a full-scope token to create the job on "dev-stream-al", then
    // attempt to stream it with a restricted token whose device_ids=["other"]
    // → 403.
    let server = spawn_server_with_state(test_state().await).await;
    let mut device = attach_owned_device(&server, "dev-stream-al", "user-stream-al").await;
    let full_token = mint_cp_jwt(&server, "user-stream-al");

    // Create the job with the full-scope token.
    let resp = post_create_job(
        &server,
        &full_token,
        serde_json::json!({
            "device_id": "dev-stream-al",
            "tool": "sleep",
            "args": ["30"],
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let job_id = resp.json::<serde_json::Value>().await.unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let _ = recv_job_request(&mut device).await;

    // Attempt to stream with a restricted token scoped to a different device.
    let restricted_token = mint_cp_jwt_with_options(
        "user-stream-al",
        "jobs:execute",
        Some(vec!["other".to_string()]),
    );
    let stream_resp = reqwest::Client::new()
        .get(format!(
            "{}/api/control/jobs/{}/stream",
            server.http_base_url(),
            job_id
        ))
        .bearer_auth(&restricted_token)
        .send()
        .await
        .unwrap();
    assert_eq!(stream_resp.status(), StatusCode::FORBIDDEN);
    let body: serde_json::Value = stream_resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "FORBIDDEN");

    drop(device);
    server.shutdown().await;
}

#[tokio::test]
async fn cancel_job_rejects_device_not_in_allowlist() {
    // Mint a full-scope token to create the job on "dev-cancel-al", then
    // attempt to cancel it with a restricted token whose device_ids=["other"]
    // → 403.
    let server = spawn_server_with_state(test_state().await).await;
    let mut device = attach_owned_device(&server, "dev-cancel-al", "user-cancel-al").await;
    let full_token = mint_cp_jwt(&server, "user-cancel-al");

    // Create the job with the full-scope token.
    let resp = post_create_job(
        &server,
        &full_token,
        serde_json::json!({
            "device_id": "dev-cancel-al",
            "tool": "sleep",
            "args": ["30"],
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let job_id = resp.json::<serde_json::Value>().await.unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let _ = recv_job_request(&mut device).await;

    // Attempt to cancel with a restricted token scoped to a different device.
    let restricted_token = mint_cp_jwt_with_options(
        "user-cancel-al",
        "jobs:execute",
        Some(vec!["other".to_string()]),
    );
    let cancel_resp = reqwest::Client::new()
        .post(format!(
            "{}/api/control/jobs/{}/cancel",
            server.http_base_url(),
            job_id
        ))
        .bearer_auth(&restricted_token)
        .send()
        .await
        .unwrap();
    assert_eq!(cancel_resp.status(), StatusCode::FORBIDDEN);
    let body: serde_json::Value = cancel_resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "FORBIDDEN");

    drop(device);
    server.shutdown().await;
}

// ── R2-5: scope claim validation ──────────────────────────────────────────────

#[tokio::test]
async fn create_job_rejects_wrong_scope() {
    // Token with scope "jobs:read" must be rejected 403 before any DB work.
    let server = spawn_server_with_state(test_state().await).await;
    let _device = attach_owned_device(&server, "dev-scope", "user-scope").await;
    let token = mint_cp_jwt_with_options("user-scope", "jobs:read", None);
    let resp = post_create_job(
        &server,
        &token,
        serde_json::json!({ "device_id": "dev-scope", "tool": "echo" }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "FORBIDDEN");
    server.shutdown().await;
}
