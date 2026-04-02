use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ahand_hub_core::HubError;
use ahand_hub_core::audit::{AuditEntry, AuditFilter};
use ahand_hub_core::device::{Device, NewDevice};
use ahand_hub_core::job::{JobFilter, JobStatus, NewJob};
use ahand_hub_core::services::job_dispatcher::JobDispatcher;
use ahand_hub_core::traits::{AuditStore, DeviceStore, JobStore};
use async_trait::async_trait;
use dashmap::DashMap;

struct OnlineDeviceStore {
    device: Device,
}

impl OnlineDeviceStore {
    fn new(device_id: &str) -> Self {
        Self {
            device: Device {
                id: device_id.into(),
                public_key: None,
                hostname: "online-device".into(),
                os: "linux".into(),
                capabilities: vec!["exec".into()],
                version: Some("0.1.2".into()),
                auth_method: "ed25519".into(),
                online: true,
            },
        }
    }
}

#[async_trait]
impl DeviceStore for OnlineDeviceStore {
    async fn insert(&self, _device: NewDevice) -> ahand_hub_core::Result<Device> {
        Ok(self.device.clone())
    }

    async fn get(&self, device_id: &str) -> ahand_hub_core::Result<Option<Device>> {
        Ok((self.device.id == device_id).then(|| self.device.clone()))
    }

    async fn list(&self) -> ahand_hub_core::Result<Vec<Device>> {
        Ok(vec![self.device.clone()])
    }

    async fn delete(&self, _device_id: &str) -> ahand_hub_core::Result<()> {
        Ok(())
    }
}

struct ErrorDeviceStore;

#[async_trait]
impl DeviceStore for ErrorDeviceStore {
    async fn insert(&self, _device: NewDevice) -> ahand_hub_core::Result<Device> {
        Err(HubError::Internal("device store unavailable".into()))
    }

    async fn get(&self, _device_id: &str) -> ahand_hub_core::Result<Option<Device>> {
        Err(HubError::Internal("device store unavailable".into()))
    }

    async fn list(&self) -> ahand_hub_core::Result<Vec<Device>> {
        Err(HubError::Internal("device store unavailable".into()))
    }

    async fn delete(&self, _device_id: &str) -> ahand_hub_core::Result<()> {
        Err(HubError::Internal("device store unavailable".into()))
    }
}

#[derive(Default)]
struct MemoryJobStore {
    jobs: DashMap<String, ahand_hub_core::job::Job>,
}

#[async_trait]
impl JobStore for MemoryJobStore {
    async fn insert(&self, job: NewJob) -> ahand_hub_core::Result<ahand_hub_core::job::Job> {
        let job =
            ahand_hub_core::job::Job::new_pending(uuid::Uuid::new_v4(), job, chrono::Utc::now());
        self.jobs.insert(job.id.to_string(), job.clone());
        Ok(job)
    }

    async fn get(&self, job_id: &str) -> ahand_hub_core::Result<Option<ahand_hub_core::job::Job>> {
        Ok(self.jobs.get(job_id).map(|job| job.clone()))
    }

    async fn list(
        &self,
        filter: JobFilter,
    ) -> ahand_hub_core::Result<Vec<ahand_hub_core::job::Job>> {
        let mut jobs = self
            .jobs
            .iter()
            .filter(|entry| {
                let job = entry.value();
                filter
                    .device_id
                    .as_ref()
                    .is_none_or(|device_id| &job.device_id == device_id)
                    && filter.status.is_none_or(|status| job.status == status)
            })
            .map(|entry| entry.value().clone())
            .collect::<Vec<_>>();
        jobs.sort_by_key(|job| job.id);
        Ok(jobs)
    }

    async fn update_status(&self, job_id: &str, status: JobStatus) -> ahand_hub_core::Result<()> {
        let mut job = self
            .jobs
            .get_mut(job_id)
            .ok_or_else(|| HubError::JobNotFound(job_id.into()))?;
        job.apply_status_transition(status, chrono::Utc::now());
        Ok(())
    }

    async fn update_terminal(
        &self,
        job_id: &str,
        exit_code: i32,
        error: &str,
        output_summary: &str,
    ) -> ahand_hub_core::Result<()> {
        let mut job = self
            .jobs
            .get_mut(job_id)
            .ok_or_else(|| HubError::JobNotFound(job_id.into()))?;
        job.record_terminal_outcome(exit_code, error.into(), output_summary.into());
        Ok(())
    }
}

