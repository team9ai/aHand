use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::Result;
use crate::audit::{AuditEntry, AuditFilter};
use crate::device::{Device, NewDevice};
use crate::job::{Job, JobFilter, JobStatus, NewJob};

#[async_trait]
pub trait DeviceStore: Send + Sync {
    async fn insert(&self, device: NewDevice) -> Result<Device>;
    async fn get(&self, device_id: &str) -> Result<Option<Device>>;
    async fn list(&self) -> Result<Vec<Device>>;
    async fn delete(&self, device_id: &str) -> Result<()>;
}

#[async_trait]
pub trait JobStore: Send + Sync {
    async fn insert(&self, job: NewJob) -> Result<Job>;
    async fn get(&self, job_id: &str) -> Result<Option<Job>>;
    async fn list(&self, filter: JobFilter) -> Result<Vec<Job>>;
    async fn transition_status(&self, job_id: &str, status: JobStatus)
    -> Result<Option<JobStatus>>;
    async fn update_status(&self, job_id: &str, status: JobStatus) -> Result<()>;
    async fn update_terminal(
        &self,
        job_id: &str,
        exit_code: i32,
        error: &str,
        output_summary: &str,
    ) -> Result<()>;
}

#[async_trait]
pub trait AuditStore: Send + Sync {
    async fn append(&self, entries: &[AuditEntry]) -> Result<()>;
    async fn query(&self, filter: AuditFilter) -> Result<Vec<AuditEntry>>;

    async fn prune_before(&self, _cutoff: DateTime<Utc>) -> Result<u64> {
        Ok(0)
    }
}
