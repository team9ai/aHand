use std::sync::{Arc, Mutex};

use ahand_hub_core::HubError;
use ahand_hub_core::audit::{AuditEntry, AuditFilter};
use ahand_hub_core::services::audit_service::AuditService;
use ahand_hub_core::traits::AuditStore;
use async_trait::async_trait;

#[derive(Default)]
struct RecordingAuditStore {
    entries: Mutex<Vec<AuditEntry>>,
}

#[async_trait]
impl AuditStore for RecordingAuditStore {
    async fn append(&self, entries: &[AuditEntry]) -> ahand_hub_core::Result<()> {
        self.entries
            .lock()
            .map_err(|err| HubError::Internal(err.to_string()))?
            .extend(entries.iter().cloned());
        Ok(())
    }

    async fn query(&self, filter: AuditFilter) -> ahand_hub_core::Result<Vec<AuditEntry>> {
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

#[tokio::test]
async fn audit_service_appends_and_queries_entries() {
    let store = Arc::new(RecordingAuditStore::default());
    let service = AuditService::new(store);
    let entry = AuditEntry {
        timestamp: chrono::Utc::now(),
        action: "job.created".into(),
        resource_type: "job".into(),
        resource_id: "job-1".into(),
        actor: "service:test".into(),
        detail: serde_json::json!({ "tool": "git" }),
        source_ip: Some("127.0.0.1".into()),
    };

    service.append(entry.clone()).await.unwrap();

    let results = service
        .query(AuditFilter {
            resource_type: Some("job".into()),
            resource_id: Some("job-1".into()),
            action: Some("job.created".into()),
        })
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].action, entry.action);
    assert_eq!(results[0].resource_type, entry.resource_type);
    assert_eq!(results[0].resource_id, entry.resource_id);
    assert_eq!(results[0].actor, entry.actor);
    assert_eq!(results[0].detail, entry.detail);
    assert_eq!(results[0].source_ip, entry.source_ip);
}
