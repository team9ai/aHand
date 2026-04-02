mod support;

use std::time::Duration;

use ahand_hub_core::audit::AuditFilter;
use ahand_hub_core::device::NewDevice;
use ahand_hub_core::traits::DeviceStore;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use futures_util::{SinkExt, StreamExt};
use tower::ServiceExt;

async fn wait_for_audit_count(
    state: &ahand_hub::state::AppState,
    action: &str,
    resource_id: &str,
    expected: usize,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        let entries = state
            .audit_store
            .query(AuditFilter {
                action: Some(action.into()),
                resource_type: None,
                resource_id: Some(resource_id.into()),
                ..Default::default()
            })
            .await
            .unwrap();
        if entries.len() == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {expected} audit entries for {action} {resource_id}, got {}",
            entries.len()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn health_endpoint_reports_ok() {
    let app = support::build_test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn devices_endpoint_requires_auth() {
    let server = support::spawn_server_with_state(support::test_state().await).await;
    let response = reqwest::Client::new()
        .get(format!("{}/api/devices", server.http_base_url()))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let payload: serde_json::Value = response.json().await.unwrap();
    assert_eq!(payload["error"]["code"], "UNAUTHORIZED");
}

#[tokio::test]
async fn devices_endpoint_accepts_service_token() {
    let app = support::build_test_app().await;
    let response = app
        .oneshot(support::service_request("/api/devices"))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn create_job_returns_conflict_for_offline_device() {
    let app = support::build_test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/jobs")
                .header(header::AUTHORIZATION, "Bearer service-test-token")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "device_id": "device-1",
                        "tool": "echo",
                        "args": ["hello"],
                        "timeout_ms": 30_000
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn create_job_rejects_malformed_json_with_error_envelope() {
    let server = support::spawn_server_with_state(support::test_state().await).await;
    let response = reqwest::Client::new()
        .post(format!("{}/api/jobs", server.http_base_url()))
        .bearer_auth("service-test-token")
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body("{\"device_id\":")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let payload: serde_json::Value = response.json().await.unwrap();
    assert_eq!(payload["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn create_job_returns_not_found_for_unknown_device() {
    let server = support::spawn_server_with_state(support::test_state().await).await;
    let response = server
        .post(
            "/api/jobs",
            "service-test-token",
            serde_json::json!({
                "device_id": "device-404",
                "tool": "echo",
                "args": ["hello"],
                "timeout_ms": 30_000
            }),
        )
        .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let payload: serde_json::Value = response.json().await.unwrap();
    assert_eq!(payload["error"]["code"], "DEVICE_NOT_FOUND");
    assert_eq!(
        payload["error"]["message"],
        "Device device-404 was not found"
    );
}

#[tokio::test]
async fn create_device_rejects_malformed_json_with_error_envelope() {
    let server = support::spawn_server_with_state(support::test_state().await).await;
    let response = reqwest::Client::new()
        .post(format!("{}/api/devices", server.http_base_url()))
        .bearer_auth("service-test-token")
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body("{\"id\":")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let payload: serde_json::Value = response.json().await.unwrap();
    assert_eq!(payload["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn stream_output_returns_not_found_for_unknown_job() {
    let app = support::build_test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/jobs/missing-job/output")
                .header(header::AUTHORIZATION, "Bearer service-test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_device_preregisters_device_and_returns_bootstrap_data() {
    let state = support::test_state().await;
    let server = support::spawn_server_with_state(state.clone()).await;

    let response = server
        .post(
            "/api/devices",
            "service-test-token",
            serde_json::json!({
                "id": "device-9",
                "hostname": "edge-box",
                "os": "linux",
                "capabilities": ["exec", "browser"],
                "version": "0.1.2"
            }),
        )
        .await;

    assert_eq!(response.status(), reqwest::StatusCode::CREATED);
    let payload: serde_json::Value = response.json().await.unwrap();
    assert_eq!(payload["device_id"], "device-9");
    assert!(payload["bootstrap_token"].is_string());
    let bootstrap_token = payload["bootstrap_token"].as_str().unwrap();
    assert!(state.auth.verify_jwt(bootstrap_token).is_err());

    let stored = state.devices.get("device-9").await.unwrap().unwrap();
    assert!(!stored.online);
    assert!(stored.public_key.is_none());

    let mut first = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap()
        .0;
    let challenge = support::read_hello_challenge(&mut first).await;
    let hello = support::bootstrap_hello("device-9", bootstrap_token, &challenge.nonce);
    first
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            prost::Message::encode_to_vec(&hello).into(),
        ))
        .await
        .unwrap();
    let _accepted = support::read_hello_accepted(&mut first).await;
    let _ = first.close(None).await;

    let mut replay = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap()
        .0;
    let replay_challenge = support::read_hello_challenge(&mut replay).await;
    let replay_hello =
        support::bootstrap_hello("device-9", bootstrap_token, &replay_challenge.nonce);
    replay
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            prost::Message::encode_to_vec(&replay_hello).into(),
        ))
        .await
        .unwrap();
    let replay_response = tokio::time::timeout(Duration::from_secs(1), replay.next())
        .await
        .expect("replay bootstrap handshake should terminate");
    assert!(matches!(
        replay_response,
        Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) | Some(Err(_)) | None
    ));

    wait_for_audit_count(&state, "device.registered", "device-9", 1).await;
}

#[tokio::test]
async fn device_jwt_cannot_be_reused_as_bootstrap_registration_credential() {
    let state = support::test_state().await;
    let server = support::spawn_server_with_state(state.clone()).await;

    let response = server
        .post(
            "/api/devices",
            "service-test-token",
            serde_json::json!({
                "id": "device-9",
                "hostname": "edge-box",
                "os": "linux",
                "capabilities": ["exec"],
                "version": "0.1.2"
            }),
        )
        .await;
    assert_eq!(response.status(), reqwest::StatusCode::CREATED);

    let device_jwt = state.auth.issue_device_jwt("device-9").unwrap();
    let mut socket = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap()
        .0;
    let challenge = support::read_hello_challenge(&mut socket).await;
    let hello = support::bootstrap_hello("device-9", &device_jwt, &challenge.nonce);
    socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            prost::Message::encode_to_vec(&hello).into(),
        ))
        .await
        .unwrap();

    let response = tokio::time::timeout(Duration::from_secs(1), socket.next())
        .await
        .expect("device-jwt bootstrap handshake should terminate");
    assert!(matches!(
        response,
        Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) | Some(Err(_)) | None
    ));

    let stored = state.devices.get("device-9").await.unwrap().unwrap();
    assert!(!stored.online);
    assert!(stored.public_key.is_none());
}

#[tokio::test]
async fn device_token_can_read_only_its_own_device_record() {
    let state = support::test_state().await;
    state
        .devices
        .insert(NewDevice {
            id: "device-7".into(),
            public_key: None,
            hostname: "edge-box".into(),
            os: "linux".into(),
            capabilities: vec!["exec".into(), "browser".into()],
            version: Some("0.1.2".into()),
            auth_method: "bootstrap".into(),
        })
        .await
        .unwrap();
    state.devices.mark_offline("device-7").await.unwrap();

    let device_token = state.auth.issue_device_jwt("device-7").unwrap();
    let other_device_token = state.auth.issue_device_jwt("device-1").unwrap();
    let server = support::spawn_server_with_state(state).await;

    let own_response = server.get("/api/devices/device-7", &device_token).await;
    assert_eq!(own_response.status(), reqwest::StatusCode::OK);

    let capabilities = server
        .get("/api/devices/device-7/capabilities", &device_token)
        .await;
    assert_eq!(capabilities.status(), reqwest::StatusCode::OK);

    let other_response = server
        .get("/api/devices/device-7", &other_device_token)
        .await;
    assert_eq!(other_response.status(), reqwest::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn delete_device_requires_admin_and_removes_device() {
    let state = support::test_state().await;
    state
        .devices
        .insert(NewDevice {
            id: "device-7".into(),
            public_key: None,
            hostname: "to-delete".into(),
            os: "linux".into(),
            capabilities: vec!["exec".into()],
            version: Some("0.1.2".into()),
            auth_method: "bootstrap".into(),
        })
        .await
        .unwrap();
    state.devices.mark_offline("device-7").await.unwrap();

    let dashboard_token = state.auth.issue_dashboard_jwt("operator-1").unwrap();
    let server = support::spawn_server_with_state(state.clone()).await;

    let forbidden = reqwest::Client::new()
        .delete(format!("{}/api/devices/device-7", server.http_base_url()))
        .bearer_auth(&dashboard_token)
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden.status(), reqwest::StatusCode::FORBIDDEN);

    let deleted = reqwest::Client::new()
        .delete(format!("{}/api/devices/device-7", server.http_base_url()))
        .bearer_auth("service-test-token")
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), reqwest::StatusCode::NO_CONTENT);
    assert!(state.devices.get("device-7").await.unwrap().is_none());
    wait_for_audit_count(&state, "device.deleted", "device-7", 1).await;
}

