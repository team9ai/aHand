mod support;

use std::time::Duration;

use ahand_hub_core::audit::{AuditEntry, AuditFilter};
use ahand_hub_core::device::NewDevice;
use ahand_hub_core::job::{JobStatus, NewJob};
use ahand_hub_core::traits::DeviceStore;
use chrono::{Duration as ChronoDuration, Utc};
use futures_util::StreamExt;
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message;

use support::{persistent_test_config, spawn_server_with_state};

async fn wait_for_audit_event(state: &ahand_hub::state::AppState, action: &str, expected: usize) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        let entries = state
            .audit_store
            .query(AuditFilter {
                action: Some(action.into()),
                ..Default::default()
            })
            .await
            .unwrap();
        if entries.len() == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {expected} audit entries for {action}, got {}",
            entries.len()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn dashboard_login_issues_a_dashboard_token_and_verify_accepts_it() {
    let state = support::test_state().await;
    let server = spawn_server_with_state(state.clone()).await;

    let response = server
        .post(
            "/api/auth/login",
            "",
            serde_json::json!({ "password": "shared-secret" }),
        )
        .await;

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let payload: Value = response.json().await.unwrap();
    let token = payload["token"]
        .as_str()
        .expect("login should return token");

    let verify = server.get("/api/auth/verify", token).await;
    assert_eq!(verify.status(), reqwest::StatusCode::OK);
    let verify_payload: Value = verify.json().await.unwrap();
    assert_eq!(verify_payload["role"], "DashboardUser");
    assert_eq!(verify_payload["subject"], "dashboard");
    wait_for_audit_event(&state, "auth.login_success", 1).await;
}

#[tokio::test]
async fn service_verify_uses_production_subject_name() {
    let state = support::test_state().await;
    let server = spawn_server_with_state(state).await;

    let verify = server.get("/api/auth/verify", "service-test-token").await;
    assert_eq!(verify.status(), reqwest::StatusCode::OK);
    let verify_payload: Value = verify.json().await.unwrap();
    assert_eq!(verify_payload["role"], "Admin");
    assert_eq!(verify_payload["subject"], "service");
}

#[tokio::test]
async fn dashboard_login_rejects_invalid_password() {
    let state = support::test_state().await;
    let server = spawn_server_with_state(state.clone()).await;

    let response = server
        .post(
            "/api/auth/login",
            "",
            serde_json::json!({ "password": "wrong-password" }),
        )
        .await;

    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    let payload: Value = response.json().await.unwrap();
    assert_eq!(payload["error"]["code"], "UNAUTHORIZED");
    assert_eq!(payload["error"]["message"], "Invalid credentials");
    wait_for_audit_event(&state, "auth.login_failed", 1).await;
}

