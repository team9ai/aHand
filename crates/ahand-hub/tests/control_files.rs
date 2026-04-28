//! Integration tests for `POST /api/control/files`.
//!
//! Mirrors `tests/browser_api.rs::control_browser_*` — spin up a hub,
//! pre-register a device with `external_user_id`, attach a fake daemon
//! over WS, mint a control-plane JWT, then drive the HTTP endpoint
//! and have the daemon answer the `FileRequest` envelope. The
//! daemon-side file_manager is tested exhaustively in the ahandd
//! crate's file_ops integration tests; here we only cover the hub's
//! control-plane surface (auth, ownership, JSON ⇄ proto conversion,
//! error mapping).

mod support;

use std::time::Duration;

use ahand_hub_core::traits::DeviceAdminStore;
use ahand_protocol::{
    FileError, FileErrorCode, FileResponse, FileStatResult, FileType, file_request, file_response,
};
use ed25519_dalek::SigningKey;
use futures_util::SinkExt;
use prost::Message;
use support::{
    TestServer, read_hello_accepted, read_hello_challenge, signed_hello, spawn_server_with_state,
};
use tokio_tungstenite::tungstenite::Message as WsMessage;

const JWT_SECRET: &str = "service-test-secret";

/// Pre-register `device_id` as owned by `external_user_id`, then attach
/// a live WS daemon. File operations don't require a specific
/// capability advertisement — the dashboard /api/devices/{id}/files
/// flow doesn't gate on capabilities either, so we use the standard
/// `signed_hello` (exec-only).
async fn attach_owned_device(
    server: &TestServer,
    device_id: &str,
    external_user_id: &str,
) -> support::TestDevice {
    let verifying = SigningKey::from_bytes(&[7u8; 32])
        .verifying_key()
        .to_bytes();
    server
        .state()
        .devices
        .pre_register(device_id, &verifying, external_user_id)
        .await
        .unwrap();
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
    // Brief grace so the hub finishes registering the WS connection +
    // marking the device online before the HTTP request races in.
    tokio::time::sleep(Duration::from_millis(50)).await;
    support::test_device_from_socket(device_id, socket)
}

fn mint_cp_jwt(external_user_id: &str) -> String {
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

fn mint_cp_jwt_with_scope(external_user_id: &str, scope: &str) -> String {
    use ahand_hub_core::auth::mint_control_plane_jwt;
    let (token, _) = mint_control_plane_jwt(
        JWT_SECRET.as_bytes(),
        external_user_id,
        scope,
        None,
        Duration::from_secs(60),
    )
    .unwrap();
    token
}

#[tokio::test]
async fn control_files_401_without_auth_header() {
    let server = spawn_server_with_state(support::test_state().await).await;

    let response = reqwest::Client::new()
        .post(format!("{}/api/control/files", server.http_base_url()))
        .json(&serde_json::json!({
            "device_id": "anything",
            "operation": "stat",
            "params": { "path": "/tmp/x" },
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    server.shutdown().await;
}

#[tokio::test]
async fn control_files_409_when_device_offline() {
    // Pre-register the device so ownership matches, but DON'T attach a
    // WS daemon — file_service::execute will hit DeviceOffline on send.
    // Mirrors the dashboard contract where DEVICE_OFFLINE is 409.
    let server = spawn_server_with_state(support::test_state().await).await;
    let verifying = SigningKey::from_bytes(&[7u8; 32])
        .verifying_key()
        .to_bytes();
    server
        .state()
        .devices
        .pre_register("cf-offline", &verifying, "user-off")
        .await
        .unwrap();
    let token = mint_cp_jwt("user-off");

    let response = reqwest::Client::new()
        .post(format!("{}/api/control/files", server.http_base_url()))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "device_id": "cf-offline",
            "operation": "stat",
            "params": { "path": "/tmp/x" },
            "timeout_ms": 5_000,
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "DEVICE_OFFLINE");
    server.shutdown().await;
}

#[tokio::test]
async fn control_files_stat_happy_path_round_trips() {
    let server = spawn_server_with_state(support::test_state().await).await;
    let mut device = attach_owned_device(&server, "cf-stat", "user-stat").await;
    let token = mint_cp_jwt("user-stat");

    let api_task = {
        let base_url = server.http_base_url().to_string();
        let token = token.clone();
        tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("{base_url}/api/control/files"))
                .bearer_auth(&token)
                .json(&serde_json::json!({
                    "device_id": "cf-stat",
                    "operation": "stat",
                    "params": { "path": "/tmp/fake.txt" },
                    "timeout_ms": 10_000,
                    "correlation_id": "c-1",
                }))
                .send()
                .await
                .unwrap()
        })
    };

    let req = device.recv_file_request().await;
    // Verify the proto envelope carries the right oneof — Stat with
    // the path the SDK forwarded.
    match req.operation {
        Some(file_request::Operation::Stat(s)) => {
            assert_eq!(s.path, "/tmp/fake.txt");
            assert!(!s.no_follow_symlink);
        }
        other => panic!("expected Stat operation, got {other:?}"),
    }

    device
        .send_file_response(FileResponse {
            request_id: req.request_id.clone(),
            result: Some(file_response::Result::Stat(FileStatResult {
                path: "/tmp/fake.txt".into(),
                file_type: FileType::File as i32,
                size: 1234,
                modified_ms: 1_700_000_000_000,
                created_ms: 1_700_000_000_000,
                accessed_ms: 1_700_000_000_000,
                unix_permission: None,
                windows_acl: None,
                symlink_target: None,
            })),
        })
        .await;

    let response = api_task.await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["success"], true);
    assert_eq!(body["operation"], "stat");
    assert_eq!(body["result"]["path"], "/tmp/fake.txt");
    assert_eq!(body["result"]["file_type"], "file");
    assert_eq!(body["result"]["size"], 1234);
    assert_eq!(body["request_id"], req.request_id);
    assert!(body["duration_ms"].as_u64().is_some());
    // No `error` field on success.
    assert!(body.as_object().unwrap().get("error").is_none());

    drop(device);
    server.shutdown().await;
}

