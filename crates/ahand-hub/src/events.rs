use std::sync::Arc;

use ahand_hub_core::audit::AuditEntry;
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
}
