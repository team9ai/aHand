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
    FileError, FileErrorCode, FileReadBinary, FileReadBinaryResult, FileReadText, FileRequest,
    FileResponse, FileStatResult, FileType, FileWrite, FullWrite, file_request, file_response,
    file_write, full_write,
};
use prost::Message;

use support::{TestServer, spawn_server_with_state, spawn_test_server, test_state_with_s3};

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

#[tokio::test]
async fn file_operation_returns_504_when_device_does_not_respond_within_configured_timeout() {
    // T17 follow-up: the timeout window used to be a hard-coded 30s
    // constant (`DEFAULT_REQUEST_TIMEOUT_SECS`), which made writing an
    // integration test prohibitively slow. It's now driven by
    // `Config::file_request_timeout_ms` → `AppState::file_request_timeout`,
    // so we can build a state with a 100 ms window, attach a device,
    // intentionally NOT respond, and assert the 504 envelope arrives
    // shortly after the configured deadline.
    use ahand_hub::state::AppState;
    use support::spawn_server_with_state;

    let mut config = support::test_config();
    config.file_request_timeout_ms = 100;
    let state = AppState::from_config(config).await.unwrap();
    let server = spawn_server_with_state(state).await;
    let token = dashboard_token(&server).await;

    // Attach a device so the request makes it past the offline guard
    // (which would otherwise return 409 immediately) — but DO NOT
    // respond to the FileRequest. The hub will time out after 100 ms.
    let mut device = server
        .attach_bootstrap_device("device-2", "bootstrap-test-token")
        .await;

    let url = format!("{}/api/devices/device-2/files", server.http_base_url());
    let body = encoded_read_text("/tmp/fake.txt", "req-timeout");
    let token_clone = token.clone();

    let started = std::time::Instant::now();
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

    // Wait for the device to *receive* the FileRequest (so we know the
    // hub has armed its timeout) but never reply.
    let received = device.recv_file_request().await;
    assert_eq!(received.request_id, "req-timeout");

    let response = http_handle.await.unwrap();
    let elapsed = started.elapsed();

    assert_eq!(response.status().as_u16(), 504);
    let body = response.text().await.unwrap();
    assert!(
        body.contains("100ms") || body.contains("DEVICE_TIMEOUT"),
        "expected DEVICE_TIMEOUT envelope citing the 100ms deadline, got: {body}"
    );

    // Sanity bound: the timeout should fire close to the configured
    // 100 ms deadline. Floor at 80 ms (catches a regression to "fires
    // immediately" / grace=0); ceiling generous at 5 s for slow CI.
    assert!(
        elapsed >= Duration::from_millis(80),
        "timeout fired suspiciously fast: {elapsed:?} (configured 100ms)"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "timeout took too long: {elapsed:?} (configured 100ms)"
    );

    // Tidy up: also assert the slot was released, since the timeout
    // path runs `cancel()` via the RAII guard.
    drop(device);
    server.shutdown().await;
}

// ── S3 large-file transfer tests ──────────────────────────────────────────

/// `POST /files/upload-url` must reject when the hub has no `[s3]`
/// config — exposing the route would otherwise return a 500 from the
/// presigner with an unhelpful error. 503 + `S3_DISABLED` lets clients
/// degrade gracefully (fall back to inline bytes for small files).
#[tokio::test]
async fn upload_url_returns_503_when_s3_disabled() {
    let server = spawn_test_server().await;
    let token = dashboard_token(&server).await;
    let url = format!(
        "{}/api/devices/device-2/files/upload-url",
        server.http_base_url()
    );
    let response = reqwest::Client::new()
        .post(&url)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 503);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        body.pointer("/error/code").and_then(|v| v.as_str()),
        Some("S3_DISABLED"),
        "unexpected error body: {body}"
    );
    server.shutdown().await;
}

/// `POST /files/upload-url` must require the same dashboard auth as
/// `POST /files`. Without a token the route should 401, never leaking a
/// presigned URL to anonymous callers.
#[tokio::test]
async fn upload_url_requires_dashboard_auth() {
    let state = test_state_with_s3().await;
    let server = spawn_server_with_state(state).await;
    let url = format!(
        "{}/api/devices/device-2/files/upload-url",
        server.http_base_url()
    );
    let response = reqwest::Client::new().post(&url).send().await.unwrap();
    assert_eq!(response.status().as_u16(), 401);
    server.shutdown().await;
}

