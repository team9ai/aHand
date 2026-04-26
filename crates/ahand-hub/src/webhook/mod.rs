//! Outbound webhook dispatcher for device lifecycle events.
//!
//! The hub owns a single [`Webhook`] handle that the gateway, admin API,
//! and device WS handlers call to notify team9's gateway about device
//! state changes:
//!
//! - `device.registered` — the first time a daemon completes a hello
//!   with an `externalUserId` (admin pre-registered it).
//! - `device.online` — the WS accepted a verified hello.
//! - `device.offline` — the WS disconnected or was kicked.
//! - `device.heartbeat` — inbound `Heartbeat` envelope from the daemon.
//! - `device.revoked` — admin DELETE removed the device row.
//!
//! The caller invokes one of the `enqueue_*` helpers; the helper builds
//! a [`WebhookPayload`], persists it to [`WebhookDeliveryStore`], and
//! nudges the background worker via `tokio::sync::Notify`. The worker
//! ([`worker::run`]) POSTs to `webhook_url` with an HMAC-SHA256
//! signature over the raw body, retrying with exponential backoff (1s,
//! 2s, 4s, …, capped at 256s) up to `max_retries` before DLQing.
//!
//! Memory-mode hubs and tests can call [`Webhook::disabled`] to get a
//! no-op instance — the enqueue helpers still accept calls but return
//! immediately without persisting or POSTing anything.

use std::sync::Arc;
use std::time::Duration;

use ahand_hub_store::webhook_delivery_store::{WebhookDelivery, WebhookDeliveryStore};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, Semaphore};

pub mod sender;
pub mod worker;

#[cfg(test)]
mod tests;

/// Canonical JSON payload posted to the gateway. The `data` field is
/// event-specific: it's empty `{}` for offline/revoked, and carries
/// extra fields for heartbeat (`sentAtMs`, `presenceTtlSeconds`).
///
/// The signature is computed over the exact bytes that
/// `serde_json::to_vec(&WebhookPayload)` produces, so any round-tripping
/// (deserialize → re-serialize) that reorders fields will invalidate it.
/// The store stores this struct serialized as JSONB; the worker reads
/// it out and re-serializes, which is stable because `serde_json`
/// preserves the declared field order on structs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookPayload {
    #[serde(rename = "eventId")]
    pub event_id: String,
    #[serde(rename = "eventType")]
    pub event_type: String,
    #[serde(rename = "deviceId")]
    pub device_id: String,
    #[serde(rename = "externalUserId", skip_serializing_if = "Option::is_none")]
    pub external_user_id: Option<String>,
    #[serde(rename = "occurredAt")]
    pub occurred_at: chrono::DateTime<Utc>,
    pub data: serde_json::Value,
}

/// Tunable runtime config passed to the background worker.
#[derive(Debug, Clone)]
pub struct WebhookConfig {
    pub url: String,
    pub secret: String,
    pub max_retries: u32,
    pub max_concurrency: usize,
    pub dlq_path: std::path::PathBuf,
    /// Per-request HTTP timeout for the POST. Spec § 2.2.4 mandates
    /// 5000ms; `AppState::from_config` feeds this value from
    /// `AHAND_HUB_WEBHOOK_TIMEOUT_MS`.
    pub request_timeout: Duration,
}

impl WebhookConfig {
    /// Default HTTP timeout for POSTs. Matches spec § 2.2.4
    /// (`AHAND_HUB_WEBHOOK_TIMEOUT_MS=5000`) and the deployed
    /// `task-definition.json` default. Tests that don't care about the
    /// exact value can use this constant without pulling `Duration`
    /// into the call site.
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(5_000);
}

/// The outbound webhook handle. Cheap to `clone` (internally
/// reference-counted); callers should just clone the `Arc<Webhook>`
/// stashed on `AppState`.
pub struct Webhook {
    inner: Option<WebhookInner>,
}

struct WebhookInner {
    store: Arc<dyn WebhookDeliveryStore>,
    notify: Arc<Notify>,
    // The config/semaphore/http are owned by the background worker
    // once it's spawned; they're kept here only so `inner_for_tests`
    // can hand them back to white-box tests without spinning up a
    // second worker. Production code doesn't read them after the
    // worker is spawned, but holding the `Arc`s here keeps the
    // structure complete for future features (e.g. runtime
    // reconfiguration).
    #[allow(dead_code)]
    config: Arc<WebhookConfig>,
    #[allow(dead_code)]
    semaphore: Arc<Semaphore>,
    #[allow(dead_code)]
    http: reqwest::Client,
}