#[derive(Default)]
struct RecordingAuditStore {
    entries: Mutex<Vec<AuditEntry>>,
}

#[async_trait]
impl AuditStore for RecordingAuditStore {
    async fn append(&self, entries: &[AuditEntry]) -> ahand_hub_core::Result<()> {
        self.entries
            .lock()
            .map_err(|err| HubError::Internal(err.to_string()))?
            .extend(entries.iter().cloned());
        Ok(())
    }

    async fn query(&self, filter: AuditFilter) -> ahand_hub_core::Result<Vec<AuditEntry>> {
        let entries = self
            .entries
            .lock()
            .map_err(|err| HubError::Internal(err.to_string()))?;
        Ok(entries
            .iter()
            .filter(|entry| {
                filter
                    .resource_type
                    .as_ref()
                    .is_none_or(|resource_type| &entry.resource_type == resource_type)
                    && filter
                        .resource_id
                        .as_ref()
                        .is_none_or(|resource_id| &entry.resource_id == resource_id)
                    && filter
                        .action
                        .as_ref()
                        .is_none_or(|action| &entry.action == action)
            })
            .cloned()
            .collect())
    }
}

struct FailingAuditStore;

#[async_trait]
impl AuditStore for FailingAuditStore {
    async fn append(&self, _entries: &[AuditEntry]) -> ahand_hub_core::Result<()> {
        Err(HubError::Internal("audit unavailable".into()))
    }

    async fn query(&self, _filter: AuditFilter) -> ahand_hub_core::Result<Vec<AuditEntry>> {
        Ok(vec![])
    }
}

struct FailingInsertJobStore;

#[async_trait]
impl JobStore for FailingInsertJobStore {
    async fn insert(&self, _job: NewJob) -> ahand_hub_core::Result<ahand_hub_core::job::Job> {
        Err(HubError::Internal("job insert failed".into()))
    }

    async fn get(&self, _job_id: &str) -> ahand_hub_core::Result<Option<ahand_hub_core::job::Job>> {
        Ok(None)
    }

    async fn list(
        &self,
        _filter: JobFilter,
    ) -> ahand_hub_core::Result<Vec<ahand_hub_core::job::Job>> {
        Ok(Vec::new())
    }

    async fn update_status(&self, _job_id: &str, _status: JobStatus) -> ahand_hub_core::Result<()> {
        Err(HubError::Internal("job update failed".into()))
    }

    async fn update_terminal(
        &self,
        _job_id: &str,
        _exit_code: i32,
        _error: &str,
        _output_summary: &str,
    ) -> ahand_hub_core::Result<()> {
        Err(HubError::Internal("job update failed".into()))
    }
}

struct FailingUpdateJobStore {
    job: Mutex<Option<ahand_hub_core::job::Job>>,
}

impl FailingUpdateJobStore {
    fn new(job: ahand_hub_core::job::Job) -> Self {
        Self {
            job: Mutex::new(Some(job)),
        }
    }
}

#[async_trait]
impl JobStore for FailingUpdateJobStore {
    async fn insert(&self, _job: NewJob) -> ahand_hub_core::Result<ahand_hub_core::job::Job> {
        unreachable!("insert is not used in this test")
    }

    async fn get(&self, _job_id: &str) -> ahand_hub_core::Result<Option<ahand_hub_core::job::Job>> {
        Ok(self.job.lock().unwrap().clone())
    }

    async fn list(
        &self,
        _filter: JobFilter,
    ) -> ahand_hub_core::Result<Vec<ahand_hub_core::job::Job>> {
        Ok(self.job.lock().unwrap().clone().into_iter().collect())
    }

    async fn update_status(&self, _job_id: &str, _status: JobStatus) -> ahand_hub_core::Result<()> {
        Err(HubError::Internal("job update failed".into()))
    }

    async fn update_terminal(
        &self,
        _job_id: &str,
        _exit_code: i32,
        _error: &str,
        _output_summary: &str,
    ) -> ahand_hub_core::Result<()> {
        Err(HubError::Internal("job update failed".into()))
    }
}

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
async fn create_job_returns_not_found_for_unknown_device() {
    let devices = Arc::new(OnlineDeviceStore::new("device-1"));
    let jobs = Arc::new(MemoryJobStore::default());
    let audit = Arc::new(RecordingAuditStore::default());
    let dispatcher = JobDispatcher::new(devices, jobs, audit);

    let err = dispatcher
        .create_job(NewJob {
            device_id: "device-missing".into(),
            tool: "git".into(),
            args: vec!["status".into()],
            cwd: None,
            env: HashMap::new(),
            timeout_ms: 30_000,
            requested_by: "service:test".into(),
        })
        .await
        .unwrap_err();

    assert_eq!(err, HubError::DeviceNotFound("device-missing".into()));
}

