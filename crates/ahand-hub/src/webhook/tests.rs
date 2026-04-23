//! Unit tests for [`Webhook`] helpers that don't require an HTTP
//! mock (see `tests/webhook_sender.rs` for end-to-end coverage).

use std::sync::Arc;

use ahand_hub_store::webhook_delivery_store::MemoryWebhookDeliveryStore;

use super::*;

fn noop_config() -> WebhookConfig {
    WebhookConfig {
        url: "http://127.0.0.1:1/webhook".into(),
        secret: "secret".into(),
        max_retries: 8,
        max_concurrency: 4,
        dlq_path: std::env::temp_dir().join("ahand-hub-webhook-dlq-test.jsonl"),
        request_timeout: WebhookConfig::DEFAULT_TIMEOUT,
    }
}

#[tokio::test]
async fn disabled_enqueue_returns_ok_without_persisting() {
    let webhook = Webhook::disabled();
    assert!(!webhook.is_enabled());
    webhook
        .enqueue_online("device-1", Some("user-1"))
        .await
        .unwrap();
    webhook.enqueue_offline("device-1", None).await.unwrap();
    webhook
        .enqueue_heartbeat("device-1", None, 1_000_000, 180)
        .await
        .unwrap();
    webhook
        .enqueue_registered("device-1", Some("user-1"))
        .await
        .unwrap();
    webhook.enqueue_revoked("device-1", None).await.unwrap();
}

#[tokio::test]
async fn enabled_enqueue_persists_and_notifies() {
    let store: Arc<dyn ahand_hub_store::webhook_delivery_store::WebhookDeliveryStore> =
        Arc::new(MemoryWebhookDeliveryStore::new());
    let (webhook, handle) = Webhook::new(store.clone(), noop_config());
    // Drop the handle so nothing tries to actually POST — the
    // enqueue should still persist and signal the notify.
    drop(handle);

    webhook
        .enqueue_online("device-1", Some("user-1"))
        .await
        .unwrap();
    webhook
        .enqueue_heartbeat("device-1", Some("user-1"), 42, 180)
        .await
        .unwrap();

    assert_eq!(store.len().await.unwrap(), 2);
}

#[tokio::test]
async fn payload_data_varies_by_event_type() {
    let store: Arc<dyn ahand_hub_store::webhook_delivery_store::WebhookDeliveryStore> =
        Arc::new(MemoryWebhookDeliveryStore::new());
    let (webhook, handle) = Webhook::new(store.clone(), noop_config());
    drop(handle);

    webhook
        .enqueue_heartbeat("device-1", None, 99, 180)
        .await
        .unwrap();
    let rows = store.lease_due(chrono::Utc::now(), 10).await.unwrap();
    assert_eq!(rows.len(), 1);
    let payload: WebhookPayload = serde_json::from_value(rows[0].payload.clone()).unwrap();
    assert_eq!(payload.event_type, "device.heartbeat");
    assert_eq!(payload.data["sentAtMs"], 99);
    assert_eq!(payload.data["presenceTtlSeconds"], 180);
    assert!(payload.external_user_id.is_none());
}

#[tokio::test]
async fn payload_skips_external_user_id_when_absent() {
    let store: Arc<dyn ahand_hub_store::webhook_delivery_store::WebhookDeliveryStore> =
        Arc::new(MemoryWebhookDeliveryStore::new());
    let (webhook, handle) = Webhook::new(store.clone(), noop_config());
    drop(handle);

    webhook.enqueue_offline("device-1", None).await.unwrap();
    let rows = store.lease_due(chrono::Utc::now(), 10).await.unwrap();
    let body = rows[0].payload.to_string();
    assert!(
        !body.contains("externalUserId"),
        "JSON must omit externalUserId when None: {body}",
    );
}

#[tokio::test]
async fn disabled_webhook_has_no_store() {
    let webhook = Webhook::disabled();
    assert!(webhook.store().is_none());
}
