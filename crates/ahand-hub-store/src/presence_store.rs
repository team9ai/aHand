use std::sync::Arc;

use ahand_hub_core::{HubError, Result};
use redis::AsyncCommands;
use redis::aio::ConnectionManager;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct RedisPresenceStore {
    connection: Arc<Mutex<ConnectionManager>>,
    ttl_secs: u64,
}

impl RedisPresenceStore {
    pub fn new(connection: ConnectionManager) -> Self {
        Self::new_with_ttl(connection, 60)
    }

    pub fn new_with_ttl(connection: ConnectionManager, ttl_secs: u64) -> Self {
        Self {
            connection: Arc::new(Mutex::new(connection)),
            ttl_secs: ttl_secs.max(1),
        }
    }

    pub async fn mark_online(&self, device_id: &str, endpoint: &str) -> Result<()> {
        let key = presence_key(device_id);
        let mut connection = self.connection.lock().await;
        let _: () = connection
            .set_ex(key, endpoint, self.ttl_secs)
            .await
            .map_err(|err| HubError::Internal(err.to_string()))?;
        Ok(())
    }

    pub async fn is_online(&self, device_id: &str) -> Result<bool> {
        let key = presence_key(device_id);
        let mut connection = self.connection.lock().await;
        connection
            .exists(key)
            .await
            .map_err(|err| HubError::Internal(err.to_string()))
    }

    pub async fn mark_offline(&self, device_id: &str) -> Result<()> {
        let key = presence_key(device_id);
        let mut connection = self.connection.lock().await;
        let _: () = connection
            .del(key)
            .await
            .map_err(|err| HubError::Internal(err.to_string()))?;
        Ok(())
    }
}

fn presence_key(device_id: &str) -> String {
    format!("ahand:hub:presence:{device_id}")
}
