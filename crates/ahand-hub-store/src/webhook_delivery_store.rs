//! Durable queue for outbound webhook deliveries.
//!
//! Task 1.5 uses this store to persist each `device.*` webhook so retries
//! survive hub restarts, and so a single attempt lost in-flight doesn't
//! become a dropped event. The shape is deliberately small — one row per
//! `event_id` — and the trait below fronts two interchangeable impls:
//!
//! - [`PgWebhookDeliveryStore`] — production, uses the `webhook_deliveries`
//!   table added in migration `0003`.
//! - [`MemoryWebhookDeliveryStore`] — in-process `DashMap`, used by the
//!   hub's memory mode and by unit / integration tests that don't need
//!   Postgres.
//!
//! Callers enqueue by inserting a [`WebhookDelivery`] (the worker inserts
//! with `attempts = 0` and `next_retry_at = now`). They lease due rows via
//! [`WebhookDeliveryStore::lease_due`], which applies `FOR UPDATE SKIP
//! LOCKED` on Postgres and a best-effort in-process flag on memory, so two
//! workers (or the same worker woken twice) don't double-deliver. On 2xx
//! the caller [`delete`][WebhookDeliveryStore::delete]s the row; on
//! transient failure they call [`mark_failed`][WebhookDeliveryStore::mark_failed]
//! to bump attempts and schedule the next retry.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use sqlx::Row;
use std::sync::Arc;

/// A single pending webhook delivery. `payload` is the JSON body bytes
/// plus metadata that the sender needs (event type, headers, etc.) — see
/// `ahand_hub::webhook::WebhookPayload` for the canonical shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookDelivery {
    pub event_id: String,
    pub payload: serde_json::Value,
    pub attempts: i32,
    pub next_retry_at: DateTime<Utc>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Operations the webhook worker needs on the delivery queue. Each impl
/// must be cheap to `Arc` and safe to call concurrently — the worker
/// holds a single `Arc<dyn WebhookDeliveryStore>`.
#[async_trait]
pub trait WebhookDeliveryStore: Send + Sync {
    /// Upsert a delivery. If a row with the same `event_id` already
    /// exists the store updates `payload` and resets `next_retry_at` /
    /// `attempts` to the new values. (The plan calls for the primary
    /// key to dedupe — but callers may legitimately retry the
    /// serialize step, so we treat the latest enqueue as canonical.)
    async fn enqueue(&self, delivery: WebhookDelivery) -> anyhow::Result<()>;

    /// Claim up to `limit` deliveries whose `next_retry_at <= now`.
    /// Claimed rows are hidden from further `lease_due` calls until the
    /// caller releases them via `delete` or `mark_failed`. On Postgres
    /// this is implemented with `FOR UPDATE SKIP LOCKED`; on memory it
    /// flips an in-process lease flag.
    async fn lease_due(
        &self,
        now: DateTime<Utc>,
        limit: usize,
    ) -> anyhow::Result<Vec<WebhookDelivery>>;

    /// Drop a delivery — used on 2xx success, on permanent failure
    /// (401 signature mismatch), or after the row has been copied to
    /// the DLQ on retry exhaustion.
    async fn delete(&self, event_id: &str) -> anyhow::Result<()>;

    /// Increment `attempts`, set `next_retry_at`, and record
    /// `last_error`. Also releases the in-process lease (memory
    /// backend) so the row becomes visible to the next `lease_due`
    /// tick.
    async fn mark_failed(
        &self,
        event_id: &str,
        next_retry_at: DateTime<Utc>,
        attempts: i32,
        last_error: &str,
    ) -> anyhow::Result<()>;

    /// The earliest `next_retry_at` among pending (non-leased) rows, if
    /// any. The worker uses this to decide how long to sleep between
    /// ticks so it doesn't busy-loop.
    async fn earliest_next_retry(&self) -> anyhow::Result<Option<DateTime<Utc>>>;

    /// Count of rows in the store. Tests use this to assert that the
    /// worker deleted the row after a 2xx response; there is no
    /// operational consumer.
    async fn len(&self) -> anyhow::Result<usize>;
}

/// Postgres-backed [`WebhookDeliveryStore`]. Expects migration 0003 to
/// have run.
#[derive(Clone)]
pub struct PgWebhookDeliveryStore {
    pool: PgPool,
}

