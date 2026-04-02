use ahand_hub_core::audit::{AuditEntry, AuditFilter};
use ahand_hub_core::traits::AuditStore;
use ahand_hub_core::{HubError, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::postgres::Postgres;
use sqlx::{PgPool, QueryBuilder};

#[derive(Clone)]
pub struct PgAuditStore {
    pool: PgPool,
}

impl PgAuditStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AuditStore for PgAuditStore {
    async fn append(&self, entries: &[AuditEntry]) -> Result<()> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|err| HubError::Internal(err.to_string()))?;

        for entry in entries {
            sqlx::query(
                r#"
                INSERT INTO audit_logs (timestamp, action, resource_type, resource_id, actor, detail, source_ip)
                VALUES ($1, $2, $3, $4, $5, $6, $7)
                "#,
            )
            .bind(entry.timestamp)
            .bind(&entry.action)
            .bind(&entry.resource_type)
            .bind(&entry.resource_id)
            .bind(&entry.actor)
            .bind(&entry.detail)
            .bind(&entry.source_ip)
            .execute(&mut *tx)
            .await
            .map_err(|err| HubError::Internal(err.to_string()))?;
        }

        tx.commit()
            .await
            .map_err(|err| HubError::Internal(err.to_string()))?;

        Ok(())
    }

    async fn query(&self, filter: AuditFilter) -> Result<Vec<AuditEntry>> {
        let mut query = QueryBuilder::<Postgres>::new(
            r#"
            SELECT timestamp, action, resource_type, resource_id, actor, detail, source_ip
            FROM audit_logs
            WHERE 1 = 1
            "#,
        );

        if let Some(resource) = filter.resource.as_ref() {
            query
                .push(" AND (resource_type = ")
                .push_bind(resource)
                .push(" OR resource_id = ")
                .push_bind(resource)
                .push(")");
        }
        if let Some(resource_type) = filter.resource_type.as_ref() {
            query.push(" AND resource_type = ").push_bind(resource_type);
        }
        if let Some(resource_id) = filter.resource_id.as_ref() {
            query.push(" AND resource_id = ").push_bind(resource_id);
        }
        if let Some(action) = filter.action.as_ref() {
            query.push(" AND action = ").push_bind(action);
        }
        if let Some(since) = filter.since {
            query.push(" AND timestamp >= ").push_bind(since);
        }
        if let Some(until) = filter.until {
            query.push(" AND timestamp <= ").push_bind(until);
        }

        query.push(" ORDER BY timestamp ");
        if filter.descending {
            query.push("DESC, id DESC");
        } else {
            query.push("ASC, id ASC");
        }

        if let Some(limit) = filter.limit {
            query.push(" LIMIT ").push_bind(limit as i64);
        }
        if let Some(offset) = filter.offset.filter(|offset| *offset > 0) {
            query.push(" OFFSET ").push_bind(offset as i64);
        }

        let rows = query
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|err| HubError::Internal(err.to_string()))?;

        rows.into_iter().map(map_audit).collect()
    }

    async fn prune_before(&self, cutoff: DateTime<Utc>) -> Result<u64> {
        let result = sqlx::query(
            r#"
            DELETE FROM audit_logs
            WHERE timestamp < $1
            "#,
        )
        .bind(cutoff)
        .execute(&self.pool)
        .await
        .map_err(|err| HubError::Internal(err.to_string()))?;

        Ok(result.rows_affected())
    }
}

fn map_audit(row: sqlx::postgres::PgRow) -> Result<AuditEntry> {
    Ok(AuditEntry {
        timestamp: row
            .try_get("timestamp")
            .map_err(|err| HubError::Internal(err.to_string()))?,
        action: row
            .try_get("action")
            .map_err(|err| HubError::Internal(err.to_string()))?,
        resource_type: row
            .try_get("resource_type")
            .map_err(|err| HubError::Internal(err.to_string()))?,
        resource_id: row
            .try_get("resource_id")
            .map_err(|err| HubError::Internal(err.to_string()))?,
        actor: row
            .try_get("actor")
            .map_err(|err| HubError::Internal(err.to_string()))?,
        detail: row
            .try_get("detail")
            .map_err(|err| HubError::Internal(err.to_string()))?,
        source_ip: row
            .try_get("source_ip")
            .map_err(|err| HubError::Internal(err.to_string()))?,
    })
}
