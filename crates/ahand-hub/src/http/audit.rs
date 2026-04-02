use ahand_hub_core::audit::{AuditEntry, AuditFilter};
use axum::{
    Json,
    extract::rejection::QueryRejection,
    extract::{Query, State},
};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::auth::AuthContextExt;
use crate::http::api_error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Debug, Deserialize, Default)]
pub struct AuditLogQuery {
    pub action: Option<String>,
    pub resource: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

pub async fn list_audit_logs(
    auth: AuthContextExt,
    State(state): State<AppState>,
    query: Result<Query<AuditLogQuery>, QueryRejection>,
) -> ApiResult<Json<Vec<AuditEntry>>> {
    auth.require_read_audit()?;
    let Query(query) = query.map_err(ApiError::from_query_rejection)?;

    let since = parse_timestamp(query.since.as_deref())?;
    let until = parse_timestamp(query.until.as_deref())?;
    let mut entries = state
        .audit_store
        .query(AuditFilter {
            action: query.action.clone(),
            ..Default::default()
        })
        .await
        .map_err(|_| ApiError::internal("Failed to list audit logs"))?;

    entries.retain(|entry| {
        query.resource.as_ref().is_none_or(|resource| {
            &entry.resource_type == resource || &entry.resource_id == resource
        }) && since.is_none_or(|since| entry.timestamp >= since)
            && until.is_none_or(|until| entry.timestamp <= until)
    });
    entries.sort_by(|left, right| right.timestamp.cmp(&left.timestamp));
    apply_pagination(&mut entries, query.offset.unwrap_or(0), query.limit);

    Ok(Json(entries))
}

fn parse_timestamp(value: Option<&str>) -> ApiResult<Option<DateTime<Utc>>> {
    match value {
        None => Ok(None),
        Some(value) => DateTime::parse_from_rfc3339(value)
            .map(|timestamp| Some(timestamp.with_timezone(&Utc)))
            .map_err(|_| ApiError::validation(format!("Invalid RFC3339 timestamp: {value}"))),
    }
}

fn apply_pagination<T>(items: &mut Vec<T>, offset: usize, limit: Option<usize>) {
    if offset == 0 && limit.is_none() {
        return;
    }

    let take = limit.unwrap_or(usize::MAX);
    let paged = std::mem::take(items)
        .into_iter()
        .skip(offset)
        .take(take)
        .collect();
    *items = paged;
}
