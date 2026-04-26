mod support;

use std::time::Duration;

use ahand_hub_core::traits::DeviceAdminStore;
use ahand_protocol::BrowserResponse;
use ed25519_dalek::SigningKey;
use futures_util::SinkExt;
use prost::Message;
use support::{
    TestServer, read_hello_accepted, read_hello_challenge, signed_hello_with_browser,
    spawn_server_with_state,
};
use tokio_tungstenite::tungstenite::Message as WsMessage;

const JWT_SECRET: &str = "service-test-secret";

/// Pre-register `device_id` as owned by `external_user_id`, then attach
/// a live WS daemon advertising the `browser` capability. The returned
/// `TestDevice` owns the WS socket; drop it when the test is done.
async fn attach_owned_browser_device(
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
    // After pre-register the row exists with external_user_id set; the
    // hello path will upsert the row with the (`exec`,`browser`)
    // capability set without clobbering the external_user_id.
    let (mut socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();
    let challenge = read_hello_challenge(&mut socket).await;
    let hello = signed_hello_with_browser(device_id, &challenge.nonce);
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

/// Mint a control-plane JWT directly via the hub's auth helper.
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

async fn login_token(server: &support::TestServer) -> String {
    let body = server
        .post_json(
            "/api/auth/login",
            "",
            serde_json::json!({ "password": "shared-secret" }),
        )
        .await;
    body["token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn browser_command_roundtrip() {
    let state = support::test_state_with_browser_device().await;
    let server = spawn_server_with_state(state).await;
    let token = login_token(&server).await;

    let mut device = server.attach_browser_device("device-1").await;

    let api_task = {
        let base_url = server.http_base_url().to_string();
        let token = token.clone();
        tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("{base_url}/api/browser"))
                .bearer_auth(&token)
                .json(&serde_json::json!({
                    "device_id": "device-1",
                    "session_id": "sess-1",
                    "action": "snapshot",
                    "params": { "selector": "#main" },
                    "timeout_ms": 10_000
                }))
                .send()
                .await
                .unwrap()
        })
    };

    let browser_req = device.recv_browser_request().await;
    assert_eq!(browser_req.session_id, "sess-1");
    assert_eq!(browser_req.action, "snapshot");

    device
        .send_browser_response(BrowserResponse {
            request_id: browser_req.request_id.clone(),
            session_id: "sess-1".into(),
            success: true,
            result_json: r#"{"title":"Test Page"}"#.into(),
            error: String::new(),
            binary_data: Vec::new(),
            binary_mime: String::new(),
        })
        .await;

    let response = api_task.await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["success"], true);
    assert_eq!(body["data"]["title"], "Test Page");
    assert!(body.get("binary_data").is_none() || body["binary_data"].is_null());
    assert!(body.get("error").is_none() || body["error"].is_null());
}

#[tokio::test]
async fn browser_command_with_binary_data() {
    let state = support::test_state_with_browser_device().await;
    let server = spawn_server_with_state(state).await;
    let token = login_token(&server).await;

    let mut device = server.attach_browser_device("device-1").await;

    let fake_png: Vec<u8> = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0xDE, 0xAD];

    let api_task = {
        let base_url = server.http_base_url().to_string();
        let token = token.clone();
        tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("{base_url}/api/browser"))
                .bearer_auth(&token)
                .json(&serde_json::json!({
                    "device_id": "device-1",
                    "session_id": "sess-2",
                    "action": "screenshot",
                    "timeout_ms": 10_000
                }))
                .send()
                .await
                .unwrap()
        })
    };

    let browser_req = device.recv_browser_request().await;
    assert_eq!(browser_req.action, "screenshot");

    let binary_payload = fake_png.clone();
    device
        .send_browser_response(BrowserResponse {
            request_id: browser_req.request_id.clone(),
            session_id: "sess-2".into(),
            success: true,
            result_json: String::new(),
            error: String::new(),
            binary_data: binary_payload,
            binary_mime: "image/png".into(),
        })
        .await;

    let response = api_task.await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["success"], true);
    assert_eq!(body["binary_mime"], "image/png");

    let encoded = body["binary_data"].as_str().unwrap();
    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    assert_eq!(decoded, fake_png);
}

#[tokio::test]
async fn browser_command_offline_device_returns_404() {
    let state = support::test_state_with_browser_device().await;
    // Device is seeded but NOT attached (offline).
    let server = spawn_server_with_state(state).await;
    let token = login_token(&server).await;

    let response = server
        .post(
            "/api/browser",
            &token,
            serde_json::json!({
                "device_id": "device-1",
                "session_id": "sess-3",
                "action": "snapshot",
                "timeout_ms": 5_000
            }),
        )
        .await;

    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "DEVICE_OFFLINE");
}

#[tokio::test]
async fn browser_command_no_capability_returns_400() {
    // test_state() seeds device-1 with only "exec" capability — no "browser".
    let state = support::test_state().await;
    let server = spawn_server_with_state(state).await;
    let token = login_token(&server).await;

    // Attach the device so it's online (uses signed_hello with exec-only caps).
    let _device = server.attach_test_device("device-1").await;

    let response = server
        .post(
            "/api/browser",
            &token,
            serde_json::json!({
                "device_id": "device-1",
                "session_id": "sess-4",
                "action": "snapshot",
                "timeout_ms": 5_000
            }),
        )
        .await;

    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "NO_BROWSER_CAPABILITY");
}

