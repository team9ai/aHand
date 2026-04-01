use ahand_hub_core::audit::{AuditEntry, AuditFilter};
use ahand_hub_core::traits::AuditStore;
use ahand_hub_core::{HubError, Result};
use async_trait::async_trait;
use sqlx::PgPool;
use sqlx::Row;

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
            .execute(&self.pool)
            .await
            .map_err(|err| HubError::Internal(err.to_string()))?;
        }

        Ok(())
    }

    async fn query(&self, filter: AuditFilter) -> Result<Vec<AuditEntry>> {
        let rows = sqlx::query(
            r#"
            SELECT timestamp, action, resource_type, resource_id, actor, detail, source_ip
            FROM audit_logs
            WHERE ($1::text IS NULL OR resource_type = $1)
              AND ($2::text IS NULL OR resource_id = $2)
              AND ($3::text IS NULL OR action = $3)
            ORDER BY id
            "#,
        )
        .bind(filter.resource_type)
        .bind(filter.resource_id)
        .bind(filter.action)
        .fetch_all(&self.pool)
        .await
        .map_err(|err| HubError::Internal(err.to_string()))?;

        rows.into_iter().map(map_audit).collect()
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
