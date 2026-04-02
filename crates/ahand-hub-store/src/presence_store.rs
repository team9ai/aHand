use std::collections::HashMap;
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

    pub async fn online_states(&self, device_ids: &[String]) -> Result<HashMap<String, bool>> {
        if device_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let mut pipeline = redis::pipe();
        for device_id in device_ids {
            pipeline.cmd("EXISTS").arg(presence_key(device_id));
        }

        let mut connection = self.connection.lock().await;
        let exists: Vec<u64> = pipeline
            .query_async(&mut *connection)
            .await
            .map_err(|err| HubError::Internal(err.to_string()))?;

        Ok(device_ids
            .iter()
            .cloned()
            .zip(exists.into_iter().map(|value| value > 0))
            .collect())
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
