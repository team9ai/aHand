mod support;

use ahand_hub::config::{Config, StoreConfig};
use ahand_hub::state::MemoryDeviceStore;
use ahand_hub_core::HubError;
use ahand_hub_core::audit::AuditFilter;
use ahand_hub_core::job::JobFilter;
use ahand_hub_store::test_support::TestStack;

use support::spawn_server_with_state;

#[tokio::test]
async fn app_state_uses_persistent_store_backends_across_restart() -> anyhow::Result<()> {
    let stack = TestStack::start().await?;
    let config = Config {
        bind_addr: "127.0.0.1:0".into(),
        service_token: "service-test-token".into(),
        dashboard_shared_password: "shared-secret".into(),
        device_bootstrap_token: "bootstrap-test-token".into(),
        device_bootstrap_device_id: "device-2".into(),
        device_hello_max_age_ms: 30_000,
        device_presence_ttl_secs: 60,
        device_presence_refresh_ms: 20_000,
        jwt_secret: "service-test-secret".into(),
        output_retention_ms: 60_000,
        store: StoreConfig::Persistent {
            database_url: stack.database_url().into(),
            redis_url: stack.redis_url().into(),
        },
    };

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
        })
        .await?;
    assert_eq!(audit_entries.len(), 1);

    Ok(())
}

#[tokio::test]
async fn persistent_presence_is_refreshed_while_device_socket_stays_open() -> anyhow::Result<()> {
    let stack = TestStack::start().await?;
    let config = Config {
        bind_addr: "127.0.0.1:0".into(),
        service_token: "service-test-token".into(),
        dashboard_shared_password: "shared-secret".into(),
        device_bootstrap_token: "bootstrap-test-token".into(),
        device_bootstrap_device_id: "device-2".into(),
        device_hello_max_age_ms: 30_000,
        device_presence_ttl_secs: 1,
        device_presence_refresh_ms: 100,
        jwt_secret: "service-test-secret".into(),
        output_retention_ms: 60_000,
        store: StoreConfig::Persistent {
            database_url: stack.database_url().into(),
            redis_url: stack.redis_url().into(),
        },
    };

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
