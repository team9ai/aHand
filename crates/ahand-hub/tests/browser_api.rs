mod support;

use ahand_protocol::BrowserResponse;
use support::spawn_server_with_state;

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
