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
        Ok(filter.apply(entries.iter().cloned()))
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
            ..Default::default()
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

#[test]
fn audit_filter_matches_resource_aliases_and_sorts_descending_with_pagination() {
    use chrono::{Duration, TimeZone};

    let base = chrono::Utc.with_ymd_and_hms(2026, 4, 3, 10, 0, 0).unwrap();
    let entries = vec![
        AuditEntry {
            timestamp: base,
            action: "job.finished".into(),
            resource_type: "job".into(),
            resource_id: "job-2".into(),
            actor: "service-b".into(),
            detail: serde_json::json!({ "ordinal": 2 }),
            source_ip: None,
        },
        AuditEntry {
            timestamp: base,
            action: "job.created".into(),
            resource_type: "job".into(),
            resource_id: "job-1".into(),
            actor: "service-a".into(),
            detail: serde_json::json!({ "ordinal": 1 }),
            source_ip: None,
        },
        AuditEntry {
            timestamp: base + Duration::seconds(5),
            action: "device.online".into(),
            resource_type: "device".into(),
            resource_id: "device-1".into(),
            actor: "service-c".into(),
            detail: serde_json::json!({ "ordinal": 3 }),
            source_ip: None,
        },
    ];

    let filter = AuditFilter {
        resource: Some("job".into()),
        since: Some(base),
        until: Some(base + Duration::seconds(5)),
        descending: true,
        limit: Some(1),
        offset: Some(1),
        ..Default::default()
    };

    let filtered = filter.apply(entries);

    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].resource_type, "job");
    assert_eq!(filtered[0].resource_id, "job-1");
    let cloned_filter = filter.clone();
    assert!(format!("{cloned_filter:?}").contains("descending"));

    let id_filter = AuditFilter {
        resource: Some("device-1".into()),
        ..Default::default()
    };
    assert!(id_filter.matches(&AuditEntry {
        timestamp: base,
        action: "device.online".into(),
        resource_type: "device".into(),
        resource_id: "device-1".into(),
        actor: "service-c".into(),
        detail: serde_json::json!({}),
        source_ip: None,
    }));

    assert!(
        !AuditFilter {
            resource_type: Some("job".into()),
            ..Default::default()
        }
        .matches(&AuditEntry {
            timestamp: base,
            action: "device.online".into(),
            resource_type: "device".into(),
            resource_id: "device-1".into(),
            actor: "service-c".into(),
            detail: serde_json::json!({}),
            source_ip: None,
        })
    );
    assert!(
        !AuditFilter {
            resource_id: Some("job-3".into()),
            ..Default::default()
        }
        .matches(&AuditEntry {
            timestamp: base,
            action: "job.finished".into(),
            resource_type: "job".into(),
            resource_id: "job-2".into(),
            actor: "service-b".into(),
            detail: serde_json::json!({}),
            source_ip: None,
        })
    );
    assert!(
        !AuditFilter {
            action: Some("job.failed".into()),
            ..Default::default()
        }
        .matches(&AuditEntry {
            timestamp: base,
            action: "job.finished".into(),
            resource_type: "job".into(),
            resource_id: "job-2".into(),
            actor: "service-b".into(),
            detail: serde_json::json!({}),
            source_ip: None,
        })
    );
    assert!(
        !AuditFilter {
            since: Some(base + Duration::seconds(1)),
            ..Default::default()
        }
        .matches(&AuditEntry {
            timestamp: base,
            action: "job.finished".into(),
            resource_type: "job".into(),
            resource_id: "job-2".into(),
            actor: "service-b".into(),
            detail: serde_json::json!({}),
            source_ip: None,
        })
    );
    assert!(
        !AuditFilter {
            until: Some(base - Duration::seconds(1)),
            ..Default::default()
        }
        .matches(&AuditEntry {
            timestamp: base,
            action: "job.finished".into(),
            resource_type: "job".into(),
            resource_id: "job-2".into(),
            actor: "service-b".into(),
            detail: serde_json::json!({}),
            source_ip: None,
        })
    );

    let tie_broken = AuditFilter {
        resource_type: Some("job".into()),
        ..Default::default()
    }
    .apply(vec![
        AuditEntry {
            timestamp: base,
            action: "job.finished".into(),
            resource_type: "job".into(),
            resource_id: "job-1".into(),
            actor: "service-b".into(),
            detail: serde_json::json!({}),
            source_ip: None,
        },
        AuditEntry {
            timestamp: base,
            action: "job.finished".into(),
            resource_type: "job".into(),
            resource_id: "job-1".into(),
            actor: "service-a".into(),
            detail: serde_json::json!({}),
            source_ip: None,
        },
        AuditEntry {
            timestamp: base,
            action: "job.finished".into(),
            resource_type: "job".into(),
            resource_id: "job-0".into(),
            actor: "service-z".into(),
            detail: serde_json::json!({}),
            source_ip: None,
        },
    ]);
    assert_eq!(
        tie_broken
            .iter()
            .map(|entry| (entry.resource_id.as_str(), entry.actor.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("job-0", "service-z"),
            ("job-1", "service-a"),
            ("job-1", "service-b")
        ]
    );
}

