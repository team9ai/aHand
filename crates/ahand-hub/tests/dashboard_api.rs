mod support;

use std::time::Duration;

use ahand_hub::state::AppState;
use ahand_hub_core::audit::AuditEntry;
use ahand_hub_core::device::NewDevice;
use ahand_hub_core::job::{JobStatus, NewJob};
use ahand_hub_core::traits::DeviceStore;
use chrono::{Duration as ChronoDuration, Utc};
use futures_util::StreamExt;
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message;

use support::spawn_server_with_state;

#[tokio::test]
async fn dashboard_login_issues_a_dashboard_token_and_verify_accepts_it() {
    let state = AppState::for_tests().await;
    let server = spawn_server_with_state(state).await;

    let response = server
        .post(
            "/api/auth/login",
            "",
            serde_json::json!({ "password": "shared-secret" }),
        )
        .await;

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let payload: Value = response.json().await.unwrap();
    let token = payload["token"].as_str().expect("login should return token");

    let verify = server.get("/api/auth/verify", token).await;
    assert_eq!(verify.status(), reqwest::StatusCode::OK);
    let verify_payload: Value = verify.json().await.unwrap();
    assert_eq!(verify_payload["role"], "DashboardUser");
    assert_eq!(verify_payload["subject"], "dashboard");
}

#[tokio::test]
async fn dashboard_login_rejects_invalid_password() {
    let state = AppState::for_tests().await;
    let server = spawn_server_with_state(state).await;

    let response = server
        .post(
            "/api/auth/login",
            "",
            serde_json::json!({ "password": "wrong-password" }),
        )
        .await;

    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    let payload: Value = response.json().await.unwrap();
    assert_eq!(payload["error"], "invalid_credentials");
}

#[tokio::test]
async fn dashboard_read_endpoints_return_filtered_resources_for_dashboard_users() {
    let state = AppState::for_tests().await;
    state
        .devices
        .insert(NewDevice {
            id: "device-2".into(),
            public_key: Some(vec![9; 32]),
            hostname: "render-node".into(),
            os: "linux".into(),
            capabilities: vec!["exec".into(), "gpu".into()],
            version: Some("0.1.2".into()),
            auth_method: "ed25519".into(),
        })
        .await
        .unwrap();

    let running_job = state
        .jobs_store
        .insert(NewJob {
            device_id: "device-2".into(),
            tool: "render".into(),
            args: vec!["scene.blend".into()],
            cwd: Some("/srv/work".into()),
            env: Default::default(),
            timeout_ms: 30_000,
            requested_by: "operator".into(),
        })
        .await
        .unwrap();
    state
        .jobs_store
        .update_status(&running_job.id.to_string(), JobStatus::Running)
        .await
        .unwrap();

    let finished_job = state
        .jobs_store
        .insert(NewJob {
            device_id: "device-1".into(),
            tool: "echo".into(),
            args: vec!["done".into()],
            cwd: None,
            env: Default::default(),
            timeout_ms: 5_000,
            requested_by: "operator".into(),
        })
        .await
        .unwrap();
    state
        .jobs_store
        .update_status(&finished_job.id.to_string(), JobStatus::Finished)
        .await
        .unwrap();

    state
        .audit_store
        .append(&[
            AuditEntry {
                timestamp: Utc::now() - ChronoDuration::minutes(10),
                action: "device.online".into(),
                resource_type: "device".into(),
                resource_id: "device-2".into(),
                actor: "device".into(),
                detail: serde_json::json!({ "hostname": "render-node" }),
                source_ip: None,
            },
            AuditEntry {
                timestamp: Utc::now() - ChronoDuration::minutes(1),
                action: "job.finished".into(),
                resource_type: "job".into(),
                resource_id: finished_job.id.to_string(),
                actor: "device:device-1".into(),
                detail: serde_json::json!({ "status": "finished" }),
                source_ip: None,
            },
        ])
        .await
        .unwrap();

    let token = state.auth.issue_dashboard_jwt("operator-1").unwrap();
    let server = spawn_server_with_state(state).await;

    let stats_response = server.get("/api/stats", &token).await;
    assert_eq!(stats_response.status(), reqwest::StatusCode::OK);
    let stats: Value = stats_response.json().await.unwrap();
    assert_eq!(stats["online_devices"], 1);
    assert_eq!(stats["offline_devices"], 1);
    assert_eq!(stats["running_jobs"], 1);

    let device_response = server.get("/api/devices/device-2", &token).await;
    assert_eq!(device_response.status(), reqwest::StatusCode::OK);
    let device: Value = device_response.json().await.unwrap();
    assert_eq!(device["hostname"], "render-node");
    assert_eq!(device["online"], true);

    let jobs_response = server
        .get("/api/jobs?status=running&device_id=device-2", &token)
        .await;
    assert_eq!(jobs_response.status(), reqwest::StatusCode::OK);
    let jobs: Value = jobs_response.json().await.unwrap();
    let jobs = jobs.as_array().expect("jobs list should be an array");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["tool"], "render");

    let job_response = server
        .get(&format!("/api/jobs/{}", running_job.id), &token)
        .await;
    assert_eq!(job_response.status(), reqwest::StatusCode::OK);
    let job: Value = job_response.json().await.unwrap();
    assert_eq!(job["device_id"], "device-2");

    let since = (Utc::now() - ChronoDuration::minutes(5))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let audit_response = server
        .get(
            &format!("/api/audit-logs?action=job.finished&since={since}"),
            &token,
        )
        .await;
    assert_eq!(audit_response.status(), reqwest::StatusCode::OK);
    let audit: Value = audit_response.json().await.unwrap();
    let audit = audit.as_array().expect("audit log response should be an array");
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0]["action"], "job.finished");
}

