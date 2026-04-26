use std::sync::Arc;
use std::time::Duration;

use ahand_hub_core::{HubError, Result};
use redis::AsyncCommands;
use redis::Script;
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
            let inserted: i32 = issue_bootstrap_token_script()
                .key(device_token_key(device_id))
                .key(bootstrap_token_key(&token))
                .arg(&token)
                .arg(device_id)
                .invoke_async(&mut *connection)
                .await
                .map_err(redis_err)?;
            if inserted == 0 {
                continue;
            }
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
        let ttl_ms = self.reservation_ttl.as_millis().min(i64::MAX as u128) as i64;
        let reserved: i32 = reserve_bootstrap_token_script()
            .key(bootstrap_token_key(token))
            .key(bootstrap_lock_key(token))
            .arg(device_id)
            .arg(&reservation_id)
            .arg(ttl_ms)
            .invoke_async(&mut *connection)
            .await
            .map_err(redis_err)?;
        if reserved == 0 {
            return Ok(None);
        }

        Ok(Some(RedisBootstrapReservation {
            token: token.into(),
            device_id: device_id.into(),
            reservation_id,
        }))
    }

    pub async fn release(&self, reservation: &RedisBootstrapReservation) -> Result<()> {
        let mut connection = self.connection.lock().await;
        let _: i32 = release_bootstrap_reservation_script()
            .key(bootstrap_lock_key(&reservation.token))
            .arg(&reservation.reservation_id)
            .invoke_async(&mut *connection)
            .await
            .map_err(redis_err)?;
        Ok(())
    }

    pub async fn consume(&self, reservation: &RedisBootstrapReservation) -> Result<()> {
        let mut connection = self.connection.lock().await;
        let _: i32 = consume_bootstrap_reservation_script()
            .key(bootstrap_lock_key(&reservation.token))
            .key(bootstrap_token_key(&reservation.token))
            .key(device_token_key(&reservation.device_id))
            .arg(&reservation.reservation_id)
            .invoke_async(&mut *connection)
            .await
            .map_err(redis_err)?;
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

fn issue_bootstrap_token_script() -> Script {
    Script::new(
        r#"
        local existing = redis.call('GET', KEYS[1])
        if redis.call('EXISTS', KEYS[2]) == 1 then
            return 0
        end
        if existing then
            redis.call('DEL', 'ahand:bootstrap:' .. existing, 'ahand:bootstrap:' .. existing .. ':lock')
        end
        redis.call('SET', KEYS[2], ARGV[2])
        redis.call('SET', KEYS[1], ARGV[1])
        return 1
        "#,
    )
}

fn reserve_bootstrap_token_script() -> Script {
    Script::new(
        r#"
        local bound = redis.call('GET', KEYS[1])
        if not bound or bound ~= ARGV[1] then
            return 0
        end
        local reserved = redis.call('SET', KEYS[2], ARGV[2], 'NX', 'PX', ARGV[3])
        if not reserved then
            return 0
        end
        return 1
        "#,
    )
}

fn release_bootstrap_reservation_script() -> Script {
    Script::new(
        r#"
        if redis.call('GET', KEYS[1]) == ARGV[1] then
            return redis.call('DEL', KEYS[1])
        end
        return 0
        "#,
    )
}

fn consume_bootstrap_reservation_script() -> Script {
    Script::new(
        r#"
        if redis.call('GET', KEYS[1]) ~= ARGV[1] then
            return 0
        end
        redis.call('DEL', KEYS[2], KEYS[1], KEYS[3])
        return 1
        "#,
    )
}
