mod support;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use ahand_hub_core::audit::{AuditEntry, AuditFilter};
use ahand_hub_core::device::NewDevice;
use ahand_hub_core::job::{JobFilter, JobStatus, NewJob};
use ahand_hub_core::services::job_dispatcher::JobDispatcher;
use ahand_hub_core::traits::{AuditStore, DeviceStore, JobStore};
use ahand_hub_store::bootstrap_store::RedisBootstrapStore;
use ahand_hub_store::job_output_store::{JobOutputRecord, RedisJobOutputStore};
use chrono::{Duration as ChronoDuration, Utc};
use sqlx::Row;
use support::TestStack;

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
            external_user_id: None,
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
            interactive: false,
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
            ..Default::default()
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
            external_user_id: None,
        })
        .await?;
    stack.presence.mark_online("device-2", "ws").await?;
    assert!(stack.presence.is_online("device-2").await?);

    stack.devices.delete("device-2").await?;

    assert!(!stack.presence.is_online("device-2").await?);

    Ok(())
}

#[tokio::test]
async fn redis_presence_store_reads_multiple_device_states_in_one_call() -> anyhow::Result<()> {
    let stack = TestStack::start().await?;
    stack
        .presence
        .mark_online("device-1", "127.0.0.1:12345")
        .await?;
    stack
        .presence
        .mark_online("device-3", "127.0.0.1:12346")
        .await?;

    let states = stack
        .presence
        .online_states(&[
            "device-1".to_string(),
            "device-2".to_string(),
            "device-3".to_string(),
        ])
        .await?;

    assert_eq!(states.get("device-1"), Some(&true));
    assert_eq!(states.get("device-2"), Some(&false));
    assert_eq!(states.get("device-3"), Some(&true));

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
            external_user_id: None,
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
            interactive: false,
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
async fn redis_output_store_roundtrips_history_and_expires_terminal_streams() -> anyhow::Result<()>
{
    let stack = TestStack::start().await?;
    let store = RedisJobOutputStore::new(stack.redis_url(), Duration::from_millis(200)).await?;

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
    assert!(matches!(
        history[2].record,
        JobOutputRecord::Finished { exit_code: 0, .. }
    ));

    tokio::time::sleep(Duration::from_millis(300)).await;
    let expired = store.read_history("job-1").await?;
    assert!(expired.is_empty());

    Ok(())
}

#[tokio::test]
async fn redis_output_live_reads_do_not_block_appends() -> anyhow::Result<()> {
    let stack = TestStack::start().await?;
    let store = RedisJobOutputStore::new(stack.redis_url(), Duration::from_millis(200)).await?;

    let live_store = store.clone();
    let reader = tokio::spawn(async move { live_store.read_live("job-live", "$", 500).await });
    tokio::time::sleep(Duration::from_millis(50)).await;

    tokio::time::timeout(
        Duration::from_millis(100),
        store.append("job-live", JobOutputRecord::Stdout("hello".into())),
    )
    .await
    .expect("append should not block behind live tail")?;

    reader.abort();
    let _ = reader.await;
    Ok(())
}

#[tokio::test]
async fn redis_bootstrap_store_enforces_one_time_reservations() -> anyhow::Result<()> {
    let stack = TestStack::start().await?;
    let store = RedisBootstrapStore::new(
        ahand_hub_store::redis::connect_redis(stack.redis_url()).await?,
        Duration::from_secs(5),
    );

    let token = store.issue("device-7").await?;
    let reservation = store
        .reserve("device-7", &token)
        .await?
        .expect("issued token should reserve");
    assert!(
        store.reserve("device-7", &token).await?.is_none(),
        "reserved token should not reserve twice before release"
    );

    store.release(&reservation).await?;

    let reservation = store
        .reserve("device-7", &token)
        .await?
        .expect("released token should reserve again");
    store.consume(&reservation).await?;
    assert!(
        store.reserve("device-7", &token).await?.is_none(),
        "consumed token should not reserve again"
    );

    let rotated = store.issue("device-7").await?;
    assert_ne!(rotated, token);
    store.delete_device("device-7").await?;
    assert!(store.reserve("device-7", &rotated).await?.is_none());

    Ok(())
}

#[tokio::test]
async fn concurrent_terminal_transitions_do_not_overwrite_each_other() -> anyhow::Result<()> {
    let stack = TestStack::start().await?;
    stack
        .devices
        .insert(NewDevice {
            id: "device-1".into(),
            public_key: Some(vec![7; 32]),
            hostname: "devbox".into(),
            os: "linux".into(),
            capabilities: vec!["exec".into()],
            version: Some("0.1.2".into()),
            auth_method: "ed25519".into(),
            external_user_id: None,
        })
        .await?;
    let store = stack.jobs.clone();
    let job = store
        .insert(NewJob {
            device_id: "device-1".into(),
            tool: "echo".into(),
            args: vec!["hello".into()],
            cwd: None,
            env: Default::default(),
            timeout_ms: 30_000,
            requested_by: "service".into(),
            interactive: false,
        })
        .await?;

    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let finished_store = store.clone();
    let finished_job_id = job.id.to_string();
    let finished_barrier = barrier.clone();
    let finished = tokio::spawn(async move {
        finished_barrier.wait().await;
        finished_store
            .transition_status(&finished_job_id, JobStatus::Finished)
            .await
    });
    let failed_store = store.clone();
    let failed_job_id = job.id.to_string();
    let failed_barrier = barrier.clone();
    let failed = tokio::spawn(async move {
        failed_barrier.wait().await;
        failed_store
            .transition_status(&failed_job_id, JobStatus::Failed)
            .await
    });
    barrier.wait().await;

    let first = finished.await.unwrap();
    let second = failed.await.unwrap();
    let outcomes = [first, second];
    let success_count = outcomes.iter().filter(|outcome| outcome.is_ok()).count();
    assert_eq!(
        success_count, 1,
        "only one concurrent terminal transition should succeed"
    );

    let stored = store
        .get(&job.id.to_string())
        .await?
        .expect("job should still exist");
    assert!(matches!(
        stored.status,
        JobStatus::Finished | JobStatus::Failed
    ));

    Ok(())
}

#[tokio::test]
async fn audit_store_filters_orders_and_paginates_in_query() -> anyhow::Result<()> {
    let stack = TestStack::start().await?;
    let base = Utc::now();
    stack
        .audit
        .append(&[
            AuditEntry {
                timestamp: base,
                action: "job.finished".into(),
                resource_type: "job".into(),
                resource_id: "job-1".into(),
                actor: "service:test".into(),
                detail: serde_json::json!({ "ordinal": 1 }),
                source_ip: None,
            },
            AuditEntry {
                timestamp: base + ChronoDuration::seconds(10),
                action: "job.running".into(),
                resource_type: "job".into(),
                resource_id: "job-2".into(),
                actor: "service:test".into(),
                detail: serde_json::json!({ "ordinal": 2 }),
                source_ip: None,
            },
            AuditEntry {
                timestamp: base + ChronoDuration::seconds(20),
                action: "job.finished".into(),
                resource_type: "job".into(),
                resource_id: "job-3".into(),
                actor: "service:test".into(),
                detail: serde_json::json!({ "ordinal": 3 }),
                source_ip: None,
            },
            AuditEntry {
                timestamp: base + ChronoDuration::seconds(30),
                action: "job.finished".into(),
                resource_type: "job".into(),
                resource_id: "job-4".into(),
                actor: "service:test".into(),
                detail: serde_json::json!({ "ordinal": 4 }),
                source_ip: None,
            },
        ])
        .await?;

    let entries = stack
        .audit
        .query(AuditFilter {
            action: Some("job.finished".into()),
            since: Some(base + ChronoDuration::seconds(5)),
            until: Some(base + ChronoDuration::seconds(35)),
            limit: Some(1),
            offset: Some(1),
            descending: true,
            ..Default::default()
        })
        .await?;

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].resource_id, "job-3");

    Ok(())
}

#[tokio::test]
async fn audit_store_prunes_entries_older_than_cutoff() -> anyhow::Result<()> {
    let stack = TestStack::start().await?;
    let base = Utc::now();
    stack
        .audit
        .append(&[
            AuditEntry {
                timestamp: base - ChronoDuration::days(120),
                action: "job.created".into(),
                resource_type: "job".into(),
                resource_id: "old-job".into(),
                actor: "service:test".into(),
                detail: serde_json::json!({}),
                source_ip: None,
            },
            AuditEntry {
                timestamp: base - ChronoDuration::days(10),
                action: "job.created".into(),
                resource_type: "job".into(),
                resource_id: "recent-job".into(),
                actor: "service:test".into(),
                detail: serde_json::json!({}),
                source_ip: None,
            },
        ])
        .await?;

    let removed = stack
        .audit
        .prune_before(base - ChronoDuration::days(90))
        .await?;
    assert_eq!(removed, 1);

    let remaining = stack.audit.query(AuditFilter::default()).await?;
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].resource_id, "recent-job");

    Ok(())
}
