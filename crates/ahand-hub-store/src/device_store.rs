use ahand_hub_core::device::{Device, NewDevice};
use ahand_hub_core::traits::DeviceStore;
use ahand_hub_core::{HubError, Result};
use async_trait::async_trait;
use sqlx::PgPool;
use sqlx::Row;

use crate::presence_store::RedisPresenceStore;

#[derive(Clone)]
pub struct PgDeviceStore {
    pool: PgPool,
    presence: Option<RedisPresenceStore>,
}

impl PgDeviceStore {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            presence: None,
        }
    }

    pub fn with_presence(pool: PgPool, presence: RedisPresenceStore) -> Self {
        Self {
            pool,
            presence: Some(presence),
        }
    }

    pub async fn upsert_device(&self, device: NewDevice) -> Result<Device> {
        let device_id = device.id.clone();
        sqlx::query(
            r#"
            INSERT INTO devices (id, public_key, hostname, os, capabilities, version, auth_method)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (id) DO UPDATE
            SET public_key = EXCLUDED.public_key,
                hostname = EXCLUDED.hostname,
                os = EXCLUDED.os,
                capabilities = EXCLUDED.capabilities,
                version = EXCLUDED.version,
                auth_method = EXCLUDED.auth_method,
                last_seen_at = now()
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
            .ok_or_else(|| HubError::Internal(format!("upserted device missing: {device_id}")))
    }

    pub async fn mark_online(&self, device_id: &str, endpoint: &str) -> Result<()> {
        match &self.presence {
            Some(presence) => presence.mark_online(device_id, endpoint).await,
            None => Ok(()),
        }
    }

    pub async fn mark_offline(&self, device_id: &str) -> Result<()> {
        match &self.presence {
            Some(presence) => presence.mark_offline(device_id).await,
            None => Ok(()),
        }
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
        .map_err(|err| match err {
            sqlx::Error::Database(database_err) if database_err.is_unique_violation() => {
                HubError::DeviceAlreadyExists(device_id.clone())
            }
            other => HubError::Internal(other.to_string()),
        })?;

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

        match row {
            Some(row) => {
                let online = self.online_state(device_id).await?;
                Ok(Some(map_device(row, online)?))
            }
            None => Ok(None),
        }
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

        let mut devices = Vec::with_capacity(rows.len());
        for row in rows {
            let device_id = row
                .try_get::<String, _>("id")
                .map_err(|err| HubError::Internal(err.to_string()))?;
            let online = self.online_state(&device_id).await?;
            devices.push(map_device(row, online)?);
        }

        Ok(devices)
    }

    async fn delete(&self, device_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM devices WHERE id = $1")
            .bind(device_id)
            .execute(&self.pool)
            .await
            .map_err(|err| HubError::Internal(err.to_string()))?;
        self.mark_offline(device_id).await?;

        Ok(())
    }
}

impl PgDeviceStore {
    async fn online_state(&self, device_id: &str) -> Result<bool> {
        match &self.presence {
            Some(presence) => presence.is_online(device_id).await,
            None => Ok(false),
        }
    }
}

fn map_device(row: sqlx::postgres::PgRow, online: bool) -> Result<Device> {
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
        online,
    })
}
