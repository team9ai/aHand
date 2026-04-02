use std::sync::Arc;

use ahand_hub_core::audit::AuditEntry;
use ahand_hub_core::job::Job;
use ahand_hub_core::traits::AuditStore;
use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::sync::broadcast;

#[derive(Debug, Clone, Serialize)]
pub struct DashboardEvent {
    pub event: String,
    pub resource_type: String,
    pub resource_id: String,
    pub actor: String,
    pub detail: serde_json::Value,
    pub timestamp: DateTime<Utc>,
}

pub struct EventBus {
    audit: Arc<dyn AuditStore>,
    tx: broadcast::Sender<DashboardEvent>,
}

impl EventBus {
    pub fn new(audit: Arc<dyn AuditStore>) -> Self {
        let (tx, _) = broadcast::channel(256);
        Self { audit, tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<DashboardEvent> {
        self.tx.subscribe()
    }

    pub async fn emit_device_online(&self, device_id: &str, hostname: &str) -> anyhow::Result<()> {
        self.record_and_publish(
            "device.online",
            "device",
            device_id,
            "device",
            serde_json::json!({ "hostname": hostname }),
        )
        .await?;
        Ok(())
    }

    pub async fn emit_device_offline(&self, device_id: &str) -> anyhow::Result<()> {
        self.record_and_publish(
            "device.offline",
            "device",
            device_id,
            "device",
            serde_json::json!({}),
        )
        .await?;
        Ok(())
    }

    pub async fn emit_job_status(&self, job: &Job, actor: &str) -> anyhow::Result<()> {
        let Some(action) = job_status_action(job) else {
            return Ok(());
        };

        self.record_and_publish(
            action,
            "job",
            &job.id.to_string(),
            actor,
            serde_json::json!({
                "device_id": job.device_id,
                "tool": job.tool,
                "status": job_status_name(job),
            }),
        )
        .await?;
        Ok(())
    }

    pub fn publish_job_created(&self, job: &Job) {
        let audit = self.audit.clone();
        let audit_job = job.clone();
        tokio::spawn(async move {
            let _ = audit
                .append(&[AuditEntry {
                    timestamp: Utc::now(),
                    action: "job.created".into(),
                    resource_type: "job".into(),
                    resource_id: audit_job.id.to_string(),
                    actor: audit_job.requested_by.clone(),
                    detail: serde_json::json!({
                        "device_id": audit_job.device_id,
                        "tool": audit_job.tool,
                        "status": job_status_name(&audit_job),
                    }),
                    source_ip: None,
                }])
                .await;
        });
        self.publish(DashboardEvent {
            event: "job.created".into(),
            resource_type: "job".into(),
            resource_id: job.id.to_string(),
            actor: job.requested_by.clone(),
            detail: serde_json::json!({
                "device_id": job.device_id,
                "tool": job.tool,
                "status": job_status_name(job),
            }),
            timestamp: Utc::now(),
        });
    }

    async fn record_and_publish(
        &self,
        action: &str,
        resource_type: &str,
        resource_id: &str,
        actor: &str,
        detail: serde_json::Value,
    ) -> anyhow::Result<()> {
        let timestamp = Utc::now();
        let _ = self.audit.append(&[AuditEntry {
            timestamp,
            action: action.into(),
            resource_type: resource_type.into(),
            resource_id: resource_id.into(),
            actor: actor.into(),
            detail: detail.clone(),
            source_ip: None,
        }]).await;
        self.publish(DashboardEvent {
            event: action.into(),
            resource_type: resource_type.into(),
            resource_id: resource_id.into(),
            actor: actor.into(),
            detail,
            timestamp,
        });
        Ok(())
    }

    fn publish(&self, event: DashboardEvent) {
        let _ = self.tx.send(event);
    }
}

fn job_status_action(job: &Job) -> Option<&'static str> {
    match job.status {
        ahand_hub_core::job::JobStatus::Sent => Some("job.sent"),
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