#[tokio::test]
async fn browser_command_timeout_returns_504() {
    let state = support::test_state_with_browser_device().await;
    let server = spawn_server_with_state(state).await;
    let token = login_token(&server).await;

    // Attach but never respond to the browser request.
    let _device = server.attach_browser_device("device-1").await;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();

    let response = client
        .post(format!("{}/api/browser", server.http_base_url()))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "device_id": "device-1",
            "session_id": "sess-5",
            "action": "snapshot",
            "timeout_ms": 1_000
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::GATEWAY_TIMEOUT);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "TIMEOUT");
}

#[tokio::test]
async fn browser_command_unauthenticated_returns_401() {
    let state = support::test_state_with_browser_device().await;
    let server = spawn_server_with_state(state).await;

    // POST without any auth token.
    let response = reqwest::Client::new()
        .post(format!("{}/api/browser", server.http_base_url()))
        .json(&serde_json::json!({
            "device_id": "device-1",
            "session_id": "sess-6",
            "action": "snapshot",
            "timeout_ms": 5_000
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
}

// ──────────────────────────────────────────────────────────────────────
// `/api/control/browser` — Task 9 worker-side endpoint.
//
// Each test follows the pattern from `tests/control_plane.rs`: spin up
// a hub, pre-register a device with `external_user_id`, attach a fake
// daemon over WS that advertises the `browser` capability, mint a
// control-plane JWT, then drive the HTTP endpoint.
// ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn control_browser_200_with_valid_jwt_and_owner_match() {
    let server = spawn_server_with_state(support::test_state().await).await;
    let mut device = attach_owned_browser_device(&server, "cb-200", "user-cp-200").await;
    let token = mint_cp_jwt("user-cp-200");

    let api_task = {
        let base_url = server.http_base_url().to_string();
        let token = token.clone();
        tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("{base_url}/api/control/browser"))
                .bearer_auth(&token)
                .json(&serde_json::json!({
                    "device_id": "cb-200",
                    "session_id": "sess-cp-1",
                    "action": "snapshot",
                    "params": { "selector": "body" },
                    "timeout_ms": 10_000,
                    "correlation_id": "c-1",
                }))
                .send()
                .await
                .unwrap()
        })
    };

    // Receive the BrowserRequest the hub forwarded over WS, then reply.
    let req = device.recv_browser_request().await;
    assert_eq!(req.session_id, "sess-cp-1");
    assert_eq!(req.action, "snapshot");
    device
        .send_browser_response(BrowserResponse {
            request_id: req.request_id.clone(),
            session_id: "sess-cp-1".into(),
            success: true,
            result_json: r#"{"title":"Hello"}"#.into(),
            error: String::new(),
            binary_data: Vec::new(),
            binary_mime: String::new(),
        })
        .await;

    let response = api_task.await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["success"], true);
    assert_eq!(body["data"]["title"], "Hello");
    // duration_ms must be present and a non-negative integer (could be
    // 0 on extremely fast in-process round trips, but the field must
    // exist with the right type).
    assert!(
        body.get("duration_ms").and_then(|v| v.as_u64()).is_some(),
        "duration_ms missing or wrong type: {body}"
    );

    drop(device);
    server.shutdown().await;
}

