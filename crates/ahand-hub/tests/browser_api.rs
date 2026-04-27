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

/// Mint a control-plane JWT with a custom `device_ids` allowlist (or
/// `None` for the default — i.e. no per-device restriction). Mirrors
/// the helper in `tests/control_plane.rs` so the browser handler's
/// allowlist branch can be exercised end-to-end.
fn mint_cp_jwt_with_device_ids(external_user_id: &str, device_ids: Option<Vec<String>>) -> String {
    use ahand_hub_core::auth::mint_control_plane_jwt;
    let (token, _) = mint_control_plane_jwt(
        JWT_SECRET.as_bytes(),
        external_user_id,
        "jobs:execute",
        device_ids,
        Duration::from_secs(60),
    )
    .unwrap();
    token
}

/// Mint a control-plane JWT with a custom `scope` claim. Mirrors
/// `mint_cp_jwt_with_options` in `tests/control_plane.rs` and is used
/// to exercise the scope-claim guard at the top of
/// `browser_command_control` (control_plane.rs:626-632).
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

/// Regression: when the HTTP client disconnects mid-await (axum drops
/// the handler future before the oneshot or timeout resolves), the
/// `browser_pending` entry must still be cleaned up. The fix is an RAII
/// guard in `browser_service::execute()`; without it the entry would
/// leak for the lifetime of the hub process if the daemon never
/// responded.
#[tokio::test]
async fn browser_command_pending_cleared_on_client_cancel() {
    let state = support::test_state_with_browser_device().await;
    let server = spawn_server_with_state(state).await;
    let token = login_token(&server).await;

    // Attach a daemon that will never respond to the BrowserRequest.
    let _device = server.attach_browser_device("device-1").await;

    // Sanity: pending map starts empty.
    assert_eq!(server.state().browser_pending.len(), 0);

    // Reqwest client with a short timeout (200ms) — much shorter than
    // the server-side `timeout_ms` (5_000ms). When the client times out,
    // the connection is dropped and axum cancels the handler future
    // mid-await on the oneshot.
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(200))
        .build()
        .unwrap();

    let result = client
        .post(format!("{}/api/browser", server.http_base_url()))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "device_id": "device-1",
            "session_id": "sess-cancel",
            "action": "snapshot",
            "timeout_ms": 5_000
        }))
        .send()
        .await;
    assert!(
        result.is_err(),
        "expected client-side timeout/cancel, got {result:?}"
    );

    // After cancellation, the RAII guard's Drop should run and remove
    // the pending entry. Allow a short grace for the server-side future
    // to actually be dropped after the connection close propagates.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if server.state().browser_pending.is_empty() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "browser_pending still has {} entries after client cancel — guard did not fire",
                server.state().browser_pending.len()
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
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

    // T6 / I9: optional fields (`error`, `binary_data`, `binary_mime`)
    // must be OMITTED from the JSON object — not serialized as `null` —
    // when the daemon response carries no error and no binary payload.
    // The dashboard endpoint asserts the same invariant (see
    // `browser_command_roundtrip` above); a regression that flips to
    // "always serialize as null" would silently break SDK consumers
    // that branch on `"error" in body`.
    let body_obj = body
        .as_object()
        .expect("response body must be a JSON object");
    assert!(
        !body_obj.contains_key("error"),
        "`error` should be omitted when absent; got: {body}"
    );
    assert!(
        !body_obj.contains_key("binary_data"),
        "`binary_data` should be omitted when absent; got: {body}"
    );
    assert!(
        !body_obj.contains_key("binary_mime"),
        "`binary_mime` should be omitted when absent; got: {body}"
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
    let server = spawn_server_with_state(support::test_state_with_browser_device().await).await;
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

// ──────────────────────────────────────────────────────────────────────
// Parity tests: device-allowlist + rate-limit branches mirror the
// `/api/control/jobs` suite (see `tests/control_plane.rs::
// create_job_rejects_device_not_in_allowlist` and `rate_limit_returns_429`).
// ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn control_browser_403_when_device_not_in_allowlist() {
    // Token's `device_ids` claim restricts access to "other-device", but
    // the request targets "cb-allowlist" (which the user *does* own) —
    // the handler must refuse 403 with FORBIDDEN before any WS
    // dispatch.
    let server = spawn_server_with_state(support::test_state().await).await;
    let _device = attach_owned_browser_device(&server, "cb-allowlist", "user-allowlist").await;
    let token =
        mint_cp_jwt_with_device_ids("user-allowlist", Some(vec!["other-device".to_string()]));

    let response = reqwest::Client::new()
        .post(format!("{}/api/control/browser", server.http_base_url()))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "device_id": "cb-allowlist",
            "session_id": "x",
            "action": "snapshot",
            "timeout_ms": 5_000,
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
async fn control_browser_returns_429_when_rate_limited() {
    // Same approach as `control_plane::rate_limit_returns_429`: fire
    // many requests concurrently so they hit the per-user limiter
    // inside the same governor refill window. Default limiter is
    // burst=100, rps=10 — 150 concurrent POSTs reliably trip 429.
    //
    // We deliberately target an *unknown* device so that requests
    // which pass the rate-limit check fail fast with DEVICE_NOT_FOUND
    // instead of blocking on the WS round-trip waiting for a daemon
    // response. The rate-limit check happens BEFORE the device lookup
    // in `browser_command_control`, so the 429 path is exercised
    // identically.
    let server = spawn_server_with_state(support::test_state().await).await;
    let token = mint_cp_jwt("user-rl-browser");

    let base_url = server.http_base_url().to_string();
    let token_ref = &token;
    let base_url_ref = &base_url;
    let futures = (0..150).map(|i| async move {
        reqwest::Client::new()
            .post(format!("{base_url_ref}/api/control/browser"))
            .bearer_auth(token_ref)
            .json(&serde_json::json!({
                "device_id": "cb-rl-ghost",
                "session_id": format!("burst-{i}"),
                "action": "snapshot",
                "timeout_ms": 5_000,
            }))
            .send()
            .await
            .unwrap()
            .status()
    });
    let statuses: Vec<_> = futures_util::future::join_all(futures).await;
    assert!(
        statuses.contains(&reqwest::StatusCode::TOO_MANY_REQUESTS),
        "expected at least one 429 in {statuses:?}"
    );

    // Assert non-429 responses are exactly 404 (DEVICE_NOT_FOUND). This
    // guards against a regression that fires the rate-limiter BEFORE
    // auth/lookup — in which case unauthed callers would also receive
    // 429, and requests that pass the limiter would still be 404
    // (since the device is unknown). If a non-429, non-404 leaks
    // through (e.g. 401, 200), the ordering of guards has shifted and
    // we want to know.
    let non_429s: Vec<reqwest::StatusCode> = statuses
        .iter()
        .filter(|&&s| s != reqwest::StatusCode::TOO_MANY_REQUESTS)
        .copied()
        .collect();
    assert!(
        non_429s
            .iter()
            .all(|&s| s == reqwest::StatusCode::NOT_FOUND),
        "Expected non-429 statuses to all be 404 (DEVICE_NOT_FOUND); got: {non_429s:?}"
    );

    server.shutdown().await;
}

// ──────────────────────────────────────────────────────────────────────
// Additional gap coverage (review follow-ups).
// ──────────────────────────────────────────────────────────────────────

/// T1: scope-claim guard. The handler at
/// `crates/ahand-hub/src/http/control_plane.rs:626-632` rejects JWTs
/// whose `claims.scope != "jobs:execute"` with 403 FORBIDDEN before any
/// DB / WS work. Mirrors `create_job_rejects_wrong_scope` in
/// `tests/control_plane.rs:1815`.
#[tokio::test]
async fn control_browser_403_when_scope_not_jobs_execute() {
    let server = spawn_server_with_state(support::test_state().await).await;
    // Pre-register an owned, browser-capable device so a successful
    // path WOULD return 200 — that proves the 403 we observe is from
    // the scope guard, not a downstream failure.
    let _device = attach_owned_browser_device(&server, "cb-scope", "user-scope").await;
    let token = mint_cp_jwt_with_scope("user-scope", "jobs:read");

    let response = reqwest::Client::new()
        .post(format!("{}/api/control/browser", server.http_base_url()))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "device_id": "cb-scope",
            "session_id": "x",
            "action": "snapshot",
            "params": {},
            "timeout_ms": 1_000,
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

/// T2: idempotency — pin the CURRENT "no dedupe" behavior of
/// `correlation_id`. The wire schema accepts the field but the
/// `browser_service::execute` path does NOT dedupe (deferred — see
/// the doc-comment at `crates/ahand-hub/src/http/control_plane.rs:24-29`).
///
/// Two sequential POSTs with the same `correlation_id` must BOTH cause
/// a daemon dispatch — i.e. the daemon receives two `BrowserRequest`
/// envelopes. If a future commit accidentally implements idempotency
/// (or follows the spec), this test will fail and force a conscious
/// decision: update the test or update the spec.
#[tokio::test]
async fn control_browser_correlation_id_does_not_dedupe_currently() {
    let server = spawn_server_with_state(support::test_state().await).await;
    let mut device = attach_owned_browser_device(&server, "cb-corr", "user-corr").await;
    let token = mint_cp_jwt("user-corr");

    let base_url = server.http_base_url().to_string();

    // First POST — kick off, then immediately respond from the daemon
    // so the request completes cleanly before the second POST starts.
    let first = {
        let base_url = base_url.clone();
        let token = token.clone();
        tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("{base_url}/api/control/browser"))
                .bearer_auth(&token)
                .json(&serde_json::json!({
                    "device_id": "cb-corr",
                    "session_id": "sess-corr-1",
                    "action": "snapshot",
                    "timeout_ms": 5_000,
                    "correlation_id": "dup-corr-id",
                }))
                .send()
                .await
                .unwrap()
        })
    };
    let req1 = device.recv_browser_request().await;
    device
        .send_browser_response(BrowserResponse {
            request_id: req1.request_id.clone(),
            session_id: req1.session_id.clone(),
            success: true,
            result_json: r#"{"ok":1}"#.into(),
            error: String::new(),
            binary_data: Vec::new(),
            binary_mime: String::new(),
        })
        .await;
    let resp1 = first.await.unwrap();
    assert_eq!(resp1.status(), reqwest::StatusCode::OK);

    // Second POST — same `correlation_id`. Under "no dedupe" this MUST
    // hit the daemon a second time and complete on its own merits.
    let second = {
        let base_url = base_url.clone();
        let token = token.clone();
        tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("{base_url}/api/control/browser"))
                .bearer_auth(&token)
                .json(&serde_json::json!({
                    "device_id": "cb-corr",
                    "session_id": "sess-corr-2",
                    "action": "snapshot",
                    "timeout_ms": 5_000,
                    "correlation_id": "dup-corr-id",
                }))
                .send()
                .await
                .unwrap()
        })
    };
    // Crucial assertion: the daemon receives a SECOND BrowserRequest.
    // If the hub had deduped on `correlation_id`, this `recv` would
    // hang and the test would fail via the per-test timeout (or via
    // `second` panicking on the JSON shape if the hub answered from
    // a cache).
    let req2 = tokio::time::timeout(Duration::from_secs(3), device.recv_browser_request())
        .await
        .expect(
            "daemon did not receive a second BrowserRequest within 3s — \
         the hub appears to be deduping on correlation_id, which \
         contradicts the documented current behavior at \
         control_plane.rs:24-29. Update this test or the spec.",
        );
    // Distinct request_id confirms the hub minted a fresh dispatch
    // rather than replaying the first one.
    assert_ne!(
        req2.request_id, req1.request_id,
        "request_ids must differ — same id would imply hub-side replay/dedupe"
    );
    assert_eq!(req2.session_id, "sess-corr-2");
    device
        .send_browser_response(BrowserResponse {
            request_id: req2.request_id.clone(),
            session_id: req2.session_id.clone(),
            success: true,
            result_json: r#"{"ok":2}"#.into(),
            error: String::new(),
            binary_data: Vec::new(),
            binary_mime: String::new(),
        })
        .await;
    let resp2 = second.await.unwrap();
    assert_eq!(resp2.status(), reqwest::StatusCode::OK);
    let body2: serde_json::Value = resp2.json().await.unwrap();
    assert_eq!(body2["success"], true);
    assert_eq!(body2["data"]["ok"], 2);

    drop(device);
    server.shutdown().await;
}

