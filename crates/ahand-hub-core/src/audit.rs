use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub timestamp: DateTime<Utc>,
    pub action: String,
    pub resource_type: String,
    pub resource_id: String,
    pub actor: String,
    pub detail: serde_json::Value,
    pub source_ip: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AuditFilter {
    pub resource: Option<String>,
    pub resource_type: Option<String>,
    pub resource_id: Option<String>,
    pub action: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub descending: bool,
}

impl AuditFilter {
    pub fn matches(&self, entry: &AuditEntry) -> bool {
        self.resource.as_ref().is_none_or(|resource| {
            &entry.resource_type == resource || &entry.resource_id == resource
        }) && self
            .resource_type
            .as_ref()
            .is_none_or(|resource_type| &entry.resource_type == resource_type)
            && self
                .resource_id
                .as_ref()
                .is_none_or(|resource_id| &entry.resource_id == resource_id)
            && self
                .action
                .as_ref()
                .is_none_or(|action| &entry.action == action)
            && self.since.is_none_or(|since| entry.timestamp >= since)
            && self.until.is_none_or(|until| entry.timestamp <= until)
    }

    pub fn apply(&self, entries: impl IntoIterator<Item = AuditEntry>) -> Vec<AuditEntry> {
        let mut entries: Vec<_> = entries
            .into_iter()
            .filter(|entry| self.matches(entry))
            .collect();
        entries.sort_by(|left, right| {
            left.timestamp
                .cmp(&right.timestamp)
                .then_with(|| left.action.cmp(&right.action))
                .then_with(|| left.resource_type.cmp(&right.resource_type))
                .then_with(|| left.resource_id.cmp(&right.resource_id))
                .then_with(|| left.actor.cmp(&right.actor))
        });
        if self.descending {
            entries.reverse();
        }

        let offset = self.offset.unwrap_or(0);
        let take = self.limit.unwrap_or(usize::MAX);
        entries.into_iter().skip(offset).take(take).collect()
    }
}
