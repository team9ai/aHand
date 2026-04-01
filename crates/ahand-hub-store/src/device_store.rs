use ahand_hub_core::device::{Device, NewDevice};
use ahand_hub_core::traits::DeviceStore;
use ahand_hub_core::{HubError, Result};
use async_trait::async_trait;
use sqlx::PgPool;
use sqlx::Row;

#[derive(Clone)]
pub struct PgDeviceStore {
    pool: PgPool,
}

impl PgDeviceStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl DeviceStore for PgDeviceStore {
    async fn insert(&self, device: NewDevice) -> Result<Device> {
        let device_id = device.id.clone();
        sqlx::query(
            r#"
            INSERT INTO devices (id, public_key, hostname, os, capabilities, version, auth_method)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
        )
        .bind(&device.id)
        .bind(&device.public_key)
        .bind(&device.hostname)
        .bind(&device.os)
        .bind(&device.capabilities)
        .bind(&device.version)
        .bind(&device.auth_method)
        .execute(&self.pool)
        .await
        .map_err(|err| HubError::Internal(err.to_string()))?;

        self.get(&device_id)
            .await?
            .ok_or_else(|| HubError::Internal(format!("inserted device missing: {device_id}")))
    }

    async fn get(&self, device_id: &str) -> Result<Option<Device>> {
        let row = sqlx::query(
            r#"
            SELECT id, public_key, hostname, os, capabilities, version, auth_method
            FROM devices
            WHERE id = $1
            "#,
        )
        .bind(device_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| HubError::Internal(err.to_string()))?;

        row.map(map_device).transpose()
    }

    async fn list(&self) -> Result<Vec<Device>> {
        let rows = sqlx::query(
            r#"
            SELECT id, public_key, hostname, os, capabilities, version, auth_method
            FROM devices
            ORDER BY id
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|err| HubError::Internal(err.to_string()))?;

        rows.into_iter().map(map_device).collect()
    }

    async fn delete(&self, device_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM devices WHERE id = $1")
            .bind(device_id)
            .execute(&self.pool)
            .await
            .map_err(|err| HubError::Internal(err.to_string()))?;

        Ok(())
    }
}

fn map_device(row: sqlx::postgres::PgRow) -> Result<Device> {
    Ok(Device {
        id: row
            .try_get("id")
            .map_err(|err| HubError::Internal(err.to_string()))?,
        public_key: row
            .try_get("public_key")
            .map_err(|err| HubError::Internal(err.to_string()))?,
        hostname: row
            .try_get("hostname")
            .map_err(|err| HubError::Internal(err.to_string()))?,
        os: row
            .try_get("os")
            .map_err(|err| HubError::Internal(err.to_string()))?,
        capabilities: row
            .try_get("capabilities")
            .map_err(|err| HubError::Internal(err.to_string()))?,
        version: row
            .try_get("version")
            .map_err(|err| HubError::Internal(err.to_string()))?,
        auth_method: row
            .try_get("auth_method")
            .map_err(|err| HubError::Internal(err.to_string()))?,
        online: false,
    })
}