/// T3: `BrowserServiceError::ChannelClosed` → 500 INTERNAL_ERROR.
///
/// Reproduced by reaching into `state.browser_pending` after the
/// envelope has been dispatched and dropping the oneshot `tx`. That
/// makes `rx.await` in `browser_service::execute` resolve with
/// `Err(RecvError)` → `ChannelClosed` → 500. Without this test, a
/// future change that re-routed the channel (or swallowed the error)
/// could go unnoticed.
///
/// We poll the daemon side for the `BrowserRequest` first to ensure
/// the pending entry has been inserted (the handler inserts BEFORE
/// dispatch), then yank-and-drop the sender out of `browser_pending`.
#[tokio::test]
async fn control_browser_500_when_response_channel_closed() {
    let server = spawn_server_with_state(support::test_state().await).await;
    let mut device = attach_owned_browser_device(&server, "cb-chan", "user-chan").await;
    let token = mint_cp_jwt("user-chan");

    let api_task = {
        let base_url = server.http_base_url().to_string();
        let token = token.clone();
        tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("{base_url}/api/control/browser"))
                .bearer_auth(&token)
                .json(&serde_json::json!({
                    "device_id": "cb-chan",
                    "session_id": "sess-chan",
                    "action": "snapshot",
                    // Long enough that the test forces ChannelClosed
                    // before timeout could fire.
                    "timeout_ms": 30_000,
                }))
                .send()
                .await
                .unwrap()
        })
    };

    // Wait until the daemon has received the BrowserRequest. By that
    // point the handler has already inserted the pending oneshot.
    let req = device.recv_browser_request().await;

    // Yank the sender out of `browser_pending` and drop it. The handler
    // future is awaiting `rx`; dropping `tx` makes that resolve with
    // `Err(RecvError)` → `ChannelClosed` → 500 INTERNAL_ERROR.
    let removed = server
        .state()
        .browser_pending
        .remove(&req.request_id)
        .map(|(_, tx)| tx)
        .expect(
            "expected `browser_pending` entry for the dispatched request_id; \
             handler must insert before WS dispatch",
        );
    drop(removed);

    let response = api_task.await.unwrap();
    assert_eq!(
        response.status(),
        reqwest::StatusCode::INTERNAL_SERVER_ERROR
    );
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "INTERNAL_ERROR");

    drop(device);
    server.shutdown().await;
}

