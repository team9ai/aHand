//! Integration tests for the `POST /api/devices/{id}/files` endpoint.
//!
//! These tests drive a real in-process hub server via the support helpers
//! and a fake device that speaks raw protobuf over the WebSocket gateway.
//! They cover the HTTP surface (auth, body encoding, offline/malformed/empty
//! cases) — the device-side file handlers are tested exhaustively in the
//! ahandd crate's file_ops integration tests.

mod support;

use std::time::Duration;

use ahand_protocol::{
    FileError, FileErrorCode, FileReadText, FileRequest, FileResponse, FileStatResult, FileType,
    file_request, file_response,
};
use prost::Message;

use support::{TestServer, spawn_test_server};

const PROTOBUF_CONTENT_TYPE: &str = "application/x-protobuf";

fn encoded_read_text(path: &str, request_id: &str) -> Vec<u8> {
    let req = FileRequest {
        request_id: request_id.into(),
        operation: Some(file_request::Operation::ReadText(FileReadText {
            path: path.into(),
            start: None,
            max_lines: None,
            max_bytes: None,
            target_end: None,
            max_line_width: None,
            encoding: None,
            line_numbers: true,
            no_follow_symlink: false,
        })),
    };
    req.encode_to_vec()
}

async fn dashboard_token(server: &TestServer) -> String {
    server
        .state()
        .auth
        .issue_dashboard_jwt("operator-1")
        .unwrap()
}

// ── Tests ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn file_operation_happy_path_returns_device_response() {
    let server = spawn_test_server().await;
    let token = dashboard_token(&server).await;
    let mut device = server
        .attach_bootstrap_device("device-2", "bootstrap-test-token")
        .await;

    // Spawn the HTTP request in the background so we can service the device
    // side concurrently.
    let url = format!("{}/api/devices/device-2/files", server.http_base_url());
    let body = encoded_read_text("/tmp/fake.txt", "req-happy");
    let token_clone = token.clone();
    let http_handle = tokio::spawn(async move {
        reqwest::Client::new()
            .post(&url)
            .bearer_auth(&token_clone)
            .header("content-type", PROTOBUF_CONTENT_TYPE)
            .body(body)
            .send()
            .await
            .unwrap()
    });

    // Device receives the FileRequest and replies with a synthetic Stat result
    // so we can verify the full round trip.
    let received = device.recv_file_request().await;
    assert_eq!(received.request_id, "req-happy");

    let canned = FileResponse {
        request_id: received.request_id.clone(),
        result: Some(file_response::Result::Stat(FileStatResult {
            path: "/tmp/fake.txt".into(),
            file_type: FileType::File as i32,
            size: 42,
            modified_ms: 0,
            created_ms: 0,
            accessed_ms: 0,
            unix_permission: None,
            windows_acl: None,
            symlink_target: None,
        })),
    };
    device.send_file_response(canned.clone()).await;

    let http_response = http_handle.await.unwrap();
    assert_eq!(http_response.status().as_u16(), 200);
    assert_eq!(
        http_response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some(PROTOBUF_CONTENT_TYPE)
    );
    let bytes = http_response.bytes().await.unwrap();
    let decoded = FileResponse::decode(bytes.as_ref()).unwrap();
    assert_eq!(decoded.request_id, "req-happy");
    match decoded.result {
        Some(file_response::Result::Stat(stat)) => {
            assert_eq!(stat.size, 42);
        }
        other => panic!("expected Stat result, got {other:?}"),
    }

    server.shutdown().await;
}

#[tokio::test]
async fn file_operation_unauthenticated_returns_401() {
    let server = spawn_test_server().await;
    let url = format!("{}/api/devices/device-2/files", server.http_base_url());
    let body = encoded_read_text("/tmp/fake.txt", "req-noauth");
    let response = reqwest::Client::new()
        .post(&url)
        .header("content-type", PROTOBUF_CONTENT_TYPE)
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 401);
    server.shutdown().await;
}