#[tokio::test]
async fn control_files_policy_denied_returns_success_false_with_error_code() {
    // Daemon-side policy refusal: success: false, error.code = "policy_denied",
    // HTTP 200. This mirrors the dashboard contract — daemon-level errors
    // are surfaced inside the response envelope, hub-level errors are HTTP
    // codes. The SDK branches on `success` + `error.code` for these.
    let server = spawn_server_with_state(support::test_state().await).await;
    let mut device = attach_owned_device(&server, "cf-policy", "user-policy").await;
    let token = mint_cp_jwt("user-policy");

    let api_task = {
        let base_url = server.http_base_url().to_string();
        let token = token.clone();
        tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("{base_url}/api/control/files"))
                .bearer_auth(&token)
                .json(&serde_json::json!({
                    "device_id": "cf-policy",
                    "operation": "delete",
                    "params": {
                        "path": "/etc/shadow",
                        "recursive": false,
                        "mode": "permanent",
                    },
                    "timeout_ms": 5_000,
                }))
                .send()
                .await
                .unwrap()
        })
    };

    let req = device.recv_file_request().await;
    device
        .send_file_response(FileResponse {
            request_id: req.request_id.clone(),
            result: Some(file_response::Result::Error(FileError {
                code: FileErrorCode::PolicyDenied as i32,
                message: "policy refused: protected path".into(),
                path: "/etc/shadow".into(),
            })),
        })
        .await;

    let response = api_task.await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["success"], false);
    assert_eq!(body["operation"], "delete");
    assert_eq!(body["error"]["code"], "policy_denied");
    assert_eq!(body["error"]["path"], "/etc/shadow");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("policy refused")
    );
    // No `result` field on error.
    assert!(body.as_object().unwrap().get("result").is_none());

    drop(device);
    server.shutdown().await;
}

// ── extra coverage: scope, ownership, validation, timeout, write/list ──

#[tokio::test]
async fn control_files_403_when_scope_not_jobs_execute() {
    let server = spawn_server_with_state(support::test_state().await).await;
    let _device = attach_owned_device(&server, "cf-scope", "user-scope").await;
    // A token minted with `jobs:read` (or any non-execute scope) must
    // be rejected at the handler scope guard, before any DB work.
    let token = mint_cp_jwt_with_scope("user-scope", "jobs:read");

    let response = reqwest::Client::new()
        .post(format!("{}/api/control/files", server.http_base_url()))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "device_id": "cf-scope",
            "operation": "stat",
            "params": { "path": "/tmp/x" },
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "FORBIDDEN");

    drop(_device);
    server.shutdown().await;
}

#[tokio::test]
async fn control_files_403_when_user_does_not_own_device() {
    let server = spawn_server_with_state(support::test_state().await).await;
    let _device = attach_owned_device(&server, "cf-owner", "user-A").await;
    let token = mint_cp_jwt("user-B");

    let response = reqwest::Client::new()
        .post(format!("{}/api/control/files", server.http_base_url()))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "device_id": "cf-owner",
            "operation": "stat",
            "params": { "path": "/tmp/x" },
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "NOT_DEVICE_OWNER");

    drop(_device);
    server.shutdown().await;
}

