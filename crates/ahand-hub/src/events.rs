use std::sync::Arc;

use ahand_hub_core::audit::AuditEntry;
use ahand_hub_core::job::Job;
use ahand_hub_core::traits::AuditStore;
use ahand_hub_store::event_fanout::RedisEventFanout;
use async_trait::async_trait;
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
    publisher: Option<Arc<dyn EventPublisher>>,
    origin_id: Arc<String>,
}

#[async_trait]
trait EventPublisher: Send + Sync {
    async fn publish_json(&self, payload: &str) -> anyhow::Result<()>;
}

#[async_trait]
impl EventPublisher for RedisEventFanout {
    async fn publish_json(&self, payload: &str) -> anyhow::Result<()> {
        RedisEventFanout::publish_json(self, payload)
            .await
            .map_err(anyhow::Error::from)
    }
}

impl EventBus {
    pub fn new(audit: Arc<dyn AuditStore>) -> Self {
        let (tx, _) = broadcast::channel(256);
        Self {
            audit,
            tx,
            fanout: None,
            publisher: None,
            origin_id: Arc::new(uuid::Uuid::new_v4().to_string()),
        }
    }

    pub fn new_with_fanout(audit: Arc<dyn AuditStore>, fanout: RedisEventFanout) -> Self {
        let (tx, _) = broadcast::channel(256);
        let bus = Self {
            audit,
            tx,
            fanout: Some(fanout.clone()),
            publisher: Some(Arc::new(fanout)),
            origin_id: Arc::new(uuid::Uuid::new_v4().to_string()),
        };
        bus.spawn_fanout_relay();
        bus
    }

    #[cfg(test)]
    fn new_with_publisher_for_tests(
        audit: Arc<dyn AuditStore>,
        publisher: Arc<dyn EventPublisher>,
    ) -> Self {
        let (tx, _) = broadcast::channel(256);
        Self {
            audit,
            tx,
            fanout: None,
            publisher: Some(publisher),
            origin_id: Arc::new(uuid::Uuid::new_v4().to_string()),
        }
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
        if let Some(publisher) = &self.publisher {
            let envelope = FanoutEnvelope {
                origin_id: self.origin_id.as_ref().clone(),
                event,
            };
            let payload = serde_json::to_string(&envelope)?;
            if let Err(err) = publisher.publish_json(&payload).await {
                tracing::warn!(error = %err, "failed to publish dashboard event fanout");
            }
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use ahand_hub_core::audit::{AuditEntry, AuditFilter};
    use ahand_hub_core::job::{Job, JobStatus};
    use ahand_hub_core::traits::AuditStore;
    use async_trait::async_trait;
    use chrono::Utc;

    use super::EventBus;

    #[derive(Default)]
    struct NoopAuditStore;

    #[async_trait]
    impl AuditStore for NoopAuditStore {
        async fn append(&self, _entries: &[AuditEntry]) -> ahand_hub_core::Result<()> {
            Ok(())
        }

        async fn query(&self, _filter: AuditFilter) -> ahand_hub_core::Result<Vec<AuditEntry>> {
            Ok(Vec::new())
        }
    }

    #[derive(Default)]
    struct FailingPublisher {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl super::EventPublisher for FailingPublisher {
        async fn publish_json(&self, _payload: &str) -> anyhow::Result<()> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            anyhow::bail!("fanout unavailable");
        }
    }

    #[tokio::test]
    async fn publish_job_created_ignores_fanout_failures() {
        let publisher = Arc::new(FailingPublisher::default());
        let bus = EventBus::new_with_publisher_for_tests(
            Arc::new(NoopAuditStore),
            publisher.clone(),
        );
        let mut rx = bus.subscribe();
        let job = Job {
            id: uuid::Uuid::new_v4(),
            device_id: "device-1".into(),
            tool: "echo".into(),
            args: vec!["hello".into()],
            cwd: None,
            env: Default::default(),
            timeout_ms: 30_000,
            status: JobStatus::Pending,
            exit_code: None,
            error: None,
            output_summary: None,
            requested_by: "service".into(),
            created_at: Utc::now(),
            started_at: None,
            finished_at: None,
        };

        bus.publish_job_created(&job).await.unwrap();

        let event = tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("subscriber should receive local event")
            .expect("event bus should stay open");
        assert_eq!(event.event, "job.created");
        assert_eq!(publisher.calls.load(Ordering::Relaxed), 1);
    }
}