#[tokio::test]
async fn create_device_rejects_duplicate_device_ids() {
    let state = support::test_state().await;
    state
        .devices
        .insert(NewDevice {
            id: "device-9".into(),
            public_key: Some(vec![9; 32]),
            hostname: "existing-box".into(),
            os: "linux".into(),
            capabilities: vec!["exec".into()],
            version: Some("0.1.2".into()),
            auth_method: "ed25519".into(),
        })
        .await
        .unwrap();
    state.devices.mark_offline("device-9").await.unwrap();
    let server = support::spawn_server_with_state(state.clone()).await;

    let response = server
        .post(
            "/api/devices",
            "service-test-token",
            serde_json::json!({
                "id": "device-9",
                "hostname": "replacement-box",
                "os": "linux",
                "capabilities": ["browser"],
                "version": "9.9.9"
            }),
        )
        .await;

    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let stored = state.devices.get("device-9").await.unwrap().unwrap();
    assert_eq!(stored.hostname, "existing-box");
    assert_eq!(stored.public_key, Some(vec![9; 32]));
}

#[tokio::test]
async fn direct_memory_insert_starts_device_offline_until_presence_is_marked() {
    let state = support::test_state().await;
    state
        .devices
        .insert(NewDevice {
            id: "device-10".into(),
            public_key: Some(vec![10; 32]),
            hostname: "lab-node".into(),
            os: "linux".into(),
            capabilities: vec!["exec".into()],
            version: Some("0.1.2".into()),
            auth_method: "ed25519".into(),
        })
        .await
        .unwrap();

    let stored = state.devices.get("device-10").await.unwrap().unwrap();
    assert!(!stored.online);
}
