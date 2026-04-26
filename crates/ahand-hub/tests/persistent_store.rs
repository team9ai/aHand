mod support;

use std::time::Duration;

use ahand_hub::config::{Config, StoreConfig};
use ahand_hub::state::MemoryDeviceStore;
use ahand_hub_core::HubError;
use ahand_hub_core::audit::AuditFilter;
use ahand_hub_core::job::JobFilter;
use ahand_hub_store::test_support::TestStack;
use futures_util::{SinkExt, StreamExt};
use prost::Message as _;
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message;

use support::spawn_server_with_state;

fn persistent_config(stack: &TestStack) -> Config {
    Config {
        bind_addr: "127.0.0.1:0".into(),
        service_token: "service-test-token".into(),
        dashboard_shared_password: "shared-secret".into(),
        dashboard_allowed_origins: Vec::new(),
        device_bootstrap_token: "bootstrap-test-token".into(),
        device_bootstrap_device_id: "device-2".into(),
        device_hello_max_age_ms: 30_000,
        device_staleness_probe_interval_ms: 30_000,
        device_staleness_timeout_ms: 180_000,
        device_expected_heartbeat_secs: 60,
        device_presence_ttl_secs: 60,
        device_presence_refresh_ms: 20_000,
        job_timeout_grace_ms: 50,
        device_disconnect_grace_ms: 100,
        jwt_secret: "service-test-secret".into(),
        audit_retention_days: 90,
        audit_fallback_path: std::env::temp_dir()
            .join("ahand-hub-persistent-store-audit-fallback.jsonl"),
        output_retention_ms: 60_000,
        webhook_url: None,
        webhook_secret: None,
        webhook_max_retries: 8,
        webhook_max_concurrency: 50,
        webhook_timeout_ms: 5_000,
        store: StoreConfig::Persistent {
            database_url: stack.database_url().into(),
            redis_url: stack.redis_url().into(),
        },
        s3: None,
    }
}