#[tokio::test]
async fn create_job_propagates_device_store_errors() {
    let jobs = Arc::new(MemoryJobStore::default());
    let audit = Arc::new(RecordingAuditStore::default());
    let dispatcher = JobDispatcher::new(Arc::new(ErrorDeviceStore), jobs, audit);

    let err = dispatcher
        .create_job(NewJob {
            device_id: "device-1".into(),
            tool: "git".into(),
            args: vec!["status".into()],
            cwd: None,
            env: HashMap::new(),
            timeout_ms: 30_000,
            requested_by: "service:test".into(),
        })
        .await
        .unwrap_err();

    assert_eq!(err, HubError::Internal("device store unavailable".into()));
}

#[tokio::test]
async fn create_job_propagates_job_insert_errors() {
    let devices = Arc::new(OnlineDeviceStore::new("device-1"));
    let audit = Arc::new(RecordingAuditStore::default());
    let dispatcher = JobDispatcher::new(devices, Arc::new(FailingInsertJobStore), audit);

    let err = dispatcher
        .create_job(NewJob {
            device_id: "device-1".into(),
            tool: "git".into(),
            args: vec!["status".into()],
            cwd: None,
            env: HashMap::new(),
            timeout_ms: 30_000,
            requested_by: "service:test".into(),
        })
        .await
        .unwrap_err();

    assert_eq!(err, HubError::Internal("job insert failed".into()));
}

#[tokio::test]
async fn create_job_writes_audit_entry_for_online_device() {
    let devices = Arc::new(OnlineDeviceStore::new("device-1"));
    let jobs = Arc::new(MemoryJobStore::default());
    let audit = Arc::new(RecordingAuditStore::default());
    let dispatcher = JobDispatcher::new(devices, jobs.clone(), audit.clone());

    let job = dispatcher
        .create_job(NewJob {
            device_id: "device-1".into(),
            tool: "git".into(),
            args: vec!["status".into()],
            cwd: Some("/tmp/demo".into()),
            env: HashMap::from([("RUST_LOG".into(), "info".into())]),
            timeout_ms: 30_000,
            requested_by: "service:test".into(),
        })
        .await
        .unwrap();

    let stored = jobs.get(&job.id.to_string()).await.unwrap().unwrap();
    let audit_entries = audit
        .query(AuditFilter {
            resource_type: Some("job".into()),
            resource_id: Some(job.id.to_string()),
            action: Some("job.created".into()),
        })
        .await
        .unwrap();

    assert_eq!(stored.id, job.id);
    assert_eq!(stored.tool, "git");
    assert_eq!(audit_entries.len(), 1);
    assert_eq!(audit_entries[0].actor, "service:test");
    assert_eq!(
        audit_entries[0].detail,
        serde_json::json!({ "tool": "git" })
    );
}

#[tokio::test]
async fn create_job_does_not_fail_after_job_is_persisted_if_audit_write_fails() {
    let devices = Arc::new(OnlineDeviceStore::new("device-1"));
    let jobs = Arc::new(MemoryJobStore::default());
    let audit = Arc::new(FailingAuditStore);
    let dispatcher = JobDispatcher::new(devices, jobs.clone(), audit);

    let job = dispatcher
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
        .unwrap();

    let stored = jobs.get(&job.id.to_string()).await.unwrap().unwrap();
    let all_jobs = jobs.list(JobFilter::default()).await.unwrap();

    assert_eq!(stored.id, job.id);
    assert_eq!(all_jobs.len(), 1);
}

