use std::sync::Arc;

use crate::audit::AuditEntry;
use crate::job::{Job, JobStatus, NewJob};
use crate::traits::{AuditStore, DeviceStore, JobStore};
use crate::{HubError, Result};

pub struct JobDispatcher {
    devices: Arc<dyn DeviceStore>,
    jobs: Arc<dyn JobStore>,
    audit: Arc<dyn AuditStore>,
}

impl JobDispatcher {
    pub fn new(
        devices: Arc<dyn DeviceStore>,
        jobs: Arc<dyn JobStore>,
        audit: Arc<dyn AuditStore>,
    ) -> Self {
        Self {
            devices,
            jobs,
            audit,
        }
    }

    pub async fn create_job(&self, new_job: NewJob) -> Result<Job> {
        let Some(device) = self.devices.get(&new_job.device_id).await? else {
            return Err(HubError::DeviceNotFound(new_job.device_id));
        };
        if !device.online {
            return Err(HubError::DeviceOffline(device.id));
        }

        let job = self.jobs.insert(new_job).await?;
        self.audit
            .append(&[AuditEntry {
                timestamp: chrono::Utc::now(),
                action: "job.created".into(),
                resource_type: "job".into(),
                resource_id: job.id.to_string(),
                actor: job.requested_by.clone(),
                detail: serde_json::json!({ "tool": job.tool }),
                source_ip: None,
            }])
            .await?;
        Ok(job)
    }

    pub async fn transition(&self, job_id: &str, status: JobStatus) -> Result<()> {
        self.jobs.update_status(job_id, status).await
    }
}
