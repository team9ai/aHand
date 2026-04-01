use std::sync::Arc;

use ahand_hub_core::audit::AuditEntry;
use ahand_hub_core::job::Job;
use ahand_hub_core::traits::AuditStore;

pub struct EventBus {
    audit: Arc<dyn AuditStore>,
}

impl EventBus {
    pub fn new(audit: Arc<dyn AuditStore>) -> Self {
        Self { audit }
    }

    pub async fn emit_device_online(&self, device_id: &str, hostname: &str) -> anyhow::Result<()> {
        self.audit
            .append(&[AuditEntry {
                timestamp: chrono::Utc::now(),
                action: "device.online".into(),
                resource_type: "device".into(),
                resource_id: device_id.into(),
                actor: "device".into(),
                detail: serde_json::json!({ "hostname": hostname }),
                source_ip: None,
            }])
            .await?;
        Ok(())
    }

    pub async fn emit_device_offline(&self, device_id: &str) -> anyhow::Result<()> {
        self.audit
            .append(&[AuditEntry {
                timestamp: chrono::Utc::now(),
                action: "device.offline".into(),
                resource_type: "device".into(),
                resource_id: device_id.into(),
                actor: "device".into(),
                detail: serde_json::json!({}),
                source_ip: None,
            }])
            .await?;
        Ok(())
    }

    pub async fn emit_job_status(&self, job: &Job, actor: &str) -> anyhow::Result<()> {
        let Some(action) = job_status_action(job) else {
            return Ok(());
        };

        self.audit
            .append(&[AuditEntry {
                timestamp: chrono::Utc::now(),
                action: action.into(),
                resource_type: "job".into(),
                resource_id: job.id.to_string(),
                actor: actor.into(),
                detail: serde_json::json!({
                    "device_id": job.device_id,
                    "tool": job.tool,
                    "status": job_status_name(job),
                }),
                source_ip: None,
            }])
            .await?;
        Ok(())
    }
}

fn job_status_action(job: &Job) -> Option<&'static str> {
    match job.status {
        ahand_hub_core::job::JobStatus::Running => Some("job.running"),
        ahand_hub_core::job::JobStatus::Finished => Some("job.finished"),
        ahand_hub_core::job::JobStatus::Failed => Some("job.failed"),
        ahand_hub_core::job::JobStatus::Cancelled => Some("job.cancelled"),
        _ => None,
    }
}

fn job_status_name(job: &Job) -> &'static str {
    match job.status {
        ahand_hub_core::job::JobStatus::Pending => "pending",
        ahand_hub_core::job::JobStatus::Sent => "sent",
        ahand_hub_core::job::JobStatus::Running => "running",
        ahand_hub_core::job::JobStatus::Finished => "finished",
        ahand_hub_core::job::JobStatus::Failed => "failed",
        ahand_hub_core::job::JobStatus::Cancelled => "cancelled",
    }
}