#[tokio::test]
async fn file_operation_device_offline_returns_409() {
    let server = spawn_test_server().await;
    let token = dashboard_token(&server).await;
    // Target a device that is NOT connected. `device-2` was pre-registered
    // by bootstrap but never attached.
    let url = format!("{}/api/devices/device-2/files", server.http_base_url());
    let body = encoded_read_text("/tmp/fake.txt", "req-offline");
    let response = reqwest::Client::new()
        .post(&url)
        .bearer_auth(&token)
        .header("content-type", PROTOBUF_CONTENT_TYPE)
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 409);
    server.shutdown().await;
}

#[tokio::test]
async fn file_operation_empty_body_returns_400() {
    let server = spawn_test_server().await;
    let token = dashboard_token(&server).await;
    let url = format!("{}/api/devices/device-2/files", server.http_base_url());
    let response = reqwest::Client::new()
        .post(&url)
        .bearer_auth(&token)
        .header("content-type", PROTOBUF_CONTENT_TYPE)
        .body(Vec::<u8>::new())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 400);
    server.shutdown().await;
}

#[tokio::test]
async fn file_operation_rejects_non_protobuf_content_type() {
    // R15 regression: the handler documents that the request body must be
    // `application/x-protobuf`. A client that sends a valid protobuf body
    // but mislabels it (e.g. `application/json`) should get a loud 415
    // instead of a silent decode, so schema confusion surfaces during
    // integration rather than deep inside the device runtime.
    let server = spawn_test_server().await;
    let token = dashboard_token(&server).await;
    let url = format!("{}/api/devices/device-2/files", server.http_base_url());
    let body = encoded_read_text("/tmp/fake.txt", "req-wrong-ct");
    let response = reqwest::Client::new()
        .post(&url)
        .bearer_auth(&token)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 415);
    server.shutdown().await;
}

#[tokio::test]
async fn file_operation_malformed_proto_returns_400() {
    let server = spawn_test_server().await;
    let token = dashboard_token(&server).await;
    let url = format!("{}/api/devices/device-2/files", server.http_base_url());
    // Random bytes that are not a valid FileRequest.
    let body = vec![0xFFu8; 32];
    let response = reqwest::Client::new()
        .post(&url)
        .bearer_auth(&token)
        .header("content-type", PROTOBUF_CONTENT_TYPE)
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 400);
    server.shutdown().await;
}

#[tokio::test]
async fn file_response_from_wrong_device_does_not_resolve_other_waiter() {
    // T12 regression via the full HTTP path. Two devices connect; the
    // request is routed to device-a but device-b sends a response with
    // the same request_id. Device-a's HTTP waiter should NOT be resolved
    // by device-b's response.
    let server = spawn_test_server().await;
    let token = dashboard_token(&server).await;

    // Seed device-3 with the well-known ed25519 test key so we can attach
    // it via `attach_test_device`. Bootstrap is single-use and device-2
    // already claimed that path.
    use ahand_hub_core::device::NewDevice;
    use ahand_hub_core::traits::DeviceStore;
    use ed25519_dalek::SigningKey;
    let test_key_bytes = SigningKey::from_bytes(&[7u8; 32])
        .verifying_key()
        .to_bytes()
        .to_vec();
    server
        .state()
        .devices
        .insert(NewDevice {
            id: "device-3".into(),
            public_key: Some(test_key_bytes),
            hostname: "test-device-3".into(),
            os: "linux".into(),
            capabilities: vec!["exec".into()],
            version: Some("0.1.2".into()),
            auth_method: "ed25519".into(),
            external_user_id: None,
        })
        .await
        .unwrap();
    server
        .state()
        .devices
        .mark_offline("device-3")
        .await
        .unwrap();

    let mut device_a = server
        .attach_bootstrap_device("device-2", "bootstrap-test-token")
        .await;
    let mut device_b = server.attach_test_device("device-3").await;

    let url_a = format!("{}/api/devices/device-2/files", server.http_base_url());
    let body = encoded_read_text("/tmp/fake.txt", "colliding-id");
    let token_clone = token.clone();
    let http_handle = tokio::spawn(async move {
        reqwest::Client::new()
            .post(&url_a)
            .bearer_auth(&token_clone)
            .header("content-type", PROTOBUF_CONTENT_TYPE)
            .body(body)
            .send()
            .await
            .unwrap()
    });

    // Device A receives the request.
    let received_by_a = device_a.recv_file_request().await;
    assert_eq!(received_by_a.request_id, "colliding-id");

    // Device B (uninvolved) sends a response with the same request_id.
    // The HTTP waiter for device-a MUST NOT be resolved by it.
    device_b
        .send_file_response(FileResponse {
            request_id: "colliding-id".into(),
            result: Some(file_response::Result::Error(FileError {
                code: FileErrorCode::Unspecified as i32,
                message: "from-wrong-device".into(),
                path: String::new(),
            })),
        })
        .await;

    // The HTTP request should still be pending; no response yet.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Now device-a sends the real response and the HTTP request should
    // complete with it.
    device_a
        .send_file_response(FileResponse {
            request_id: "colliding-id".into(),
            result: Some(file_response::Result::Error(FileError {
                code: FileErrorCode::NotFound as i32,
                message: "from-correct-device".into(),
                path: String::new(),
            })),
        })
        .await;

    let http_response = http_handle.await.unwrap();
    assert_eq!(http_response.status().as_u16(), 200);
    let bytes = http_response.bytes().await.unwrap();
    let decoded = FileResponse::decode(bytes.as_ref()).unwrap();
    match decoded.result {
        Some(file_response::Result::Error(err)) => {
            assert_eq!(err.message, "from-correct-device");
        }
        other => panic!("expected error result, got {other:?}"),
    }

    server.shutdown().await;
}