#[tokio::test]
async fn control_files_404_when_device_unknown() {
    let server = spawn_server_with_state(support::test_state().await).await;
    let token = mint_cp_jwt("user-x");

    let response = reqwest::Client::new()
        .post(format!("{}/api/control/files", server.http_base_url()))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "device_id": "ghost",
            "operation": "stat",
            "params": { "path": "/tmp/x" },
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "DEVICE_NOT_FOUND");
    server.shutdown().await;
}

#[tokio::test]
async fn control_files_400_when_unknown_operation() {
    // Catch a typo / unsupported op early — the SDK should fail loud
    // on this rather than letting it reach the daemon.
    let server = spawn_server_with_state(support::test_state().await).await;
    let _device = attach_owned_device(&server, "cf-unknown-op", "user-uo").await;
    let token = mint_cp_jwt("user-uo");

    let response = reqwest::Client::new()
        .post(format!("{}/api/control/files", server.http_base_url()))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "device_id": "cf-unknown-op",
            "operation": "nope_not_a_real_op",
            "params": {},
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "UNKNOWN_OPERATION");

    drop(_device);
    server.shutdown().await;
}

#[tokio::test]
async fn control_files_400_when_params_missing_required_field() {
    let server = spawn_server_with_state(support::test_state().await).await;
    let _device = attach_owned_device(&server, "cf-bad-params", "user-bp").await;
    let token = mint_cp_jwt("user-bp");

    let response = reqwest::Client::new()
        .post(format!("{}/api/control/files", server.http_base_url()))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "device_id": "cf-bad-params",
            "operation": "stat",
            // `path` is required for stat.
            "params": { "no_follow_symlink": true },
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "INVALID_PARAMS");

    drop(_device);
    server.shutdown().await;
}

#[tokio::test]
async fn control_files_504_on_timeout() {
    let server = spawn_server_with_state(support::test_state().await).await;
    let _device = attach_owned_device(&server, "cf-timeout", "user-to").await;
    let token = mint_cp_jwt("user-to");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let response = client
        .post(format!("{}/api/control/files", server.http_base_url()))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "device_id": "cf-timeout",
            "operation": "stat",
            "params": { "path": "/tmp/x" },
            // Hub-side timeout fires; daemon never replies.
            "timeout_ms": 1_000,
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::GATEWAY_TIMEOUT);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "DEVICE_TIMEOUT");

    drop(_device);
    server.shutdown().await;
}

#[tokio::test]
async fn control_files_write_full_write_round_trips_text_content() {
    // FullWrite content path: `params.full_write.content` is a UTF-8
    // string the hub turns into bytes in the proto envelope. This is
    // the most common write shape an SDK consumer will use.
    let server = spawn_server_with_state(support::test_state().await).await;
    let mut device = attach_owned_device(&server, "cf-write", "user-w").await;
    let token = mint_cp_jwt("user-w");

    let api_task = {
        let base_url = server.http_base_url().to_string();
        let token = token.clone();
        tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("{base_url}/api/control/files"))
                .bearer_auth(&token)
                .json(&serde_json::json!({
                    "device_id": "cf-write",
                    "operation": "write",
                    "params": {
                        "path": "/tmp/out.txt",
                        "create_parents": true,
                        "full_write": { "content": "hello world" }
                    },
                    "timeout_ms": 5_000,
                }))
                .send()
                .await
                .unwrap()
        })
    };

    let req = device.recv_file_request().await;
    match req.operation {
        Some(file_request::Operation::Write(w)) => {
            assert_eq!(w.path, "/tmp/out.txt");
            assert!(w.create_parents);
            match w.method {
                Some(ahand_protocol::file_write::Method::FullWrite(fw)) => match fw.source {
                    Some(ahand_protocol::full_write::Source::Content(bytes)) => {
                        assert_eq!(bytes, b"hello world");
                    }
                    other => panic!("expected Content source, got {other:?}"),
                },
                other => panic!("expected FullWrite method, got {other:?}"),
            }
        }
        other => panic!("expected Write operation, got {other:?}"),
    }

    device
        .send_file_response(FileResponse {
            request_id: req.request_id.clone(),
            result: Some(file_response::Result::Write(
                ahand_protocol::FileWriteResult {
                    path: "/tmp/out.txt".into(),
                    action: ahand_protocol::WriteAction::Created as i32,
                    bytes_written: 11,
                    final_size: 11,
                    replacements_made: None,
                },
            )),
        })
        .await;

    let response = api_task.await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["success"], true);
    assert_eq!(body["result"]["action"], "created");
    assert_eq!(body["result"]["bytes_written"], 11);

    drop(device);
    server.shutdown().await;
}

