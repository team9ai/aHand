use std::collections::HashMap;

use ahand_hub_core::device::{Device, NewDevice};
use ahand_hub_core::traits::{DeviceAdminStore, DeviceStore};
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
            INSERT INTO devices (
                id, public_key, hostname, os, capabilities, version, auth_method, external_user_id
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (id) DO UPDATE
            SET public_key = EXCLUDED.public_key,
                hostname = EXCLUDED.hostname,
                os = EXCLUDED.os,
                capabilities = EXCLUDED.capabilities,
                version = EXCLUDED.version,
                auth_method = EXCLUDED.auth_method,
                external_user_id = COALESCE(
                    EXCLUDED.external_user_id,
                    devices.external_user_id
                ),
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
        .bind(&device.external_user_id)
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
            INSERT INTO devices (
                id, public_key, hostname, os, capabilities, version, auth_method, external_user_id
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            "#,
        )
        .bind(&device.id)
        .bind(&device.public_key)
        .bind(&device.hostname)
        .bind(&device.os)
        .bind(&device.capabilities)
        .bind(&device.version)
        .bind(&device.auth_method)
        .bind(&device.external_user_id)
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
            SELECT id, public_key, hostname, os, capabilities, version, auth_method, external_user_id
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
            SELECT id, public_key, hostname, os, capabilities, version, auth_method, external_user_id
            FROM devices
            ORDER BY id
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|err| HubError::Internal(err.to_string()))?;

        let mut rows_with_ids = Vec::with_capacity(rows.len());
        let mut device_ids = Vec::with_capacity(rows.len());
        for row in rows {
            let device_id = row
                .try_get::<String, _>("id")
                .map_err(|err| HubError::Internal(err.to_string()))?;
            device_ids.push(device_id.clone());
            rows_with_ids.push((row, device_id));
        }

        let online_states = self.online_states(&device_ids).await?;
        rows_with_ids
            .into_iter()
            .map(|(row, device_id)| {
                map_device(row, online_states.get(&device_id).copied().unwrap_or(false))
            })
            .collect()
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

#[async_trait]
impl DeviceAdminStore for PgDeviceStore {
    async fn pre_register(
        &self,
        device_id: &str,
        public_key: &[u8],
        external_user_id: &str,
    ) -> Result<Device> {
        // Idempotency: if a matching row already exists with the same
        // external_user_id AND same public_key, return it unchanged. If
        // external_user_id differs, reject. Anything else (public_key
        // differs) is treated as an update — the admin is the source of
        // truth for the device's public key.
        if let Some(existing) = self.find_by_id(device_id).await? {
            if let Some(existing_user) = existing.external_user_id.as_ref() {
                if existing_user != external_user_id {
                    return Err(HubError::DeviceOwnedByDifferentUser {
                        device_id: device_id.into(),
                        existing_external_user_id: existing_user.clone(),
                    });
                }
            }
        }

        sqlx::query(
            r#"
            INSERT INTO devices (
                id, public_key, hostname, os, capabilities, version, auth_method, external_user_id
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (id) DO UPDATE
            SET public_key = EXCLUDED.public_key,
                external_user_id = EXCLUDED.external_user_id
            "#,
        )
        .bind(device_id)
        .bind(public_key)
        .bind("pending-device")
        .bind("unknown")
        .bind(Vec::<String>::new())
        .bind(None::<String>)
        .bind("preregistered")
        .bind(external_user_id)
        .execute(&self.pool)
        .await
        .map_err(|err| HubError::Internal(err.to_string()))?;

        self.get(device_id)
            .await?
            .ok_or_else(|| HubError::Internal(format!("preregistered device missing: {device_id}")))
    }

    async fn find_by_id(&self, device_id: &str) -> Result<Option<Device>> {
        self.get(device_id).await
    }

    async fn delete_device(&self, device_id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM devices WHERE id = $1")
            .bind(device_id)
            .execute(&self.pool)
            .await
            .map_err(|err| HubError::Internal(err.to_string()))?;
        let removed = result.rows_affected() > 0;
        if removed {
            self.mark_offline(device_id).await?;
        }
        Ok(removed)
    }

    async fn list_by_external_user(&self, external_user_id: &str) -> Result<Vec<Device>> {
        let rows = sqlx::query(
            r#"
            SELECT id, public_key, hostname, os, capabilities, version, auth_method, external_user_id
            FROM devices
            WHERE external_user_id = $1
            ORDER BY id
            "#,
        )
        .bind(external_user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|err| HubError::Internal(err.to_string()))?;

        let mut rows_with_ids = Vec::with_capacity(rows.len());
        let mut device_ids = Vec::with_capacity(rows.len());
        for row in rows {
            let device_id = row
                .try_get::<String, _>("id")
                .map_err(|err| HubError::Internal(err.to_string()))?;
            device_ids.push(device_id.clone());
            rows_with_ids.push((row, device_id));
        }
        let online_states = self.online_states(&device_ids).await?;
        rows_with_ids
            .into_iter()
            .map(|(row, device_id)| {
                map_device(row, online_states.get(&device_id).copied().unwrap_or(false))
            })
            .collect()
    }
}

impl PgDeviceStore {
    async fn online_state(&self, device_id: &str) -> Result<bool> {
        match &self.presence {
            Some(presence) => presence.is_online(device_id).await,
            None => Ok(false),
        }
    }

    async fn online_states(&self, device_ids: &[String]) -> Result<HashMap<String, bool>> {
        match &self.presence {
            Some(presence) => presence.online_states(device_ids).await,
            None => Ok(device_ids
                .iter()
                .cloned()
                .map(|device_id| (device_id, false))
                .collect()),
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
        external_user_id: row
            .try_get("external_user_id")
            .map_err(|err| HubError::Internal(err.to_string()))?,
    })
}
