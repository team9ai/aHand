use std::sync::Arc;

use crate::traits::{AuditStore, DeviceStore, JobStore};

pub struct FakeStores {
    pub devices: Arc<dyn DeviceStore>,
    pub jobs: Arc<dyn JobStore>,
    pub audit: Arc<dyn AuditStore>,
}

pub mod fakes {
    use std::sync::Arc;

    use async_trait::async_trait;

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
    struct MemoryJobStore;

    #[async_trait]
    impl JobStore for MemoryJobStore {
        async fn insert(&self, _job: NewJob) -> Result<Job> {
            Err(HubError::Internal("not needed in this test".into()))
        }

        async fn get(&self, _job_id: &str) -> Result<Option<Job>> {
            Ok(None)
        }

        async fn list(&self, _filter: JobFilter) -> Result<Vec<Job>> {
            Ok(vec![])
        }

        async fn update_status(&self, _job_id: &str, _status: JobStatus) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct MemoryAuditStore;

    #[async_trait]
    impl AuditStore for MemoryAuditStore {
        async fn append(&self, _entries: &[AuditEntry]) -> Result<()> {
            Ok(())
        }

        async fn query(&self, _filter: AuditFilter) -> Result<Vec<AuditEntry>> {
            Ok(vec![])
        }
    }
}
