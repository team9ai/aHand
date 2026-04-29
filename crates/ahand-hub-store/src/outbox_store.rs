//! Redis-backed [`OutboxStore`] implementation.
//!
//! Ships both the lock-related primitives (acquire / kick / subscribe /
//! renew / release) and the data-path methods (`reconcile_on_hello`,
//! `unacked_frames`, `fenced_incr_seq`, `xadd_frame`, `observe_ack`).
//! The data-path methods rely on fenced Lua scripts so a stale session
//! cannot bump the seq counter or write into the stream after a newer
//! session has taken the lock.

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
const RETENTION_SECS: u64 = 30 * 24 * 60 * 60; // 30 days
const STREAM_MAXLEN: u64 = 10_000;

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

    fn seq_key(device_id: &str) -> String {
        format!("seq:{device_id}")
    }

    fn outbox_key(device_id: &str) -> String {
        format!("outbox:{device_id}")
    }
}

fn redis_err(err: redis::RedisError) -> HubError {
    HubError::Internal(err.to_string())
}

fn map_redis_err(err: redis::RedisError) -> HubError {
    let not_owner_detail = err
        .detail()
        .is_some_and(|detail| detail == "NOT_OWNER" || detail.starts_with("NOT_OWNER "));
    if err.code() == Some("NOT_OWNER") || not_owner_detail {
        HubError::Unauthorized
    } else {
        HubError::Internal(err.to_string())
    }
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

    async fn reconcile_on_hello(
        &self,
        device_id: &str,
        session_id: &str,
        last_ack: u64,
    ) -> Result<u64> {
        let mut conn = self.conn.lock().await;
        let returned: u64 = self
            .scripts
            .reconcile_on_hello
            .key(Self::lock_key(device_id))
            .key(Self::seq_key(device_id))
            .key(Self::outbox_key(device_id))
            .arg(session_id)
            .arg(last_ack.to_string())
            .arg(RETENTION_SECS)
            .invoke_async(&mut *conn)
            .await
            .map_err(map_redis_err)?;
        Ok(returned)
    }

    async fn unacked_frames(&self, device_id: &str, last_ack: u64) -> Result<Vec<Vec<u8>>> {
        use redis::streams::StreamRangeReply;
        let mut conn = self.conn.lock().await;
        // XRANGE expects exclusive start prefixed with '('. last_ack=0 means
        // "everything"; encode that as start='-' so we don't synthesize 0-0.
        let start = if last_ack == 0 {
            "-".to_string()
        } else {
            format!("(0-{last_ack}")
        };
        let reply: StreamRangeReply = conn
            .xrange(Self::outbox_key(device_id), start, "+")
            .await
            .map_err(redis_err)?;
        let mut frames = Vec::with_capacity(reply.ids.len());
        for entry in reply.ids {
            if let Some(bytes) = entry.get::<Vec<u8>>("frame") {
                frames.push(bytes);
            }
        }
        Ok(frames)
    }

    async fn fenced_incr_seq(&self, device_id: &str, session_id: &str) -> Result<u64> {
        let mut conn = self.conn.lock().await;
        let seq: u64 = self
            .scripts
            .fenced_incr_seq
            .key(Self::lock_key(device_id))
            .key(Self::seq_key(device_id))
            .arg(session_id)
            .arg(RETENTION_SECS)
            .invoke_async(&mut *conn)
            .await
            .map_err(map_redis_err)?;
        Ok(seq)
    }

    async fn xadd_frame(
        &self,
        device_id: &str,
        session_id: &str,
        seq: u64,
        frame: Vec<u8>,
    ) -> Result<()> {
        let mut conn = self.conn.lock().await;
        let _: i64 = self
            .scripts
            .fenced_xadd
            .key(Self::lock_key(device_id))
            .key(Self::outbox_key(device_id))
            .arg(session_id)
            .arg(seq.to_string())
            .arg(frame)
            .arg(STREAM_MAXLEN)
            .arg(RETENTION_SECS)
            .invoke_async(&mut *conn)
            .await
            .map_err(map_redis_err)?;
        Ok(())
    }

    async fn observe_ack(&self, device_id: &str, ack: u64) -> Result<()> {
        if ack == 0 {
            return Ok(());
        }
        let mut conn = self.conn.lock().await;
        // Lua-atomic: trim the stream only if the claimed ack is within
        // the range of seqs we have actually issued. An invalid ack
        // (claim > issued) is ignored to protect the legitimate replay
        // buffer from a buggy/compromised client.
        let _: i64 = self
            .scripts
            .bounded_observe_ack
            .key(Self::seq_key(device_id))
            .key(Self::outbox_key(device_id))
            .arg(ack.to_string())
            .invoke_async(&mut *conn)
            .await
            .map_err(redis_err)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_not_owner_response_detail_to_unauthorized() {
        let err = redis::RedisError::from((
            redis::ErrorKind::ResponseError,
            "An error was signalled by the server",
            "NOT_OWNER".to_string(),
        ));

        assert!(matches!(map_redis_err(err), HubError::Unauthorized));
    }
}
