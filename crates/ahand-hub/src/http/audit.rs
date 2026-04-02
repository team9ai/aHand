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
    let entries = state
        .audit_store
        .query(AuditFilter {
            resource: query.resource.clone(),
            action: query.action.clone(),
            since,
            until,
            limit: query.limit,
            offset: query.offset,
            descending: true,
            ..Default::default()
        })
        .await
        .map_err(|_| ApiError::internal("Failed to list audit logs"))?;

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
