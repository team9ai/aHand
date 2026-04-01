use std::collections::HashMap;
use std::sync::Arc;

use ahand_hub_core::audit::{AuditEntry, AuditFilter};
use ahand_hub_core::device::NewDevice;
use ahand_hub_core::job::{JobFilter, JobStatus, NewJob};
use ahand_hub_core::services::job_dispatcher::JobDispatcher;
use ahand_hub_core::traits::{AuditStore, DeviceStore, JobStore};
use ahand_hub_store::test_support::TestStack;

#[tokio::test]
async fn store_roundtrip_persists_devices_jobs_and_presence() -> anyhow::Result<()> {
    let stack = TestStack::start().await?;

    stack
        .devices
        .insert(NewDevice {
            id: "device-1".into(),
            public_key: Some(vec![9; 32]),
            hostname: "devbox".into(),
            os: "linux".into(),
            capabilities: vec!["exec".into()],
            version: Some("0.1.2".into()),
            auth_method: "ed25519".into(),
        })
        .await?;

    let stored_device = stack.devices.get("device-1").await?.expect("device exists");
    assert_eq!(stored_device.hostname, "devbox");

    stack
        .presence
        .mark_online("device-1", "127.0.0.1:12345")
        .await?;
    assert!(stack.presence.is_online("device-1").await?);
    assert!(
        stack
            .devices
            .get("device-1")
            .await?
            .expect("device exists")
            .online
    );

    let dispatcher = JobDispatcher::new(
        Arc::new(stack.devices.clone()),
        Arc::new(stack.jobs.clone()),
        Arc::new(stack.audit.clone()),
    );
    let created_job = dispatcher
        .create_job(NewJob {
            device_id: "device-1".into(),
            tool: "git".into(),
            args: vec!["status".into()],
            cwd: Some("/tmp/demo".into()),
            env: HashMap::new(),
            timeout_ms: 30_000,
            requested_by: "service:test".into(),
        })
        .await?;

    let jobs = stack
        .jobs
        .list(JobFilter {
            device_id: Some("device-1".into()),
            status: Some(JobStatus::Pending),
        })
        .await?;
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].id, created_job.id);

    stack
        .audit
        .append(&[AuditEntry {
            timestamp: chrono::Utc::now(),
            action: "job.created".into(),
            resource_type: "job".into(),
            resource_id: jobs[0].id.to_string(),
            actor: "service:test".into(),
            detail: serde_json::json!({ "tool": "git" }),
            source_ip: None,
        }])
        .await?;
    let audit_entries = stack
        .audit
        .query(AuditFilter {
            resource_type: Some("job".into()),
            resource_id: Some(jobs[0].id.to_string()),
            action: Some("job.created".into()),
        })
        .await?;
    assert_eq!(audit_entries.len(), 1);

    Ok(())
}
