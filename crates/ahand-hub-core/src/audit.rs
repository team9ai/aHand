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
    pub resource_type: Option<String>,
    pub resource_id: Option<String>,
    pub action: Option<String>,
}
