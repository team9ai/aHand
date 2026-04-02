use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use ahand_hub_core::audit::AuditFilter;
use ahand_hub_core::device::NewDevice;
use ahand_hub_core::job::{JobFilter, JobStatus, NewJob};
use ahand_hub_core::services::job_dispatcher::JobDispatcher;
use ahand_hub_store::job_output_store::{JobOutputRecord, RedisJobOutputStore};
use ahand_hub_core::traits::{AuditStore, DeviceStore, JobStore};
use ahand_hub_store::test_support::TestStack;
use sqlx::Row;

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

    let audit_entries = stack
        .audit
        .query(AuditFilter {
            resource_type: Some("job".into()),
            resource_id: Some(created_job.id.to_string()),
            action: Some("job.created".into()),
        })
        .await?;
    assert_eq!(audit_entries.len(), 1);

    Ok(())
}

#[tokio::test]
async fn deleting_a_device_clears_presence() -> anyhow::Result<()> {
    let stack = TestStack::start().await?;

    stack
        .devices
        .insert(NewDevice {
            id: "device-2".into(),
            public_key: Some(vec![8; 32]),
            hostname: "devbox".into(),
            os: "linux".into(),
            capabilities: vec!["exec".into()],
            version: Some("0.1.2".into()),
            auth_method: "ed25519".into(),
        })
        .await?;
    stack.presence.mark_online("device-2", "ws").await?;
    assert!(stack.presence.is_online("device-2").await?);

    stack.devices.delete("device-2").await?;

    assert!(!stack.presence.is_online("device-2").await?);

    Ok(())
}

#[tokio::test]
async fn updating_job_status_records_lifecycle_timestamps() -> anyhow::Result<()> {
    let stack = TestStack::start().await?;

    stack
        .devices
        .insert(NewDevice {
            id: "device-3".into(),
            public_key: Some(vec![7; 32]),
            hostname: "devbox".into(),
            os: "linux".into(),
            capabilities: vec!["exec".into()],
            version: Some("0.1.2".into()),
            auth_method: "ed25519".into(),
        })
        .await?;

    let job = stack
        .jobs
        .insert(NewJob {
            device_id: "device-3".into(),
            tool: "echo".into(),
            args: vec!["hello".into()],
            cwd: None,
            env: HashMap::new(),
            timeout_ms: 30_000,
            requested_by: "service:test".into(),
        })
        .await?;

    stack
        .jobs
        .update_status(&job.id.to_string(), JobStatus::Running)
        .await?;
    stack
        .jobs
        .update_status(&job.id.to_string(), JobStatus::Finished)
        .await?;
    stack
        .jobs
        .update_terminal(&job.id.to_string(), 0, "", "completed successfully")
        .await?;

    let pool = ahand_hub_store::postgres::connect_database(stack.database_url()).await?;
    let row = sqlx::query(
        "SELECT started_at, finished_at, exit_code, error, output_summary FROM jobs WHERE id = $1",
    )
    .bind(job.id)
    .fetch_one(&pool)
    .await?;

    let started_at = row.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("started_at")?;
    let finished_at = row.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("finished_at")?;
    let exit_code = row.try_get::<Option<i32>, _>("exit_code")?;
    let error = row.try_get::<Option<String>, _>("error")?;
    let output_summary = row.try_get::<Option<String>, _>("output_summary")?;
    assert!(
        started_at.is_some(),
        "running transition should set started_at"
    );
    assert!(
        finished_at.is_some(),
        "terminal transition should set finished_at"
    );
    assert_eq!(exit_code, Some(0));
    assert_eq!(error.as_deref(), Some(""));
    assert_eq!(output_summary.as_deref(), Some("completed successfully"));

    let stored = stack
        .jobs
        .get(&job.id.to_string())
        .await?
        .expect("job exists");
    assert_eq!(stored.exit_code, Some(0));
    assert_eq!(stored.error.as_deref(), Some(""));
    assert_eq!(
        stored.output_summary.as_deref(),
        Some("completed successfully")
    );
    assert!(stored.started_at.is_some());
    assert!(stored.finished_at.is_some());

    Ok(())
}

#[tokio::test]
async fn redis_output_store_roundtrips_history_and_expires_terminal_streams() -> anyhow::Result<()> {
    let stack = TestStack::start().await?;
    let store = RedisJobOutputStore::new(
        ahand_hub_store::redis::connect_redis(stack.redis_url()).await?,
        Duration::from_millis(200),
    );

    let stdout = store
        .append("job-1", JobOutputRecord::Stdout("hello".into()))
        .await?;
    let progress = store.append("job-1", JobOutputRecord::Progress(42)).await?;
    let finished = store
        .append(
            "job-1",
            JobOutputRecord::Finished {
                exit_code: 0,
                error: String::new(),
            },
        )
        .await?;

    assert_eq!(stdout.seq, 1);
    assert_eq!(progress.seq, 2);
    assert_eq!(finished.seq, 3);

    let history = store.read_history("job-1").await?;
    assert_eq!(history.len(), 3);
    assert!(matches!(history[0].record, JobOutputRecord::Stdout(_)));
    assert!(matches!(history[1].record, JobOutputRecord::Progress(42)));
    assert!(matches!(history[2].record, JobOutputRecord::Finished { exit_code: 0, .. }));

    tokio::time::sleep(Duration::from_millis(300)).await;
    let expired = store.read_history("job-1").await?;
    assert!(expired.is_empty());

    Ok(())
}
