use std::sync::Arc;

use ahand_hub_core::audit::AuditEntry;
use ahand_hub_core::job::Job;
use ahand_hub_core::traits::AuditStore;
use ahand_hub_store::event_fanout::RedisEventFanout;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    fanout: Option<RedisEventFanout>,
    origin_id: Arc<String>,
}

impl EventBus {
    pub fn new(audit: Arc<dyn AuditStore>) -> Self {
        let (tx, _) = broadcast::channel(256);
        Self {
            audit,
            tx,
            fanout: None,
            origin_id: Arc::new(uuid::Uuid::new_v4().to_string()),
        }
    }

    pub fn new_with_fanout(audit: Arc<dyn AuditStore>, fanout: RedisEventFanout) -> Self {
        let (tx, _) = broadcast::channel(256);
        let bus = Self {
            audit,
            tx,
            fanout: Some(fanout),
            origin_id: Arc::new(uuid::Uuid::new_v4().to_string()),
        };
        bus.spawn_fanout_relay();
        bus
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

    pub async fn publish_job_created(&self, job: &Job) -> anyhow::Result<()> {
        let event = DashboardEvent {
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
        };
        self.publish(event).await
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
        let _ = self
            .audit
            .append(&[AuditEntry {
                timestamp,
                action: action.into(),
                resource_type: resource_type.into(),
                resource_id: resource_id.into(),
                actor: actor.into(),
                detail: detail.clone(),
                source_ip: None,
            }])
            .await;
        self.publish(DashboardEvent {
            event: action.into(),
            resource_type: resource_type.into(),
            resource_id: resource_id.into(),
            actor: actor.into(),
            detail,
            timestamp,
        })
        .await?;
        Ok(())
    }

    async fn publish(&self, event: DashboardEvent) -> anyhow::Result<()> {
        let _ = self.tx.send(event.clone());
        if let Some(fanout) = &self.fanout {
            let envelope = FanoutEnvelope {
                origin_id: self.origin_id.as_ref().clone(),
                event,
            };
            let payload = serde_json::to_string(&envelope)?;
            fanout.publish_json(&payload).await?;
        }
        Ok(())
    }

    fn spawn_fanout_relay(&self) {
        let Some(fanout) = self.fanout.clone() else {
            return;
        };
        let tx = self.tx.clone();
        let origin_id = self.origin_id.clone();

        tokio::spawn(async move {
            loop {
                let mut pubsub = match fanout.subscribe().await {
                    Ok(pubsub) => pubsub,
                    Err(err) => {
                        tracing::warn!(error = %err, "failed to subscribe to dashboard event fanout");
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        continue;
                    }
                };
                let mut messages = pubsub.on_message();
                while let Some(message) = messages.next().await {
                    let payload = match message.get_payload::<String>() {
                        Ok(payload) => payload,
                        Err(err) => {
                            tracing::warn!(error = %err, "failed to decode dashboard event fanout payload");
                            continue;
                        }
                    };
                    let envelope = match serde_json::from_str::<FanoutEnvelope>(&payload) {
                        Ok(envelope) => envelope,
                        Err(err) => {
                            tracing::warn!(error = %err, "failed to parse dashboard event fanout payload");
                            continue;
                        }
                    };
                    if envelope.origin_id == *origin_id {
                        continue;
                    }
                    let _ = tx.send(envelope.event);
                }
            }
        });
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FanoutEnvelope {
    origin_id: String,
    event: DashboardEvent,
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