#[tokio::test]
async fn audit_store_default_prune_before_returns_zero() {
    let store = RecordingAuditStore::default();

    let pruned = store.prune_before(chrono::Utc::now()).await.unwrap();

    assert_eq!(pruned, 0);
}

#[test]
fn audit_entry_clone_debug_and_serde_roundtrip() {
    let entry = AuditEntry {
        timestamp: chrono::Utc::now(),
        action: "job.finished".into(),
        resource_type: "job".into(),
        resource_id: "job-9".into(),
        actor: "service:test".into(),
        detail: serde_json::json!({ "ok": true }),
        source_ip: Some("127.0.0.1".into()),
    };

    let cloned = entry.clone();
    let debug = format!("{cloned:?}");
    let json = serde_json::to_string(&cloned).unwrap();
    let roundtrip: AuditEntry = serde_json::from_str(&json).unwrap();

    assert!(debug.contains("job.finished"));
    assert_eq!(roundtrip.action, entry.action);
    assert_eq!(roundtrip.resource_type, entry.resource_type);
    assert_eq!(roundtrip.resource_id, entry.resource_id);
    assert_eq!(roundtrip.actor, entry.actor);
    assert_eq!(roundtrip.detail, entry.detail);
    assert_eq!(roundtrip.source_ip, entry.source_ip);
}

#[test]
fn default_filter_matches_all_entries() {
    let base = chrono::Utc::now();
    let entries = vec![
        AuditEntry {
            timestamp: base,
            action: "job.created".into(),
            resource_type: "job".into(),
            resource_id: "job-1".into(),
            actor: "service-a".into(),
            detail: serde_json::json!({}),
            source_ip: None,
        },
        AuditEntry {
            timestamp: base,
            action: "device.online".into(),
            resource_type: "device".into(),
            resource_id: "device-1".into(),
            actor: "service-b".into(),
            detail: serde_json::json!({}),
            source_ip: None,
        },
    ];

    let result = AuditFilter::default().apply(entries.clone());

    assert_eq!(result.len(), 2);
}

#[test]
fn audit_filter_zero_limit_returns_empty() {
    let entries = vec![AuditEntry {
        timestamp: chrono::Utc::now(),
        action: "job.created".into(),
        resource_type: "job".into(),
        resource_id: "job-1".into(),
        actor: "service-a".into(),
        detail: serde_json::json!({}),
        source_ip: None,
    }];

    let filter = AuditFilter {
        limit: Some(0),
        ..Default::default()
    };

    assert!(filter.apply(entries).is_empty());
}

#[test]
fn audit_filter_offset_beyond_entries_returns_empty() {
    let entries = vec![AuditEntry {
        timestamp: chrono::Utc::now(),
        action: "job.created".into(),
        resource_type: "job".into(),
        resource_id: "job-1".into(),
        actor: "service-a".into(),
        detail: serde_json::json!({}),
        source_ip: None,
    }];

    let filter = AuditFilter {
        offset: Some(100),
        ..Default::default()
    };

    assert!(filter.apply(entries).is_empty());
}

#[test]
fn audit_filter_apply_on_empty_input_returns_empty() {
    let filter = AuditFilter {
        resource_type: Some("job".into()),
        action: Some("job.created".into()),
        ..Default::default()
    };

    assert!(filter.apply(Vec::new()).is_empty());
}