#[tokio::test]
async fn control_browser_401_without_auth_header() {
    let server = spawn_server_with_state(support::test_state().await).await;

    let response = reqwest::Client::new()
        .post(format!("{}/api/control/browser", server.http_base_url()))
        .json(&serde_json::json!({
            "device_id": "anything",
            "session_id": "x",
            "action": "snapshot",
            "timeout_ms": 5_000,
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    server.shutdown().await;
}

#[tokio::test]
async fn control_browser_403_when_user_does_not_own_device() {
    let server = spawn_server_with_state(support::test_state().await).await;
    // Device is owned by user-A, but we'll mint a JWT for user-B.
    let _device = attach_owned_browser_device(&server, "cb-403", "user-A").await;
    let token = mint_cp_jwt("user-B");

    let response = reqwest::Client::new()
        .post(format!("{}/api/control/browser", server.http_base_url()))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "device_id": "cb-403",
            "session_id": "x",
            "action": "snapshot",
            "timeout_ms": 5_000,
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
async fn control_browser_403_when_device_has_no_external_user_id() {
    // The seeded `device-1` row in `test_state_with_browser_device()`
    // has external_user_id = None. Any control-plane JWT must be
    // refused — owners are matched explicitly, an unowned device is
    // never anyone's.
    let server =
        spawn_server_with_state(support::test_state_with_browser_device().await).await;
    let token = mint_cp_jwt("user-anyone");

    let response = reqwest::Client::new()
        .post(format!("{}/api/control/browser", server.http_base_url()))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "device_id": "device-1",
            "session_id": "x",
            "action": "snapshot",
            "timeout_ms": 5_000,
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "NOT_DEVICE_OWNER");
    server.shutdown().await;
}

#[tokio::test]
async fn control_browser_404_when_device_offline() {
    let server = spawn_server_with_state(support::test_state().await).await;
    // Pre-register the device so ownership matches, but DON'T attach a
    // WS daemon — it will be marked offline in the device store.
    let verifying = SigningKey::from_bytes(&[7u8; 32])
        .verifying_key()
        .to_bytes();
    server
        .state()
        .devices
        .pre_register("cb-offline", &verifying, "user-off")
        .await
        .unwrap();
    let token = mint_cp_jwt("user-off");

    let response = reqwest::Client::new()
        .post(format!("{}/api/control/browser", server.http_base_url()))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "device_id": "cb-offline",
            "session_id": "x",
            "action": "snapshot",
            "timeout_ms": 5_000,
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
    let body: serde_json::Value = response.json().await.unwrap();
    // Mirrors the dashboard contract: an offline-but-known device
    // surfaces as DEVICE_OFFLINE (not DEVICE_NOT_FOUND).
    assert_eq!(body["error"]["code"], "DEVICE_OFFLINE");
    server.shutdown().await;
}

#[tokio::test]
async fn control_browser_404_when_device_unknown() {
    // No device row at all → DEVICE_NOT_FOUND.
    let server = spawn_server_with_state(support::test_state().await).await;
    let token = mint_cp_jwt("user-x");

    let response = reqwest::Client::new()
        .post(format!("{}/api/control/browser", server.http_base_url()))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "device_id": "ghost",
            "session_id": "x",
            "action": "snapshot",
            "timeout_ms": 5_000,
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
async fn control_browser_400_when_device_lacks_browser_capability() {
    // Pre-register an owned device, then attach with `exec`-only caps
    // (no `browser`). `attach_test_device` uses `signed_hello` which
    // advertises only `exec`.
    let server = spawn_server_with_state(support::test_state().await).await;
    let verifying = SigningKey::from_bytes(&[7u8; 32])
        .verifying_key()
        .to_bytes();
    server
        .state()
        .devices
        .pre_register("cb-nocap", &verifying, "user-nc")
        .await
        .unwrap();
    let _device = server.attach_test_device("cb-nocap").await;
    // Brief grace so the hub finishes registering + marking online.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let token = mint_cp_jwt("user-nc");

    let response = reqwest::Client::new()
        .post(format!("{}/api/control/browser", server.http_base_url()))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "device_id": "cb-nocap",
            "session_id": "x",
            "action": "snapshot",
            "timeout_ms": 5_000,
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "NO_BROWSER_CAPABILITY");

    drop(_device);
    server.shutdown().await;
}

#[tokio::test]
async fn control_browser_504_on_timeout() {
    let server = spawn_server_with_state(support::test_state().await).await;
    // Attach a daemon that will receive the BrowserRequest but never
    // respond, forcing the hub-side timeout to fire.
    let _device = attach_owned_browser_device(&server, "cb-timeout", "user-to").await;
    let token = mint_cp_jwt("user-to");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let response = client
        .post(format!("{}/api/control/browser", server.http_base_url()))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "device_id": "cb-timeout",
            "session_id": "x",
            "action": "snapshot",
            // 1s timeout — the hub will surface 504 well before reqwest's
            // own 5s timeout fires.
            "timeout_ms": 1_000,
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::GATEWAY_TIMEOUT);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "TIMEOUT");

    drop(_device);
    server.shutdown().await;
}

#[tokio::test]
async fn control_browser_returns_binary_data_base64_encoded() {
    // Round-trip a fake-PNG payload through the daemon-respond path
    // and confirm the base64 wire encoding survives intact, plus
    // duration_ms is reported.
    let server = spawn_server_with_state(support::test_state().await).await;
    let mut device = attach_owned_browser_device(&server, "cb-binary", "user-bin").await;
    let token = mint_cp_jwt("user-bin");

    let fake_png: Vec<u8> = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0xDE, 0xAD];

    let api_task = {
        let base_url = server.http_base_url().to_string();
        let token = token.clone();
        tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("{base_url}/api/control/browser"))
                .bearer_auth(&token)
                .json(&serde_json::json!({
                    "device_id": "cb-binary",
                    "session_id": "sess-bin",
                    "action": "screenshot",
                    "timeout_ms": 10_000,
                }))
                .send()
                .await
                .unwrap()
        })
    };

    let req = device.recv_browser_request().await;
    device
        .send_browser_response(BrowserResponse {
            request_id: req.request_id.clone(),
            session_id: "sess-bin".into(),
            success: true,
            result_json: String::new(),
            error: String::new(),
            binary_data: fake_png.clone(),
            binary_mime: "image/png".into(),
        })
        .await;

    let response = api_task.await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["success"], true);
    assert_eq!(body["binary_mime"], "image/png");
    let encoded = body["binary_data"].as_str().unwrap();
    use base64::Engine as _;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    assert_eq!(decoded, fake_png);
    assert!(body["duration_ms"].as_u64().is_some());

    drop(device);
    server.shutdown().await;
}
