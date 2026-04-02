use std::sync::Arc;
use std::time::Duration;

use ahand_hub_core::{HubError, Result};
use redis::AsyncCommands;
use redis::aio::ConnectionManager;
use tokio::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedisBootstrapReservation {
    pub token: String,
    pub device_id: String,
    pub reservation_id: String,
}

#[derive(Clone)]
pub struct RedisBootstrapStore {
    connection: Arc<Mutex<ConnectionManager>>,
    reservation_ttl: Duration,
}

impl RedisBootstrapStore {
    pub fn new(connection: ConnectionManager, reservation_ttl: Duration) -> Self {
        Self {
            connection: Arc::new(Mutex::new(connection)),
            reservation_ttl,
        }
    }

    pub async fn issue(&self, device_id: &str) -> Result<String> {
        let mut connection = self.connection.lock().await;
        if let Some(existing_token) = connection
            .get::<_, Option<String>>(device_token_key(device_id))
            .await
            .map_err(redis_err)?
        {
            let keys = vec![
                bootstrap_token_key(&existing_token),
                bootstrap_lock_key(&existing_token),
            ];
            let _: usize = connection.del(keys).await.map_err(redis_err)?;
        }

        for _ in 0..4 {
            let token = uuid::Uuid::new_v4().simple().to_string();
            let inserted: bool = connection
                .set_nx(bootstrap_token_key(&token), device_id)
                .await
                .map_err(redis_err)?;
            if !inserted {
                continue;
            }

            let _: () = connection
                .set(device_token_key(device_id), &token)
                .await
                .map_err(redis_err)?;
            return Ok(token);
        }

        Err(HubError::Internal(
            "failed to allocate unique bootstrap token".into(),
        ))
    }

    pub async fn reserve(
        &self,
        device_id: &str,
        token: &str,
    ) -> Result<Option<RedisBootstrapReservation>> {
        let mut connection = self.connection.lock().await;
        let Some(bound_device_id) = connection
            .get::<_, Option<String>>(bootstrap_token_key(token))
            .await
            .map_err(redis_err)?
        else {
            return Ok(None);
        };
        if bound_device_id != device_id {
            return Ok(None);
        }

        let reservation_id = uuid::Uuid::new_v4().simple().to_string();
        let reserved: bool = connection
            .set_nx(bootstrap_lock_key(token), &reservation_id)
            .await
            .map_err(redis_err)?;
        if !reserved {
            return Ok(None);
        }

        let ttl_ms = self.reservation_ttl.as_millis().min(i64::MAX as u128) as i64;
        let _: bool = connection
            .pexpire(bootstrap_lock_key(token), ttl_ms)
            .await
            .map_err(redis_err)?;

        Ok(Some(RedisBootstrapReservation {
            token: token.into(),
            device_id: device_id.into(),
            reservation_id,
        }))
    }

    pub async fn release(&self, reservation: &RedisBootstrapReservation) -> Result<()> {
        let mut connection = self.connection.lock().await;
        let lock_key = bootstrap_lock_key(&reservation.token);
        let current = connection
            .get::<_, Option<String>>(&lock_key)
            .await
            .map_err(redis_err)?;
        if current.as_deref() == Some(reservation.reservation_id.as_str()) {
            let _: usize = connection.del(lock_key).await.map_err(redis_err)?;
        }
        Ok(())
    }

    pub async fn consume(&self, reservation: &RedisBootstrapReservation) -> Result<()> {
        let mut connection = self.connection.lock().await;
        let lock_key = bootstrap_lock_key(&reservation.token);
        let current = connection
            .get::<_, Option<String>>(&lock_key)
            .await
            .map_err(redis_err)?;
        if current.as_deref() != Some(reservation.reservation_id.as_str()) {
            return Ok(());
        }

        let keys = vec![
            bootstrap_token_key(&reservation.token),
            lock_key,
            device_token_key(&reservation.device_id),
        ];
        let _: usize = connection.del(keys).await.map_err(redis_err)?;
        Ok(())
    }

    pub async fn delete_device(&self, device_id: &str) -> Result<()> {
        let mut connection = self.connection.lock().await;
        let device_key = device_token_key(device_id);
        let token = connection
            .get::<_, Option<String>>(&device_key)
            .await
            .map_err(redis_err)?;
        let _: usize = connection.del(device_key).await.map_err(redis_err)?;
        if let Some(token) = token {
            let keys = vec![bootstrap_token_key(&token), bootstrap_lock_key(&token)];
            let _: usize = connection.del(keys).await.map_err(redis_err)?;
        }
        Ok(())
    }
}

fn bootstrap_token_key(token: &str) -> String {
    format!("ahand:bootstrap:{token}")
}

fn bootstrap_lock_key(token: &str) -> String {
    format!("ahand:bootstrap:{token}:lock")
}

fn device_token_key(device_id: &str) -> String {
    format!("ahand:bootstrap:device:{device_id}")
}

fn redis_err(err: redis::RedisError) -> HubError {
    HubError::Internal(err.to_string())
}