#[tokio::test]
async fn dashboard_websocket_rejects_anonymous_clients() {
    let state = AppState::for_tests().await;
    let server = spawn_server_with_state(state).await;

    let result = tokio_tungstenite::connect_async(server.ws_url("/ws/dashboard"))
        .await
        .expect_err("anonymous dashboard websocket should fail");

    assert!(result.to_string().contains("401"));
}

#[tokio::test]
async fn dashboard_websocket_streams_device_and_job_events() {
    let state = AppState::for_tests().await;
    let token = state.auth.issue_dashboard_jwt("operator-1").unwrap();
    let server = spawn_server_with_state(state).await;

    let (mut dashboard_socket, _) = tokio_tungstenite::connect_async(server.ws_url(&format!(
        "/ws/dashboard?token={token}"
    )))
    .await
    .unwrap();

    let mut device = server.attach_test_device("device-1").await;
    let created = server
        .post_json(
            "/api/jobs",
            "service-test-token",
            serde_json::json!({
                "device_id": "device-1",
                "tool": "echo",
                "args": ["hello"],
                "timeout_ms": 30_000
            }),
        )
        .await;
    let job_id = created["job_id"].as_str().unwrap().to_string();

    let _ = device.recv_job_request().await;
    device.send_stdout(&job_id, b"hello\n").await;
    device.send_finished(&job_id, 0, "").await;

    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let message = tokio::time::timeout(remaining, dashboard_socket.next())
            .await
            .expect("dashboard websocket should yield event")
            .expect("dashboard websocket should stay open")
            .expect("dashboard websocket should not error");

        if let Message::Text(text) = message {
            let payload: Value = serde_json::from_str(text.as_str()).unwrap();
            if let Some(event) = payload["event"].as_str() {
                events.push(event.to_string());
            }
        }

        if events.iter().any(|event| event == "device.online")
            && events.iter().any(|event| event == "job.created")
            && events.iter().any(|event| event == "job.running")
            && events.iter().any(|event| event == "job.finished")
        {
            break;
        }
    }

    assert!(events.iter().any(|event| event == "device.online"));
    assert!(events.iter().any(|event| event == "job.created"));
    assert!(events.iter().any(|event| event == "job.running"));
    assert!(events.iter().any(|event| event == "job.finished"));
}
