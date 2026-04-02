use std::sync::Arc;

use crate::traits::{AuditStore, DeviceStore, JobStore};

pub struct FakeStores {
    pub devices: Arc<dyn DeviceStore>,
    pub jobs: Arc<dyn JobStore>,
    pub audit: Arc<dyn AuditStore>,
}

pub mod fakes {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use dashmap::DashMap;

    use crate::audit::{AuditEntry, AuditFilter};
    use crate::device::{Device, NewDevice};
    use crate::job::{Job, JobFilter, JobStatus, NewJob};
    use crate::traits::{AuditStore, DeviceStore, JobStore};
    use crate::{HubError, Result};

    pub fn offline_job_stores() -> super::FakeStores {
        super::FakeStores {
            devices: Arc::new(OfflineDeviceStore),
            jobs: Arc::new(MemoryJobStore::default()),
            audit: Arc::new(MemoryAuditStore::default()),
        }
    }

    struct OfflineDeviceStore;

    #[async_trait]
    impl DeviceStore for OfflineDeviceStore {
        async fn insert(&self, _device: NewDevice) -> Result<Device> {
            Err(HubError::Internal("not needed in this test".into()))
        }

        async fn get(&self, _device_id: &str) -> Result<Option<Device>> {
            Ok(Some(Device::offline_for_tests("device-1")))
        }

        async fn list(&self) -> Result<Vec<Device>> {
            Ok(vec![Device::offline_for_tests("device-1")])
        }

        async fn delete(&self, _device_id: &str) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct MemoryJobStore {
        jobs: DashMap<String, Job>,
    }

    #[async_trait]
    impl JobStore for MemoryJobStore {
        async fn insert(&self, job: NewJob) -> Result<Job> {
            let job = Job::new_pending(uuid::Uuid::new_v4(), job, chrono::Utc::now());
            self.jobs.insert(job.id.to_string(), job.clone());
            Ok(job)
        }

        async fn get(&self, job_id: &str) -> Result<Option<Job>> {
            Ok(self.jobs.get(job_id).map(|job| job.clone()))
        }

        async fn list(&self, filter: JobFilter) -> Result<Vec<Job>> {
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

        async fn update_status(&self, job_id: &str, status: JobStatus) -> Result<()> {
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
        ) -> Result<()> {
            let mut job = self
                .jobs
                .get_mut(job_id)
                .ok_or_else(|| HubError::JobNotFound(job_id.into()))?;
            job.record_terminal_outcome(exit_code, error.into(), output_summary.into());
            Ok(())
        }
    }

    #[derive(Default)]
    struct MemoryAuditStore {
        entries: Mutex<Vec<AuditEntry>>,
    }

    #[async_trait]
    impl AuditStore for MemoryAuditStore {
        async fn append(&self, entries: &[AuditEntry]) -> Result<()> {
            self.entries
                .lock()
                .map_err(|err| HubError::Internal(err.to_string()))?
                .extend(entries.iter().cloned());
            Ok(())
        }

        async fn query(&self, filter: AuditFilter) -> Result<Vec<AuditEntry>> {
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
}
