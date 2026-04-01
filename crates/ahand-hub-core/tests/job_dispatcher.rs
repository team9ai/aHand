use std::collections::HashMap;

use ahand_hub_core::HubError;
use ahand_hub_core::audit::{AuditEntry, AuditFilter};
use ahand_hub_core::job::{JobFilter, JobStatus, NewJob};
use ahand_hub_core::services::job_dispatcher::JobDispatcher;

#[tokio::test]
async fn create_job_requires_online_device() {
    let stores = ahand_hub_core::tests::fakes::offline_job_stores();
    let dispatcher = JobDispatcher::new(stores.devices, stores.jobs, stores.audit);

    let err = dispatcher
        .create_job(NewJob {
            device_id: "device-1".into(),
            tool: "git".into(),
            args: vec!["status".into()],
            cwd: Some("/tmp/demo".into()),
            env: HashMap::new(),
            timeout_ms: 30_000,
            requested_by: "service:test".into(),
        })
        .await
        .unwrap_err();

    assert_eq!(err, HubError::DeviceOffline("device-1".into()));
}

#[tokio::test]
async fn fake_job_store_persists_and_filters_jobs() {
    let stores = ahand_hub_core::tests::fakes::offline_job_stores();

    let first = stores
        .jobs
        .insert(NewJob {
            device_id: "device-1".into(),
            tool: "git".into(),
            args: vec!["status".into()],
            cwd: Some("/tmp/demo".into()),
            env: HashMap::from([("RUST_LOG".into(), "debug".into())]),
            timeout_ms: 30_000,
            requested_by: "service:test".into(),
        })
        .await
        .unwrap();
    let second = stores
        .jobs
        .insert(NewJob {
            device_id: "device-2".into(),
            tool: "ls".into(),
            args: vec!["-la".into()],
            cwd: None,
            env: HashMap::new(),
            timeout_ms: 5_000,
            requested_by: "service:other".into(),
        })
        .await
        .unwrap();

    assert_eq!(first.status, JobStatus::Pending);
    assert_eq!(
        stores
            .jobs
            .get(&first.id.to_string())
            .await
            .unwrap()
            .unwrap()
            .tool,
        "git"
    );

    stores
        .jobs
        .update_status(&second.id.to_string(), JobStatus::Running)
        .await
        .unwrap();

    let device_jobs = stores
        .jobs
        .list(JobFilter {
            device_id: Some("device-1".into()),
            status: None,
        })
        .await
        .unwrap();
    let running_jobs = stores
        .jobs
        .list(JobFilter {
            device_id: None,
            status: Some(JobStatus::Running),
        })
        .await
        .unwrap();

    assert_eq!(device_jobs.len(), 1);
    assert_eq!(device_jobs[0].id, first.id);
    assert_eq!(running_jobs.len(), 1);
    assert_eq!(running_jobs[0].id, second.id);
}

#[tokio::test]
async fn fake_audit_store_appends_and_queries_entries() {
    let stores = ahand_hub_core::tests::fakes::offline_job_stores();
    let first = AuditEntry {
        timestamp: chrono::Utc::now(),
        action: "job.created".into(),
        resource_type: "job".into(),
        resource_id: "job-1".into(),
        actor: "service:test".into(),
        detail: serde_json::json!({ "tool": "git" }),
        source_ip: None,
    };
    let second = AuditEntry {
        timestamp: chrono::Utc::now(),
        action: "device.deleted".into(),
        resource_type: "device".into(),
        resource_id: "device-9".into(),
        actor: "service:test".into(),
        detail: serde_json::json!({}),
        source_ip: Some("127.0.0.1".into()),
    };

    stores
        .audit
        .append(&[first.clone(), second.clone()])
        .await
        .unwrap();

    let job_entries = stores
        .audit
        .query(AuditFilter {
            resource_type: Some("job".into()),
            resource_id: None,
            action: None,
        })
        .await
        .unwrap();
    let delete_entries = stores
        .audit
        .query(AuditFilter {
            resource_type: None,
            resource_id: None,
            action: Some("device.deleted".into()),
        })
        .await
        .unwrap();

    assert_eq!(job_entries.len(), 1);
    assert_eq!(job_entries[0].resource_id, "job-1");
    assert_eq!(delete_entries.len(), 1);
    assert_eq!(delete_entries[0].resource_id, "device-9");
}