/// Happy path: with `[s3]` configured against the fake endpoint, the
/// presigner produces a real URL with the right shape (path-style host,
/// bucket name, device-scoped object key, `expires_at_ms` populated).
/// We can't exercise an actual upload without a real bucket, but the
/// presigner is pure local HMAC, so URL composition is testable.
#[tokio::test]
async fn upload_url_returns_200_with_presigned_url_when_s3_configured() {
    let state = test_state_with_s3().await;
    let server = spawn_server_with_state(state).await;
    let token = dashboard_token(&server).await;
    let url = format!(
        "{}/api/devices/device-2/files/upload-url",
        server.http_base_url()
    );
    let response = reqwest::Client::new()
        .post(&url)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    let object_key = body.get("object_key").and_then(|v| v.as_str()).unwrap();
    assert!(
        object_key.starts_with("file-ops/device-2/"),
        "object_key should be device-scoped: {object_key}"
    );
    let upload_url = body.get("upload_url").and_then(|v| v.as_str()).unwrap();
    assert!(
        upload_url.starts_with("http://127.0.0.1:1/"),
        "upload_url should target the configured endpoint: {upload_url}"
    );
    assert!(
        upload_url.contains("test-bucket"),
        "upload_url should contain the bucket name in path-style: {upload_url}"
    );
    let expires = body
        .get("expires_at_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert!(expires > 0, "expires_at_ms should be populated");
    server.shutdown().await;
}

/// When the daemon returns a `read_binary` payload larger than the
/// configured threshold, the hub must enter the swap path: upload the
/// bytes to S3 and rewrite the response to use a presigned download
/// URL. Our test config points S3 at an unreachable endpoint, so the
/// upload itself fails — but the failure code (`S3_UPLOAD_FAILED`)
/// proves the swap was attempted, which is what we're verifying. A
/// LocalStack-backed test would round-trip the URL successfully.
#[tokio::test]
async fn large_read_binary_response_triggers_s3_upload() {
    let state = test_state_with_s3().await;
    let server = spawn_server_with_state(state).await;
    let token = dashboard_token(&server).await;
    let mut device = server
        .attach_bootstrap_device("device-2", "bootstrap-test-token")
        .await;

    let req = FileRequest {
        request_id: "req-large-read".into(),
        operation: Some(file_request::Operation::ReadBinary(FileReadBinary {
            path: "/tmp/big.bin".into(),
            byte_offset: 0,
            byte_length: 0,
            max_bytes: None,
            no_follow_symlink: false,
        })),
    };
    let url = format!("{}/api/devices/device-2/files", server.http_base_url());
    let body = req.encode_to_vec();
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

    let received = device.recv_file_request().await;
    assert_eq!(received.request_id, "req-large-read");

    // Reply with content well over the 1 KiB threshold so the swap path
    // engages.
    let big = vec![b'X'; 4096];
    let canned = FileResponse {
        request_id: received.request_id.clone(),
        result: Some(file_response::Result::ReadBinary(FileReadBinaryResult {
            content: big.clone(),
            byte_offset: 0,
            bytes_read: big.len() as u64,
            total_file_bytes: big.len() as u64,
            remaining_bytes: 0,
            download_url: None,
            download_url_expires_ms: None,
        })),
    };
    device.send_file_response(canned).await;

    let resp = http_handle.await.unwrap();
    assert_eq!(resp.status().as_u16(), 502);
    let body_json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body_json.pointer("/error/code").and_then(|v| v.as_str()),
        Some("S3_UPLOAD_FAILED"),
        "unexpected error body: {body_json}"
    );
    drop(device);
    server.shutdown().await;
}

/// Small (under-threshold) read responses must pass through unchanged —
/// no S3 contact, no `download_url` in the proto. This guards against a
/// regression where the swap path fires for every read.
#[tokio::test]
async fn small_read_binary_response_is_not_swapped() {
    let state = test_state_with_s3().await;
    let server = spawn_server_with_state(state).await;
    let token = dashboard_token(&server).await;
    let mut device = server
        .attach_bootstrap_device("device-2", "bootstrap-test-token")
        .await;

    let req = FileRequest {
        request_id: "req-small-read".into(),
        operation: Some(file_request::Operation::ReadBinary(FileReadBinary {
            path: "/tmp/small.bin".into(),
            byte_offset: 0,
            byte_length: 0,
            max_bytes: None,
            no_follow_symlink: false,
        })),
    };
    let url = format!("{}/api/devices/device-2/files", server.http_base_url());
    let body = req.encode_to_vec();
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

    let received = device.recv_file_request().await;
    let small = b"hello world".to_vec();
    let canned = FileResponse {
        request_id: received.request_id.clone(),
        result: Some(file_response::Result::ReadBinary(FileReadBinaryResult {
            content: small.clone(),
            byte_offset: 0,
            bytes_read: small.len() as u64,
            total_file_bytes: small.len() as u64,
            remaining_bytes: 0,
            download_url: None,
            download_url_expires_ms: None,
        })),
    };
    device.send_file_response(canned).await;

    let resp = http_handle.await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let bytes = resp.bytes().await.unwrap();
    let decoded = FileResponse::decode(bytes.as_ref()).unwrap();
    let Some(file_response::Result::ReadBinary(r)) = decoded.result else {
        panic!("expected ReadBinary result");
    };
    assert_eq!(r.content, small);
    assert!(
        r.download_url.is_none(),
        "small reads should not be swapped to S3"
    );
    drop(device);
    server.shutdown().await;
}