#[tokio::test]
async fn app_state_uses_persistent_store_backends_across_restart() -> anyhow::Result<()> {
    let stack = TestStack::start().await?;
    let config = persistent_config(&stack);

    let state = ahand_hub::state::AppState::from_config(config.clone()).await?;
    let server = spawn_server_with_state(state).await;
    let mut device = server
        .attach_bootstrap_device("device-2", "bootstrap-test-token")
        .await;

    let response = server
        .post(
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
    assert!(
        response.status().is_success(),
        "unexpected create-job status {}",
        response.status()
    );
    let created: serde_json::Value = response.json().await?;
    let _ = device.recv_job_request().await;
    drop(device);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    server.shutdown().await;

    let restarted = ahand_hub::state::AppState::from_config(config).await?;
    let devices = restarted.device_manager.list_devices().await?;
    assert!(devices.iter().any(|device| device.id == "device-2"));

    let jobs = restarted.jobs_store.list(JobFilter::default()).await?;
    assert!(
        jobs.iter()
            .any(|job| job.id.to_string() == created["job_id"])
    );

    let audit_entries = restarted
        .audit_store
        .query(AuditFilter {
            resource_type: Some("job".into()),
            resource_id: Some(created["job_id"].as_str().unwrap().into()),
            action: Some("job.created".into()),
            ..Default::default()
        })
        .await?;
    assert_eq!(audit_entries.len(), 1);

    Ok(())
}

#[tokio::test]
async fn persistent_bootstrap_tokens_survive_restart_until_first_use() -> anyhow::Result<()> {
    let stack = TestStack::start().await?;
    let config = persistent_config(&stack);

    let state = ahand_hub::state::AppState::from_config(config.clone()).await?;
    let server = spawn_server_with_state(state).await;
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
    let payload: Value = response.json().await?;
    let bootstrap_token = payload["bootstrap_token"]
        .as_str()
        .expect("create device should return bootstrap token")
        .to_string();
    server.shutdown().await;

    let restarted = ahand_hub::state::AppState::from_config(config).await?;
    assert!(restarted.auth.verify_jwt(&bootstrap_token).is_err());
    let restarted = spawn_server_with_state(restarted).await;
    let mut socket = tokio_tungstenite::connect_async(restarted.ws_url("/ws"))
        .await?
        .0;
    let challenge = support::read_hello_challenge(&mut socket).await;
    let hello = support::bootstrap_hello("device-9", &bootstrap_token, &challenge.nonce);
    socket
        .send(Message::Binary(hello.encode_to_vec().into()))
        .await?;
    let _accepted = support::read_hello_accepted(&mut socket).await;

    Ok(())
}

#[tokio::test]
async fn persistent_presence_is_refreshed_while_device_socket_stays_open() -> anyhow::Result<()> {
    let stack = TestStack::start().await?;
    let mut config = persistent_config(&stack);
    config.device_presence_ttl_secs = 1;
    config.device_presence_refresh_ms = 100;

    let state = ahand_hub::state::AppState::from_config(config).await?;
    let server = spawn_server_with_state(state).await;
    let _device = server
        .attach_bootstrap_device("device-2", "bootstrap-test-token")
        .await;

    tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;

    let device = server
        .get_json("/api/devices/device-2", "service-test-token")
        .await;
    assert_eq!(device["online"], true);

    Ok(())
}

#[tokio::test]
async fn persistent_mark_online_rejects_missing_device_without_leaking_presence()
-> anyhow::Result<()> {
    let stack = TestStack::start().await?;
    let devices = MemoryDeviceStore::with_persistent(stack.devices.clone());

    let err = devices
        .mark_online("missing-device", "ws")
        .await
        .unwrap_err();

    assert_eq!(err, HubError::DeviceNotFound("missing-device".into()));
    assert!(!stack.presence.is_online("missing-device").await?);

    Ok(())
}

#[tokio::test]
async fn persistent_output_history_replays_after_restart() -> anyhow::Result<()> {
    let stack = TestStack::start().await?;
    let config = persistent_config(&stack);

    let state = ahand_hub::state::AppState::from_config(config.clone()).await?;
    let server = spawn_server_with_state(state).await;
    let mut device = server
        .attach_bootstrap_device("device-2", "bootstrap-test-token")
        .await;

    let created = server
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
    device.send_stdout(&job_id, b"hello\n").await;
    device.send_finished(&job_id, 0, "").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    server.shutdown().await;

    let restarted = ahand_hub::state::AppState::from_config(config).await?;
    let restarted = spawn_server_with_state(restarted).await;
    let body = restarted
        .read_sse_for(
            &format!("/api/jobs/{job_id}/output"),
            "service-test-token",
            Duration::from_millis(500),
        )
        .await;

    assert!(body.contains("event: stdout"));
    assert!(body.contains("data: hello"));
    assert!(body.contains("event: finished"));
    Ok(())
}

#[tokio::test]
async fn persistent_dashboard_fanout_reaches_other_instances() -> anyhow::Result<()> {
    let stack = TestStack::start().await?;
    let config = persistent_config(&stack);

    let publisher_state = ahand_hub::state::AppState::from_config(config.clone()).await?;
    let subscriber_state = ahand_hub::state::AppState::from_config(config).await?;
    let dashboard_token = subscriber_state.auth.issue_dashboard_jwt("operator-1")?;
    let publisher = spawn_server_with_state(publisher_state).await;
    let subscriber = spawn_server_with_state(subscriber_state).await;
    let mut dashboard_socket = subscriber
        .connect_dashboard_socket(Some(&dashboard_token))
        .await;

    let mut device = publisher
        .attach_bootstrap_device("device-2", "bootstrap-test-token")
        .await;
    let created = publisher
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

        if events.iter().any(|event| event == "job.created")
            && events.iter().any(|event| event == "job.running")
            && events.iter().any(|event| event == "job.finished")
        {
            break;
        }
    }

    assert!(events.iter().any(|event| event == "job.created"));
    assert!(events.iter().any(|event| event == "job.running"));
    assert!(events.iter().any(|event| event == "job.finished"));
    Ok(())
}
