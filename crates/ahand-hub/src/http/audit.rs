use ahand_hub_core::audit::{AuditEntry, AuditFilter};
use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::auth::AuthContextExt;
use crate::state::AppState;

#[derive(Debug, Deserialize, Default)]
pub struct AuditLogQuery {
    pub action: Option<String>,
    pub resource: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub limit: Option<usize>,
}

pub async fn list_audit_logs(
    auth: AuthContextExt,
    State(state): State<AppState>,
    Query(query): Query<AuditLogQuery>,
) -> Result<Json<Vec<AuditEntry>>, StatusCode> {
    auth.require_read_audit()?;

    let since = parse_timestamp(query.since.as_deref())?;
    let until = parse_timestamp(query.until.as_deref())?;
    let mut entries = state
        .audit_store
        .query(AuditFilter {
            action: query.action.clone(),
            ..Default::default()
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    entries.retain(|entry| {
        query.resource.as_ref().is_none_or(|resource| {
            &entry.resource_type == resource || &entry.resource_id == resource
        }) && since.is_none_or(|since| entry.timestamp >= since)
            && until.is_none_or(|until| entry.timestamp <= until)
    });
    entries.sort_by(|left, right| right.timestamp.cmp(&left.timestamp));
    if let Some(limit) = query.limit {
        entries.truncate(limit);
    }

    Ok(Json(entries))
}

fn parse_timestamp(value: Option<&str>) -> Result<Option<DateTime<Utc>>, StatusCode> {
    match value {
        None => Ok(None),
        Some(value) => DateTime::parse_from_rfc3339(value)
            .map(|timestamp| Some(timestamp.with_timezone(&Utc)))
            .map_err(|_| StatusCode::BAD_REQUEST),
    }
}