#[tokio::test]
async fn control_files_list_returns_entries_array() {
    // Round-trip a directory listing: List proto → JSON entries[]. This
    // exercises the multi-entry serializer path that other ops don't.
    let server = spawn_server_with_state(support::test_state().await).await;
    let mut device = attach_owned_device(&server, "cf-list", "user-l").await;
    let token = mint_cp_jwt("user-l");

    let api_task = {
        let base_url = server.http_base_url().to_string();
        let token = token.clone();
        tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("{base_url}/api/control/files"))
                .bearer_auth(&token)
                .json(&serde_json::json!({
                    "device_id": "cf-list",
                    "operation": "list",
                    "params": { "path": "/tmp", "include_hidden": true },
                    "timeout_ms": 5_000,
                }))
                .send()
                .await
                .unwrap()
        })
    };

    let req = device.recv_file_request().await;
    let entries = vec![
        ahand_protocol::FileEntry {
            name: "a.txt".into(),
            file_type: FileType::File as i32,
            size: 1,
            modified_ms: 0,
            symlink_target: None,
        },
        ahand_protocol::FileEntry {
            name: "subdir".into(),
            file_type: FileType::Directory as i32,
            size: 0,
            modified_ms: 0,
            symlink_target: None,
        },
    ];
    device
        .send_file_response(FileResponse {
            request_id: req.request_id.clone(),
            result: Some(file_response::Result::List(
                ahand_protocol::FileListResult {
                    entries,
                    total_count: 2,
                    has_more: false,
                },
            )),
        })
        .await;

    let response = api_task.await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["success"], true);
    let arr = body["result"]["entries"].as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["name"], "a.txt");
    assert_eq!(arr[0]["file_type"], "file");
    assert_eq!(arr[1]["name"], "subdir");
    assert_eq!(arr[1]["file_type"], "directory");
    assert_eq!(body["result"]["total_count"], 2);
    assert_eq!(body["result"]["has_more"], false);

    drop(device);
    server.shutdown().await;
}

#[tokio::test]
async fn control_files_read_text_round_trips_lines_and_metadata() {
    // ReadText is the most field-heavy proto on the response side —
    // exercise enough of it to lock the JSON shape.
    let server = spawn_server_with_state(support::test_state().await).await;
    let mut device = attach_owned_device(&server, "cf-readtext", "user-rt").await;
    let token = mint_cp_jwt("user-rt");

    let api_task = {
        let base_url = server.http_base_url().to_string();
        let token = token.clone();
        tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("{base_url}/api/control/files"))
                .bearer_auth(&token)
                .json(&serde_json::json!({
                    "device_id": "cf-readtext",
                    "operation": "read_text",
                    "params": {
                        "path": "/tmp/r.txt",
                        "start": { "start_line": 1 },
                        "max_lines": 100,
                        "line_numbers": true,
                    },
                    "timeout_ms": 5_000,
                }))
                .send()
                .await
                .unwrap()
        })
    };

    let req = device.recv_file_request().await;
    match req.operation {
        Some(file_request::Operation::ReadText(rt)) => {
            assert_eq!(rt.path, "/tmp/r.txt");
            assert!(rt.line_numbers);
            assert_eq!(
                rt.start,
                Some(ahand_protocol::file_read_text::Start::StartLine(1))
            );
        }
        other => panic!("expected ReadText, got {other:?}"),
    }

    device
        .send_file_response(FileResponse {
            request_id: req.request_id.clone(),
            result: Some(file_response::Result::ReadText(
                ahand_protocol::FileReadTextResult {
                    lines: vec![ahand_protocol::TextLine {
                        content: "first line".into(),
                        line_number: 1,
                        truncated: false,
                        remaining_bytes: 0,
                    }],
                    stop_reason: ahand_protocol::StopReason::FileEnd as i32,
                    start_pos: None,
                    end_pos: None,
                    remaining_bytes: 0,
                    total_file_bytes: 10,
                    total_lines: 1,
                    detected_encoding: "utf-8".into(),
                },
            )),
        })
        .await;

    let response = api_task.await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["success"], true);
    assert_eq!(body["result"]["stop_reason"], "file_end");
    assert_eq!(body["result"]["detected_encoding"], "utf-8");
    let lines = body["result"]["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["content"], "first line");
    assert_eq!(lines[0]["line_number"], 1);

    drop(device);
    server.shutdown().await;
}