#[tokio::test]
async fn dashboard_login_rejects_malformed_json_with_error_envelope() {
    let state = support::test_state().await;
    let server = spawn_server_with_state(state).await;

    let response = reqwest::Client::new()
        .post(format!("{}/api/auth/login", server.http_base_url()))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body("{\"password\":")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let payload: Value = response.json().await.unwrap();
    assert_eq!(payload["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn dashboard_read_endpoints_return_filtered_resources_for_dashboard_users() {
    let state = support::test_state().await;
    state
        .devices
        .insert(NewDevice {
            id: "device-3".into(),
            public_key: Some(vec![9; 32]),
            hostname: "render-node".into(),
            os: "linux".into(),
            capabilities: vec!["exec".into(), "gpu".into()],
            version: Some("0.1.2".into()),
            auth_method: "ed25519".into(),
        })
        .await
        .unwrap();
    state.devices.mark_online("device-3", "ws").await.unwrap();

    let running_job = state
        .jobs_store
        .insert(NewJob {
            device_id: "device-3".into(),
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
            device_id: "device-3".into(),
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
                resource_id: "device-3".into(),
                actor: "device".into(),
                detail: serde_json::json!({ "hostname": "render-node" }),
                source_ip: None,
            },
            AuditEntry {
                timestamp: Utc::now() - ChronoDuration::minutes(1),
                action: "job.finished".into(),
                resource_type: "job".into(),
                resource_id: finished_job.id.to_string(),
                actor: "device:device-3".into(),
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
    assert_eq!(stats["offline_devices"], 2);
    assert_eq!(stats["running_jobs"], 1);

    let device_response = server.get("/api/devices/device-3", &token).await;
    assert_eq!(device_response.status(), reqwest::StatusCode::OK);
    let device: Value = device_response.json().await.unwrap();
    assert_eq!(device["hostname"], "render-node");
    assert_eq!(device["online"], true);

    let jobs_response = server
        .get("/api/jobs?status=running&device_id=device-3", &token)
        .await;
    assert_eq!(jobs_response.status(), reqwest::StatusCode::OK);
    let jobs: Value = jobs_response.json().await.unwrap();
    let jobs = jobs.as_array().expect("jobs list should be an array");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["tool"], "render");
    assert!(jobs[0].get("env").is_none());
    assert!(jobs[0].get("requested_by").is_none());

    let paged_jobs_response = server
        .get("/api/jobs?device_id=device-3&limit=1&offset=1", &token)
        .await;
    assert_eq!(paged_jobs_response.status(), reqwest::StatusCode::OK);
    let paged_jobs: Value = paged_jobs_response.json().await.unwrap();
    let paged_jobs = paged_jobs
        .as_array()
        .expect("jobs list should remain an array under pagination");
    assert_eq!(paged_jobs.len(), 1);
    assert_eq!(paged_jobs[0]["id"], finished_job.id.to_string());

    let job_response = server
        .get(&format!("/api/jobs/{}", running_job.id), &token)
        .await;
    assert_eq!(job_response.status(), reqwest::StatusCode::OK);
    let job: Value = job_response.json().await.unwrap();
    assert_eq!(job["device_id"], "device-3");
    assert!(job.get("env").is_none());
    assert!(job.get("requested_by").is_none());

    let since = (Utc::now() - ChronoDuration::minutes(5))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        let audit_response = server
            .get(
                &format!("/api/audit-logs?action=job.finished&since={since}"),
                &token,
            )
            .await;
        assert_eq!(audit_response.status(), reqwest::StatusCode::OK);
        let audit: Value = audit_response.json().await.unwrap();
        let audit = audit
            .as_array()
            .expect("audit log response should be an array");
        if audit.len() == 1 {
            assert_eq!(audit[0]["action"], "job.finished");
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for buffered audit entry to flush"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn jobs_list_rejects_invalid_status_with_error_envelope() {
    let state = support::test_state().await;
    let token = state.auth.issue_dashboard_jwt("operator-1").unwrap();
    let server = spawn_server_with_state(state).await;

    let response = server.get("/api/jobs?status=bogus", &token).await;

    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let payload: Value = response.json().await.unwrap();
    assert_eq!(payload["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(payload["error"]["message"], "Invalid job status: bogus");
}

#[tokio::test]
async fn jobs_list_rejects_invalid_limit_with_error_envelope() {
    let state = support::test_state().await;
    let token = state.auth.issue_dashboard_jwt("operator-1").unwrap();
    let server = spawn_server_with_state(state).await;

    let response = server.get("/api/jobs?limit=abc", &token).await;

    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let payload: Value = response.json().await.unwrap();
    assert_eq!(payload["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn dashboard_websocket_rejects_anonymous_clients() {
    let state = support::test_state().await;
    let server = spawn_server_with_state(state).await;

    let result = tokio_tungstenite::connect_async(server.ws_url("/ws/dashboard"))
        .await
        .expect_err("anonymous dashboard websocket should fail");

    assert!(result.to_string().contains("401"));
}

#[tokio::test]
async fn dashboard_websocket_streams_device_and_job_events() {
    let state = support::test_state().await;
    let token = state.auth.issue_dashboard_jwt("operator-1").unwrap();
    let server = spawn_server_with_state(state).await;

    let mut dashboard_socket = server.connect_dashboard_socket(Some(&token)).await;

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
    drop(device);

    let mut events = Vec::new();
    let mut records = Vec::new();
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
                records.push(payload);
            }
        }

        if events.iter().any(|event| event == "device.online")
            && events.iter().any(|event| event == "job.created")
            && events.iter().any(|event| event == "job.sent")
            && events.iter().any(|event| event == "job.running")
            && events.iter().any(|event| event == "job.finished")
            && events.iter().any(|event| event == "device.offline")
        {
            break;
        }
    }

    assert!(events.iter().any(|event| event == "device.online"));
    assert!(events.iter().any(|event| event == "job.created"));
    assert!(events.iter().any(|event| event == "job.sent"));
    assert!(events.iter().any(|event| event == "job.running"));
    assert!(events.iter().any(|event| event == "job.finished"));
    assert!(events.iter().any(|event| event == "device.offline"));
    let created = records
        .iter()
        .find(|record| record["event"] == "job.created")
        .expect("job.created event should be present");
    let created_index = events
        .iter()
        .position(|event| event == "job.created")
        .expect("job.created should be present");
    let sent_index = events
        .iter()
        .position(|event| event == "job.sent")
        .expect("job.sent should be present");
    assert!(created_index < sent_index);
    assert_eq!(created["detail"]["status"], "pending");
}

#[tokio::test]
async fn dashboard_websocket_streams_failed_and_cancelled_events() {
    let state = support::test_state().await;
    let token = state.auth.issue_dashboard_jwt("operator-1").unwrap();
    let server = spawn_server_with_state(state).await;

    let mut dashboard_socket = server.connect_dashboard_socket(Some(&token)).await;
    let mut device = server.attach_test_device("device-1").await;

    let failed = server
        .post_json(
            "/api/jobs",
            "service-test-token",
            serde_json::json!({
                "device_id": "device-1",
                "tool": "echo",
                "args": ["bad"],
                "timeout_ms": 30_000
            }),
        )
        .await;
    let failed_job_id = failed["job_id"].as_str().unwrap().to_string();
    let _ = device.recv_job_request().await;
    device.send_finished(&failed_job_id, 1, "boom").await;

    let cancelled = server
        .post_json(
            "/api/jobs",
            "service-test-token",
            serde_json::json!({
                "device_id": "device-1",
                "tool": "sleep",
                "args": ["60"],
                "timeout_ms": 30_000
            }),
        )
        .await;
    let cancelled_job_id = cancelled["job_id"].as_str().unwrap().to_string();
    let _ = device.recv_job_request().await;
    let response = server
        .post(
            &format!("/api/jobs/{cancelled_job_id}/cancel"),
            "service-test-token",
            serde_json::json!({}),
        )
        .await;
    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
    let _ = device.recv_cancel_request().await;
    device
        .send_finished(&cancelled_job_id, -1, "cancelled")
        .await;

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

        if events.iter().any(|event| event == "job.failed")
            && events.iter().any(|event| event == "job.cancelled")
        {
            break;
        }
    }

    assert!(events.iter().any(|event| event == "job.failed"));
    assert!(events.iter().any(|event| event == "job.cancelled"));
}

#[tokio::test]
async fn job_api_returns_conflict_when_device_channel_is_stale() {
    let state = support::test_state().await;
    state
        .devices
        .insert(NewDevice {
            id: "device-8".into(),
            public_key: Some(vec![7; 32]),
            hostname: "ghost-node".into(),
            os: "linux".into(),
            capabilities: vec!["exec".into()],
            version: Some("0.1.2".into()),
            auth_method: "ed25519".into(),
        })
        .await
        .unwrap();
    state.devices.mark_offline("device-8").await.unwrap();
    let server = spawn_server_with_state(state).await;

    let response = server
        .post(
            "/api/jobs",
            "service-test-token",
            serde_json::json!({
                "device_id": "device-8",
                "tool": "echo",
                "args": ["hello"],
                "timeout_ms": 30_000
            }),
        )
        .await;

    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
}

#[tokio::test]
async fn cancel_job_returns_conflict_when_device_channel_is_stale() {
    let state = support::test_state().await;
    state
        .devices
        .insert(NewDevice {
            id: "device-9".into(),
            public_key: Some(vec![8; 32]),
            hostname: "ghost-node".into(),
            os: "linux".into(),
            capabilities: vec!["exec".into()],
            version: Some("0.1.2".into()),
            auth_method: "ed25519".into(),
        })
        .await
        .unwrap();
    state.devices.mark_offline("device-9").await.unwrap();
    let job = state
        .jobs_store
        .insert(NewJob {
            device_id: "device-9".into(),
            tool: "sleep".into(),
            args: vec!["30".into()],
            cwd: None,
            env: Default::default(),
            timeout_ms: 30_000,
            requested_by: "service:test".into(),
        })
        .await
        .unwrap();
    let server = spawn_server_with_state(state).await;

    let response = server
        .post(
            &format!("/api/jobs/{}/cancel", job.id),
            "service-test-token",
            serde_json::json!({}),
        )
        .await;

    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
}

#[tokio::test]
async fn dashboard_websocket_rejects_cross_origin_clients() {
    let state = support::test_state().await;
    let token = state.auth.issue_dashboard_jwt("operator-1").unwrap();
    let server = spawn_server_with_state(state).await;

    let request = {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let mut request = server
            .ws_url("/ws/dashboard")
            .into_client_request()
            .unwrap();
        request.headers_mut().append(
            "cookie",
            format!("ahand_hub_session={token}").parse().unwrap(),
        );
        request
            .headers_mut()
            .append("origin", "https://evil.example".parse().unwrap());
        request
    };

    let result = tokio_tungstenite::connect_async(request)
        .await
        .expect_err("cross-origin dashboard websocket should fail");

    assert!(result.to_string().contains("403"));
}

#[tokio::test]
async fn dashboard_websocket_accepts_configured_split_origin_clients() {
    let mut config = support::test_config();
    config.dashboard_allowed_origins = vec!["https://dashboard.example".into()];
    let state = ahand_hub::state::AppState::from_config(config)
        .await
        .unwrap();
    let token = state.auth.issue_dashboard_jwt("operator-1").unwrap();
    let server = spawn_server_with_state(state).await;

    let socket = server
        .connect_dashboard_socket_with_origin(Some(&token), Some("https://dashboard.example"))
        .await;

    drop(socket);
}

#[tokio::test]
async fn dashboard_websocket_receives_events_from_another_persistent_instance() -> anyhow::Result<()>
{
    let stack = ahand_hub_store::test_support::TestStack::start().await?;
    let config = persistent_test_config(&stack);

    let state_a = ahand_hub::state::AppState::from_config(config.clone()).await?;
    let state_b = ahand_hub::state::AppState::from_config(config).await?;
    let token = state_b.auth.issue_dashboard_jwt("operator-1")?;

    let server_a = spawn_server_with_state(state_a).await;
    let server_b = spawn_server_with_state(state_b).await;
    let mut dashboard_socket = server_b.connect_dashboard_socket(Some(&token)).await;

    let mut device = server_a
        .attach_bootstrap_device("device-2", "bootstrap-test-token")
        .await;
    let created = server_a
        .post_json(
            "/api/jobs",
            "service-test-token",
            serde_json::json!({
                "device_id": "device-2",
                "tool": "echo",
                "args": ["hello"],
                "timeout_ms": 30_000
            }),
        )
        .await;
    let job_id = created["job_id"].as_str().unwrap().to_string();
    let _ = device.recv_job_request().await;
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

        if events.iter().any(|event| event == "job.created")
            && events.iter().any(|event| event == "job.finished")
        {
            break;
        }
    }

    assert!(events.iter().any(|event| event == "device.online"));
    assert!(events.iter().any(|event| event == "job.created"));
    assert!(events.iter().any(|event| event == "job.finished"));
    Ok(())
}