/// T4: `BrowserServiceError::SendFailed` → 404 DEVICE_OFFLINE (race).
///
/// This is distinct from the "device row says offline" branch covered
/// by `control_browser_404_when_device_offline`. `SendFailed` fires
/// when `state.connections.send` itself returns `Err` — i.e. the
/// device was online at lookup time but the WS dropped before the hub
/// could enqueue the envelope.
///
/// Reproducing this path reliably requires either (a) injecting a mock
/// `ConnectionRegistry` that returns `Err` for a specific device, or
/// (b) winning a tight race between the device-store online check and
/// `connections.send`. Today `ConnectionRegistry` is a concrete struct
/// (no trait surface — see `crates/ahand-hub/src/ws/device_gateway.rs`),
/// so option (a) would require introducing a trait + a swappable impl
/// on `AppState` purely for tests, which is a larger refactor than
/// this gap-coverage task warrants. Option (b) is too flaky to land
/// in CI.
///
/// Filed as `#[ignore]` so it shows up in `cargo test -- --ignored`
/// and won't be silently lost. Remove the `#[ignore]` once
/// `ConnectionRegistry` grows a trait or once a deterministic harness
/// for this race exists.
#[tokio::test]
#[ignore = "Requires a mockable ConnectionRegistry to deterministically force \
            connections.send -> Err. Tracked under SendFailed branch coverage; \
            see test docstring for the unblock condition."]
