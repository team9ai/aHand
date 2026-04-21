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

/// Admin-plane operations on a device store. Implemented by the same
/// backends that implement [`DeviceStore`], but split out into its own
/// trait so callers that only need read/write ops (e.g. the dispatcher)
/// don't need to know about the admin surface. The trait is intentionally
/// additive — existing [`DeviceStore`] consumers are untouched.
#[async_trait]
pub trait DeviceAdminStore: Send + Sync {
    /// Idempotent pre-register:
    /// - if no row exists, insert `(device_id, public_key, external_user_id)`
    /// - if a row exists with matching `external_user_id` AND matching
    ///   `public_key`, return the existing row unchanged
    /// - if a row exists with a different `external_user_id`, return
    ///   [`crate::HubError::DeviceOwnedByDifferentUser`]
    async fn pre_register(
        &self,
        device_id: &str,
        public_key: &[u8],
        external_user_id: &str,
    ) -> Result<Device>;

    async fn find_by_id(&self, device_id: &str) -> Result<Option<Device>>;

    /// Delete returns true if a row was removed, false if it didn't
    /// exist. Distinguishes "idempotent no-op" from "something changed"
    /// for the admin API's 404 path.
    async fn delete_device(&self, device_id: &str) -> Result<bool>;

    async fn list_by_external_user(&self, external_user_id: &str) -> Result<Vec<Device>>;
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
