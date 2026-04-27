//! Redis-backed [`OutboxStore`] implementation.
//!
//! This module currently ships the lock-related primitives (acquire / kick /
//! subscribe / renew / release). The data-path methods
//! (`reconcile_on_hello`, `unacked_frames`, `fenced_incr_seq`, `xadd_frame`,
//! `observe_ack`) are stubbed and return [`HubError::Internal`] — they will
//! land in Task 4.

use std::sync::Arc;

use ahand_hub_core::traits::{AbortOnDropHandle, KickSubscription, OutboxStore};
use ahand_hub_core::{HubError, Result};
use async_trait::async_trait;
use futures_util::StreamExt;
use redis::aio::ConnectionManager;
use redis::{AsyncCommands, Client};
use tokio::sync::{Mutex, watch};

use crate::outbox_lua::OutboxScripts;

const LOCK_TTL_SECS: u64 = 30;

#[derive(Clone)]
pub struct RedisOutboxStore {
    client: Client,
    conn: Arc<Mutex<ConnectionManager>>,
    scripts: Arc<OutboxScripts>,
}

impl RedisOutboxStore {
    pub async fn new(redis_url: &str) -> anyhow::Result<Self> {
        let client = Client::open(redis_url)?;
        let conn = crate::redis::connect_redis(redis_url).await?;
        Ok(Self {
            client,
            conn: Arc::new(Mutex::new(conn)),
            scripts: Arc::new(OutboxScripts::load()),
        })
    }

    fn lock_key(device_id: &str) -> String {
        format!("lock:device:{device_id}")
    }

    fn kick_channel(device_id: &str) -> String {
        format!("kick:{device_id}")
    }
}

fn redis_err(err: redis::RedisError) -> HubError {
    HubError::Internal(err.to_string())
}

#[async_trait]
impl OutboxStore for RedisOutboxStore {
    async fn try_acquire_lock(&self, device_id: &str, session_id: &str) -> Result<bool> {
        let mut conn = self.conn.lock().await;
        let lock_key = Self::lock_key(device_id);
        let result: i64 = self
            .scripts
            .acquire_lock
            .key(lock_key)
            .arg(session_id)
            .arg(LOCK_TTL_SECS)
            .invoke_async(&mut *conn)
            .await
            .map_err(redis_err)?;
        Ok(result == 1)
    }

    async fn kick(&self, device_id: &str, new_session_id: &str) -> Result<()> {
        let mut conn = self.conn.lock().await;
        let _: i64 = conn
            .publish(Self::kick_channel(device_id), new_session_id)
            .await
            .map_err(redis_err)?;
        Ok(())
    }

    async fn subscribe_kick(&self, device_id: &str) -> Result<KickSubscription> {
        let channel = Self::kick_channel(device_id);
        let mut pubsub = self.client.get_async_pubsub().await.map_err(redis_err)?;
        pubsub
            .subscribe(channel.as_str())
            .await
            .map_err(redis_err)?;

        let (tx, rx) = watch::channel(0u64);

        // The subscribe call has already landed by the time we spawn this
        // task; any kick PUBLISHed from now on will be delivered. The task
        // owns the pubsub connection and lives until either:
        //   - the watch receiver is dropped (we exit the loop on send error)
        //   - AbortOnDropHandle::drop fires when KickSubscription is dropped
        let join = tokio::spawn(async move {
            let mut stream = pubsub.on_message();
            let mut counter: u64 = 0;
            while let Some(_msg) = stream.next().await {
                counter = counter.wrapping_add(1);
                if tx.send(counter).is_err() {
                    break;
                }
            }
        });

        Ok(KickSubscription {
            recv: rx,
            _drop_guard: AbortOnDropHandle::new(join),
        })
    }

    async fn renew_lock(&self, device_id: &str, session_id: &str) -> Result<bool> {
        let mut conn = self.conn.lock().await;
        let result: i64 = self
            .scripts
            .renew_lock
            .key(Self::lock_key(device_id))
            .arg(session_id)
            .arg(LOCK_TTL_SECS)
            .invoke_async(&mut *conn)
            .await
            .map_err(redis_err)?;
        Ok(result == 1)
    }

    async fn release_lock(&self, device_id: &str, session_id: &str) -> Result<()> {
        let mut conn = self.conn.lock().await;
        let _: i64 = self
            .scripts
            .release_lock
            .key(Self::lock_key(device_id))
            .arg(session_id)
            .invoke_async(&mut *conn)
            .await
            .map_err(redis_err)?;
        Ok(())
    }

    // ── stub implementations of the rest; filled in by Task 4 ──

    async fn reconcile_on_hello(
        &self,
        _device_id: &str,
        _session_id: &str,
        _last_ack: u64,
    ) -> Result<u64> {
        Err(HubError::Internal(
            "reconcile_on_hello not implemented (Task 4)".into(),
        ))
    }

    async fn unacked_frames(&self, _device_id: &str, _last_ack: u64) -> Result<Vec<Vec<u8>>> {
        Err(HubError::Internal(
            "unacked_frames not implemented (Task 4)".into(),
        ))
    }

    async fn fenced_incr_seq(&self, _device_id: &str, _session_id: &str) -> Result<u64> {
        Err(HubError::Internal(
            "fenced_incr_seq not implemented (Task 4)".into(),
        ))
    }

    async fn xadd_frame(
        &self,
        _device_id: &str,
        _session_id: &str,
        _seq: u64,
        _frame: Vec<u8>,
    ) -> Result<()> {
        Err(HubError::Internal(
            "xadd_frame not implemented (Task 4)".into(),
        ))
    }

    async fn observe_ack(&self, _device_id: &str, _ack: u64) -> Result<()> {
        Err(HubError::Internal(
            "observe_ack not implemented (Task 4)".into(),
        ))
    }
}