impl Webhook {
    /// Build an active webhook. The returned `Webhook` is paired with
    /// a [`worker::WorkerHandle`] the caller should spawn via
    /// `tokio::spawn(handle.run())` once the rest of the app state is
    /// assembled.
    pub fn new(
        store: Arc<dyn WebhookDeliveryStore>,
        config: WebhookConfig,
    ) -> (Arc<Self>, worker::WorkerHandle) {
        let http = reqwest::Client::builder()
            .timeout(config.request_timeout)
            .user_agent(concat!("ahand-hub-webhook/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("build reqwest client");
        Self::new_with_client(store, config, http)
    }

    /// Same as [`Self::new`] but with an injected HTTP client. Tests
    /// use this to swap in a client that talks to a mock gateway;
    /// production should prefer [`Self::new`].
    pub fn new_with_client(
        store: Arc<dyn WebhookDeliveryStore>,
        config: WebhookConfig,
        http: reqwest::Client,
    ) -> (Arc<Self>, worker::WorkerHandle) {
        let notify = Arc::new(Notify::new());
        let semaphore = Arc::new(Semaphore::new(config.max_concurrency.max(1)));
        let config = Arc::new(config);
        let webhook = Arc::new(Self {
            inner: Some(WebhookInner {
                store: store.clone(),
                notify: notify.clone(),
                config: config.clone(),
                semaphore: semaphore.clone(),
                http: http.clone(),
            }),
        });
        let handle = worker::WorkerHandle::new(store, notify, config, semaphore, http);
        (webhook, handle)
    }

    /// No-op webhook. All `enqueue_*` calls return immediately. Used
    /// when `webhook_url` is unset (memory-mode, local dev without a
    /// gateway) so the handshake / admin paths don't need to branch
    /// on Option themselves.
    pub fn disabled() -> Arc<Self> {
        Arc::new(Self { inner: None })
    }

    /// Returns `true` when the webhook is active (URL + secret
    /// configured). Callers with expensive pre-serialization work
    /// (e.g. heartbeat emitters on a hot path) can skip it when this
    /// returns `false`.
    pub fn is_enabled(&self) -> bool {
        self.inner.is_some()
    }

    /// Enqueue a `device.online` event. Noops when disabled.
    pub async fn enqueue_online(
        &self,
        device_id: &str,
        external_user_id: Option<&str>,
    ) -> anyhow::Result<()> {
        self.enqueue_typed(
            "device.online",
            device_id,
            external_user_id,
            serde_json::json!({}),
        )
        .await
    }

    /// Enqueue a `device.offline` event. Noops when disabled.
    pub async fn enqueue_offline(
        &self,
        device_id: &str,
        external_user_id: Option<&str>,
    ) -> anyhow::Result<()> {
        self.enqueue_typed(
            "device.offline",
            device_id,
            external_user_id,
            serde_json::json!({}),
        )
        .await
    }

    /// Enqueue a `device.heartbeat` event. `sent_at_ms` is the
    /// daemon-supplied millisecond timestamp; `presence_ttl_seconds`
    /// is the hub's hint (expected cadence × 3) so consumers can
    /// reason about staleness without tracking cadence themselves.
    pub async fn enqueue_heartbeat(
        &self,
        device_id: &str,
        external_user_id: Option<&str>,
        sent_at_ms: u64,
        presence_ttl_seconds: u64,
    ) -> anyhow::Result<()> {
        self.enqueue_typed(
            "device.heartbeat",
            device_id,
            external_user_id,
            serde_json::json!({
                "sentAtMs": sent_at_ms,
                "presenceTtlSeconds": presence_ttl_seconds,
            }),
        )
        .await
    }

    /// Enqueue a `device.registered` event. Emitted the first time a
    /// daemon's hello lands against a pre-registered device row. Noops
    /// when disabled.
    pub async fn enqueue_registered(
        &self,
        device_id: &str,
        external_user_id: Option<&str>,
    ) -> anyhow::Result<()> {
        self.enqueue_typed(
            "device.registered",
            device_id,
            external_user_id,
            serde_json::json!({}),
        )
        .await
    }

    /// Enqueue a `device.revoked` event. Admin DELETE calls this
    /// after removing the row; `external_user_id` is best-effort
    /// (None if the row was anonymous). Noops when disabled.
    pub async fn enqueue_revoked(
        &self,
        device_id: &str,
        external_user_id: Option<&str>,
    ) -> anyhow::Result<()> {
        self.enqueue_typed(
            "device.revoked",
            device_id,
            external_user_id,
            serde_json::json!({}),
        )
        .await
    }

    async fn enqueue_typed(
        &self,
        event_type: &str,
        device_id: &str,
        external_user_id: Option<&str>,
        data: serde_json::Value,
    ) -> anyhow::Result<()> {
        let Some(inner) = self.inner.as_ref() else {
            return Ok(());
        };
        let payload = WebhookPayload {
            event_id: ulid::Ulid::new().to_string(),
            event_type: event_type.to_string(),
            device_id: device_id.to_string(),
            external_user_id: external_user_id.map(str::to_string),
            occurred_at: Utc::now(),
            data,
        };
        let delivery = WebhookDelivery {
            event_id: payload.event_id.clone(),
            payload: serde_json::to_value(&payload)?,
            attempts: 0,
            next_retry_at: Utc::now(),
            last_error: None,
            created_at: Utc::now(),
        };
        inner.store.enqueue(delivery).await?;
        inner.notify.notify_one();
        Ok(())
    }

    /// Expose the store for integration tests and operational tooling
    /// that want to assert on the rows remaining after a worker tick.
    /// Production callers should treat this as opaque — the store is
    /// an implementation detail of the dispatcher.
    pub fn store(&self) -> Option<Arc<dyn WebhookDeliveryStore>> {
        self.inner.as_ref().map(|inner| inner.store.clone())
    }
}