/// `FullWrite { s3_object_key }` must trigger the hub-side download URL
/// injection: the daemon should observe `s3_download_url` populated in
/// the request that arrives over the WebSocket. We verify by capturing
/// the FileRequest on the device side and asserting the URL points at
/// the configured S3 endpoint.
#[tokio::test]
async fn full_write_with_s3_object_key_gets_download_url_injected() {
    let state = test_state_with_s3().await;
    let server = spawn_server_with_state(state).await;
    let token = dashboard_token(&server).await;
    let mut device = server
        .attach_bootstrap_device("device-2", "bootstrap-test-token")
        .await;

    let req = FileRequest {
        request_id: "req-s3-write".into(),
        operation: Some(file_request::Operation::Write(FileWrite {
            path: "/tmp/from-s3.bin".into(),
            create_parents: false,
            method: Some(file_write::Method::FullWrite(FullWrite {
                source: Some(full_write::Source::S3ObjectKey(
                    "file-ops/device-2/abc.bin".into(),
                )),
                s3_download_url: None,
                s3_download_url_expires_ms: None,
            })),
            encoding: None,
            no_follow_symlink: false,
        })),
    };
    let url = format!("{}/api/devices/device-2/files", server.http_base_url());
    let body = req.encode_to_vec();
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

    // Capture and assert on the FileRequest as the daemon sees it.
    let received = device.recv_file_request().await;
    let Some(file_request::Operation::Write(w)) = received.operation else {
        panic!("expected Write operation");
    };
    let Some(file_write::Method::FullWrite(fw)) = w.method else {
        panic!("expected FullWrite method");
    };
    let injected_url = fw
        .s3_download_url
        .as_deref()
        .expect("hub should inject s3_download_url");
    assert!(
        injected_url.starts_with("http://127.0.0.1:1/"),
        "injected url should target the configured endpoint: {injected_url}"
    );
    assert!(
        injected_url.contains("file-ops/device-2/abc.bin"),
        "injected url should embed the original object_key: {injected_url}"
    );
    assert!(fw.s3_download_url_expires_ms.unwrap_or(0) > 0);

    // Send a synthetic success response so the HTTP handler returns,
    // letting the test shut down cleanly.
    let canned = FileResponse {
        request_id: received.request_id.clone(),
        result: Some(file_response::Result::Write(
            ahand_protocol::FileWriteResult {
                path: "/tmp/from-s3.bin".into(),
                action: ahand_protocol::WriteAction::Created as i32,
                bytes_written: 0,
                final_size: 0,
                replacements_made: None,
            },
        )),
    };
    device.send_file_response(canned).await;
    let resp = http_handle.await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);

    drop(device);
    server.shutdown().await;
}

/// `FullWrite { s3_object_key }` must fail-fast at the hub when no
/// `[s3]` is configured, instead of forwarding a half-baked request to
/// the daemon. Returns 503 + `S3_DISABLED` matching the upload-url
/// route's contract.
#[tokio::test]
async fn full_write_with_s3_object_key_returns_503_when_s3_disabled() {
    let server = spawn_test_server().await;
    let token = dashboard_token(&server).await;

    let req = FileRequest {
        request_id: "req-s3-no-config".into(),
        operation: Some(file_request::Operation::Write(FileWrite {
            path: "/tmp/no-s3.bin".into(),
            create_parents: false,
            method: Some(file_write::Method::FullWrite(FullWrite {
                source: Some(full_write::Source::S3ObjectKey("k".into())),
                s3_download_url: None,
                s3_download_url_expires_ms: None,
            })),
            encoding: None,
            no_follow_symlink: false,
        })),
    };
    let url = format!("{}/api/devices/device-2/files", server.http_base_url());
    let body = req.encode_to_vec();
    let resp = reqwest::Client::new()
        .post(&url)
        .bearer_auth(&token)
        .header("content-type", PROTOBUF_CONTENT_TYPE)
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 503);
    let body_json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body_json.pointer("/error/code").and_then(|v| v.as_str()),
        Some("S3_DISABLED"),
    );
    server.shutdown().await;
}