#[tokio::test]
async fn file_operation_releases_slot_on_client_cancellation() {
    // I1 regression: when the HTTP client drops the connection mid-flight
    // (e.g. browser navigates away, network drop), Axum drops the handler
    // future without ever running the timeout/error/channel-close branches
    // that explicitly call `cancel()`. Without the RAII slot guard, the
    // entry stays in `PendingFileRequests` until the device responds — and
    // if the device never does, the slot is permanently leaked, eventually
    // exhausting the 1024-slot capacity (slow DoS).
    let server = spawn_test_server().await;
    let token = dashboard_token(&server).await;
    let mut device = server
        .attach_bootstrap_device("device-2", "bootstrap-test-token")
        .await;

    let url = format!("{}/api/devices/device-2/files", server.http_base_url());
    let body = encoded_read_text("/tmp/fake.txt", "req-cancel");
    let token_clone = token.clone();

    let http_handle = tokio::spawn(async move {
        reqwest::Client::new()
            .post(&url)
            .bearer_auth(&token_clone)
            .header("content-type", PROTOBUF_CONTENT_TYPE)
            .body(body)
            .send()
            .await
    });

    // Wait until the device sees the FileRequest — at that point the
    // server has already registered the slot.
    let received = device.recv_file_request().await;
    assert_eq!(received.request_id, "req-cancel");
    assert_eq!(
        server.state().pending_file_requests.in_flight(),
        1,
        "slot must be reserved while the request is in flight"
    );

    // Client closes the connection mid-flight. Aborting the JoinHandle
    // drops the reqwest future, which closes the TCP connection. Axum
    // then drops the handler future, which runs Drop on PendingSlotGuard.
    http_handle.abort();
    let _ = http_handle.await;

    // Give Axum a few ticks to notice the connection close and drop the
    // handler future.
    let mut waited = Duration::ZERO;
    let limit = Duration::from_secs(2);
    let step = Duration::from_millis(25);
    while server.state().pending_file_requests.in_flight() > 0 {
        if waited >= limit {
            panic!(
                "slot was not released after client cancellation; in_flight = {}",
                server.state().pending_file_requests.in_flight()
            );
        }
        tokio::time::sleep(step).await;
        waited += step;
    }
    assert_eq!(server.state().pending_file_requests.in_flight(), 0);

    server.shutdown().await;
}

// Timeout tests are intentionally NOT included here:
// `DEFAULT_REQUEST_TIMEOUT_SECS = 30` in http/files.rs is baked at compile
// time and the test support layer does not currently expose an override.
// Adding a production hook just for tests is worse than leaving this gap,
// so the timeout path is verified by inspection and by the unit-level tests
// in files.rs (`pending_file_requests_admission_control_accepts_after_cancel`
// and the cancel path covered by the happy-path integration test).