impl PgWebhookDeliveryStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl WebhookDeliveryStore for PgWebhookDeliveryStore {
    async fn enqueue(&self, delivery: WebhookDelivery) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO webhook_deliveries
                (event_id, payload, attempts, next_retry_at, last_error, created_at)
            VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (event_id) DO UPDATE SET
                payload = EXCLUDED.payload,
                attempts = EXCLUDED.attempts,
                next_retry_at = EXCLUDED.next_retry_at,
                last_error = EXCLUDED.last_error
            "#,
        )
        .bind(&delivery.event_id)
        .bind(&delivery.payload)
        .bind(delivery.attempts)
        .bind(delivery.next_retry_at)
        .bind(&delivery.last_error)
        .bind(delivery.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn lease_due(
        &self,
        now: DateTime<Utc>,
        limit: usize,
    ) -> anyhow::Result<Vec<WebhookDelivery>> {
        // A single-statement leasing query using CTE + `FOR UPDATE SKIP
        // LOCKED`. Postpones the row's `next_retry_at` by 5 minutes so
        // a crash of the worker mid-POST doesn't wedge the row. The
        // real retry decision (delete on 2xx, mark_failed with proper
        // backoff on 5xx) overwrites this value before the 5-minute
        // lease elapses.
        let rows = sqlx::query(
            r#"
            WITH due AS (
                SELECT event_id FROM webhook_deliveries
                WHERE next_retry_at <= $1
                ORDER BY next_retry_at
                FOR UPDATE SKIP LOCKED
                LIMIT $2
            )
            UPDATE webhook_deliveries w
            SET next_retry_at = $1 + INTERVAL '5 minutes'
            FROM due
            WHERE w.event_id = due.event_id
            RETURNING w.event_id, w.payload, w.attempts, w.next_retry_at,
                      w.last_error, w.created_at
            "#,
        )
        .bind(now)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| WebhookDelivery {
                event_id: row.get("event_id"),
                payload: row.get("payload"),
                attempts: row.get("attempts"),
                next_retry_at: row.get("next_retry_at"),
                last_error: row.get("last_error"),
                created_at: row.get("created_at"),
            })
            .collect())
    }

    async fn delete(&self, event_id: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM webhook_deliveries WHERE event_id = $1")
            .bind(event_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn mark_failed(
        &self,
        event_id: &str,
        next_retry_at: DateTime<Utc>,
        attempts: i32,
        last_error: &str,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            UPDATE webhook_deliveries
            SET attempts = $2,
                next_retry_at = $3,
                last_error = $4
            WHERE event_id = $1
            "#,
        )
        .bind(event_id)
        .bind(attempts)
        .bind(next_retry_at)
        .bind(last_error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn earliest_next_retry(&self) -> anyhow::Result<Option<DateTime<Utc>>> {
        let row = sqlx::query("SELECT MIN(next_retry_at) AS ts FROM webhook_deliveries")
            .fetch_one(&self.pool)
            .await?;
        let ts: Option<DateTime<Utc>> = row.try_get("ts").ok();
        Ok(ts)
    }

    async fn len(&self) -> anyhow::Result<usize> {
        let row = sqlx::query("SELECT COUNT(*)::BIGINT AS n FROM webhook_deliveries")
            .fetch_one(&self.pool)
            .await?;
        let n: i64 = row.get("n");
        Ok(n as usize)
    }
}

/// In-memory [`WebhookDeliveryStore`] for memory-mode hubs and tests.
/// Keeps an in-process `leased` flag per row so concurrent
/// `lease_due` calls don't hand out the same delivery twice.
#[derive(Default)]
pub struct MemoryWebhookDeliveryStore {
    rows: DashMap<String, MemoryRow>,
}

#[derive(Clone)]
struct MemoryRow {
    delivery: WebhookDelivery,
    leased: bool,
}

impl MemoryWebhookDeliveryStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn arc() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

#[async_trait]
impl WebhookDeliveryStore for MemoryWebhookDeliveryStore {
    async fn enqueue(&self, delivery: WebhookDelivery) -> anyhow::Result<()> {
        self.rows.insert(
            delivery.event_id.clone(),
            MemoryRow {
                delivery,
                leased: false,
            },
        );
        Ok(())
    }

    async fn lease_due(
        &self,
        now: DateTime<Utc>,
        limit: usize,
    ) -> anyhow::Result<Vec<WebhookDelivery>> {
        // DashMap iteration order is unspecified, so we collect
        // candidates first, sort by `next_retry_at`, then flip the
        // lease flag. Sorting makes the retry order deterministic
        // (oldest due first) which matches the Pg query above.
        let mut candidates: Vec<WebhookDelivery> = self
            .rows
            .iter()
            .filter(|entry| {
                let row = entry.value();
                !row.leased && row.delivery.next_retry_at <= now
            })
            .map(|entry| entry.value().delivery.clone())
            .collect();
        candidates.sort_by_key(|d| d.next_retry_at);
        candidates.truncate(limit);

        let mut claimed = Vec::with_capacity(candidates.len());
        for delivery in candidates {
            if let Some(mut entry) = self.rows.get_mut(&delivery.event_id)
                && !entry.leased
            {
                entry.leased = true;
                claimed.push(entry.delivery.clone());
            }
        }
        Ok(claimed)
    }

    async fn delete(&self, event_id: &str) -> anyhow::Result<()> {
        self.rows.remove(event_id);
        Ok(())
    }

    async fn mark_failed(
        &self,
        event_id: &str,
        next_retry_at: DateTime<Utc>,
        attempts: i32,
        last_error: &str,
    ) -> anyhow::Result<()> {
        if let Some(mut entry) = self.rows.get_mut(event_id) {
            entry.delivery.attempts = attempts;
            entry.delivery.next_retry_at = next_retry_at;
            entry.delivery.last_error = Some(last_error.to_string());
            entry.leased = false;
        }
        Ok(())
    }

    async fn earliest_next_retry(&self) -> anyhow::Result<Option<DateTime<Utc>>> {
        Ok(self
            .rows
            .iter()
            .filter(|entry| !entry.value().leased)
            .map(|entry| entry.value().delivery.next_retry_at)
            .min())
    }

    async fn len(&self) -> anyhow::Result<usize> {
        Ok(self.rows.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn sample_delivery(id: &str, offset_secs: i64) -> WebhookDelivery {
        WebhookDelivery {
            event_id: id.into(),
            payload: serde_json::json!({ "hello": id }),
            attempts: 0,
            next_retry_at: Utc::now() + Duration::seconds(offset_secs),
            last_error: None,
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn memory_enqueue_and_lease() {
        let store = MemoryWebhookDeliveryStore::new();
        store.enqueue(sample_delivery("a", -1)).await.unwrap();
        store.enqueue(sample_delivery("b", 60)).await.unwrap();

        let leased = store.lease_due(Utc::now(), 10).await.unwrap();
        assert_eq!(leased.len(), 1);
        assert_eq!(leased[0].event_id, "a");
        // Second lease must not re-hand-out the already-leased row.
        let leased2 = store.lease_due(Utc::now(), 10).await.unwrap();
        assert!(leased2.is_empty());
    }

    #[tokio::test]
    async fn memory_upsert_overwrites_payload() {
        let store = MemoryWebhookDeliveryStore::new();
        let mut first = sample_delivery("a", -1);
        first.payload = serde_json::json!({ "v": 1 });
        store.enqueue(first).await.unwrap();

        let mut second = sample_delivery("a", -1);
        second.payload = serde_json::json!({ "v": 2 });
        store.enqueue(second).await.unwrap();

        let leased = store.lease_due(Utc::now(), 10).await.unwrap();
        assert_eq!(leased.len(), 1);
        assert_eq!(leased[0].payload["v"], 2);
    }

    #[tokio::test]
    async fn memory_mark_failed_reschedules_and_unleases() {
        let store = MemoryWebhookDeliveryStore::new();
        store.enqueue(sample_delivery("a", -1)).await.unwrap();
        let _ = store.lease_due(Utc::now(), 10).await.unwrap();

        let future = Utc::now() + Duration::seconds(5);
        store
            .mark_failed("a", future, 1, "boom")
            .await
            .unwrap();

        // Not due yet
        let leased = store.lease_due(Utc::now(), 10).await.unwrap();
        assert!(leased.is_empty());
        // Due now
        let leased = store
            .lease_due(Utc::now() + Duration::seconds(6), 10)
            .await
            .unwrap();
        assert_eq!(leased.len(), 1);
        assert_eq!(leased[0].attempts, 1);
        assert_eq!(leased[0].last_error.as_deref(), Some("boom"));
    }

    #[tokio::test]
    async fn memory_delete_removes_row() {
        let store = MemoryWebhookDeliveryStore::new();
        store.enqueue(sample_delivery("a", -1)).await.unwrap();
        store.delete("a").await.unwrap();
        assert_eq!(store.len().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn memory_earliest_next_retry_tracks_unleased_rows() {
        let store = MemoryWebhookDeliveryStore::new();
        assert!(store.earliest_next_retry().await.unwrap().is_none());
        store.enqueue(sample_delivery("a", 60)).await.unwrap();
        store.enqueue(sample_delivery("b", 10)).await.unwrap();

        let earliest = store.earliest_next_retry().await.unwrap().unwrap();
        let b_time = store.rows.get("b").unwrap().delivery.next_retry_at;
        assert_eq!(earliest, b_time);

        // Leased rows drop out of the earliest computation.
        let _ = store
            .lease_due(Utc::now() + Duration::seconds(15), 1)
            .await
            .unwrap();
        let earliest = store.earliest_next_retry().await.unwrap().unwrap();
        let a_time = store.rows.get("a").unwrap().delivery.next_retry_at;
        assert_eq!(earliest, a_time);
    }

    #[tokio::test]
    async fn memory_mark_failed_missing_row_is_noop() {
        let store = MemoryWebhookDeliveryStore::new();
        store
            .mark_failed("missing", Utc::now(), 1, "boom")
            .await
            .unwrap();
        assert_eq!(store.len().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn memory_arc_constructor_returns_shared_store() {
        let store = MemoryWebhookDeliveryStore::arc();
        store.enqueue(sample_delivery("a", -1)).await.unwrap();
        assert_eq!(store.len().await.unwrap(), 1);
    }
}