#[tokio::test]
async fn transition_returns_error_for_terminal_job_regressions() {
    let devices = Arc::new(OnlineDeviceStore::new("device-1"));
    let jobs = Arc::new(MemoryJobStore::default());
    let audit = Arc::new(RecordingAuditStore::default());
    let dispatcher = JobDispatcher::new(devices, jobs.clone(), audit);

    let job = dispatcher
        .create_job(NewJob {
            device_id: "device-1".into(),
            tool: "git".into(),
            args: vec!["status".into()],
            cwd: None,
            env: HashMap::new(),
            timeout_ms: 30_000,
            requested_by: "service:test".into(),
        })
        .await
        .unwrap();

    dispatcher
        .transition(&job.id.to_string(), JobStatus::Finished)
        .await
        .unwrap();

    let err = dispatcher
        .transition(&job.id.to_string(), JobStatus::Running)
        .await
        .unwrap_err();

    assert_eq!(
        err,
        HubError::IllegalJobTransition {
            current: JobStatus::Finished,
            requested: JobStatus::Running,
        }
    );

    let stored = jobs.get(&job.id.to_string()).await.unwrap().unwrap();
    assert_eq!(stored.status, JobStatus::Finished);
}

#[tokio::test]
async fn transition_returns_none_when_status_is_unchanged() {
    let devices = Arc::new(OnlineDeviceStore::new("device-1"));
    let jobs = Arc::new(MemoryJobStore::default());
    let audit = Arc::new(RecordingAuditStore::default());
    let dispatcher = JobDispatcher::new(devices, jobs.clone(), audit);

    let job = dispatcher
        .create_job(NewJob {
            device_id: "device-1".into(),
            tool: "git".into(),
            args: vec!["status".into()],
            cwd: None,
            env: HashMap::new(),
            timeout_ms: 30_000,
            requested_by: "service:test".into(),
        })
        .await
        .unwrap();

    let transitioned = dispatcher
        .transition(&job.id.to_string(), JobStatus::Pending)
        .await
        .unwrap();

    assert_eq!(transitioned, None);
    assert_eq!(
        jobs.get(&job.id.to_string()).await.unwrap().unwrap().status,
        JobStatus::Pending
    );
}

#[tokio::test]
async fn transition_returns_explicit_error_for_illegal_job_transitions() {
    let devices = Arc::new(OnlineDeviceStore::new("device-1"));
    let jobs = Arc::new(MemoryJobStore::default());
    let audit = Arc::new(RecordingAuditStore::default());
    let dispatcher = JobDispatcher::new(devices, jobs.clone(), audit);

    let job = dispatcher
        .create_job(NewJob {
            device_id: "device-1".into(),
            tool: "git".into(),
            args: vec!["status".into()],
            cwd: None,
            env: HashMap::new(),
            timeout_ms: 30_000,
            requested_by: "service:test".into(),
        })
        .await
        .unwrap();

    jobs.update_status(&job.id.to_string(), JobStatus::Sent)
        .await
        .unwrap();

    let err = dispatcher
        .transition(&job.id.to_string(), JobStatus::Pending)
        .await
        .unwrap_err();

    assert_eq!(
        err,
        HubError::IllegalJobTransition {
            current: JobStatus::Sent,
            requested: JobStatus::Pending,
        }
    );
    assert_eq!(
        jobs.get(&job.id.to_string()).await.unwrap().unwrap().status,
        JobStatus::Sent
    );
}

#[tokio::test]
async fn transition_returns_not_found_for_missing_job() {
    let devices = Arc::new(OnlineDeviceStore::new("device-1"));
    let jobs = Arc::new(MemoryJobStore::default());
    let audit = Arc::new(RecordingAuditStore::default());
    let dispatcher = JobDispatcher::new(devices, jobs, audit);

    let err = dispatcher
        .transition("missing-job", JobStatus::Running)
        .await
        .unwrap_err();

    assert_eq!(err, HubError::JobNotFound("missing-job".into()));
}

#[tokio::test]
async fn transition_propagates_job_store_update_errors() {
    let devices = Arc::new(OnlineDeviceStore::new("device-1"));
    let audit = Arc::new(RecordingAuditStore::default());
    let job = ahand_hub_core::job::Job {
        id: uuid::Uuid::new_v4(),
        device_id: "device-1".into(),
        tool: "git".into(),
        args: vec!["status".into()],
        cwd: None,
        env: HashMap::new(),
        timeout_ms: 30_000,
        status: JobStatus::Pending,
        exit_code: None,
        error: None,
        output_summary: None,
        requested_by: "service:test".into(),
        created_at: chrono::Utc::now(),
        started_at: None,
        finished_at: None,
    };
    let dispatcher = JobDispatcher::new(
        devices,
        Arc::new(FailingUpdateJobStore::new(job.clone())),
        audit,
    );

    let err = dispatcher
        .transition(&job.id.to_string(), JobStatus::Running)
        .await
        .unwrap_err();

    assert_eq!(err, HubError::Internal("job update failed".into()));
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
