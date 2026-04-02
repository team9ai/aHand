use std::sync::Arc;

use ahand_hub_core::{HubError, Result};
use redis::AsyncCommands;
use redis::Client;
use redis::aio::{ConnectionManager, PubSub};
use tokio::sync::Mutex;

const EVENTS_CHANNEL: &str = "ahand:events";

#[derive(Clone)]
pub struct RedisEventFanout {
    client: Client,
    connection: Arc<Mutex<ConnectionManager>>,
}

impl RedisEventFanout {
    pub async fn new(redis_url: &str) -> anyhow::Result<Self> {
        let client = Client::open(redis_url)?;
        let connection = crate::redis::connect_redis(redis_url).await?;
        Ok(Self {
            client,
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub async fn publish_json(&self, payload: &str) -> Result<()> {
        let mut connection = self.connection.lock().await;
        let _: i64 = connection
            .publish(EVENTS_CHANNEL, payload)
            .await
            .map_err(redis_err)?;
        Ok(())
    }

    pub async fn subscribe(&self) -> Result<PubSub> {
        let mut pubsub = self.client.get_async_pubsub().await.map_err(redis_err)?;
        pubsub.subscribe(EVENTS_CHANNEL).await.map_err(redis_err)?;
        Ok(pubsub)
    }
}

fn redis_err(err: redis::RedisError) -> HubError {
    HubError::Internal(err.to_string())
}