async fn control_browser_404_device_offline_on_send_failure() {
    // Sketch (for the future implementer):
    //   1. Inject a `ConnectionRegistry`-equivalent that returns
    //      `Err("ws closed")` from `send()` for `device_id == "cb-send-fail"`.
    //   2. Pre-register the device as owned + mark it `online: true`
    //      in the device store (so the online check passes).
    //   3. POST /api/control/browser with `device_id: "cb-send-fail"`.
    //   4. Assert: status == 404, body["error"]["code"] == "DEVICE_OFFLINE",
    //      and the message contains "Failed to send to device:" (per
    //      `map_service_error` in `http/browser.rs:127-131`).
    panic!("see #[ignore] reason — test stub only");
}

#[tokio::test]
#[ignore = "Requires fault-injecting DeviceStore (no test surface today). \
  Unblock condition: refactor MemoryDeviceStore to support fault injection, \
  or introduce a trait with a mock that returns Err on get(). \
  This stub keeps the gap visible."]
async fn control_browser_500_when_device_store_errors() {
    // Sketch (not runnable today):
    // 1. Build harness with a DeviceStore that returns Err on get(deviceId).
    // 2. Mint a control-plane JWT.
    // 3. POST /api/control/browser.
    // 4. Assert: status == 500, error.code == "INTERNAL_ERROR".
    // 5. Verify tracing::error! captured the raw HubError.
    panic!("Stub — see #[ignore] reason");
}
