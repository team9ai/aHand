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
        .enqueue_online("device-1", Some("user-1"), &[])
        .await
        .unwrap();
    webhook.enqueue_offline("device-1", None).await.unwrap();
    webhook
        .enqueue_heartbeat("device-1", None, 1_000_000, 180)
        .await
        .unwrap();
    webhook
        .enqueue_registered("device-1", Some("user-1"), &[])
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
        .enqueue_online("device-1", Some("user-1"), &[])
        .await
        .unwrap();
    webhook
        .enqueue_heartbeat("device-1", Some("user-1"), 42, 180)
        .await
        .unwrap();

    assert_eq!(store.pending_count().await.unwrap(), 2);
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

#[tokio::test]
async fn online_event_serializes_capabilities() {
    let store: Arc<dyn ahand_hub_store::webhook_delivery_store::WebhookDeliveryStore> =
        Arc::new(MemoryWebhookDeliveryStore::new());
    let (webhook, handle) = Webhook::new(store.clone(), noop_config());
    drop(handle);

    let caps = vec!["exec".to_string(), "browser".to_string()];
    webhook
        .enqueue_online("device-1", Some("user-1"), &caps)
        .await
        .unwrap();

    let rows = store.lease_due(chrono::Utc::now(), 10).await.unwrap();
    assert_eq!(rows.len(), 1);
    let payload: WebhookPayload = serde_json::from_value(rows[0].payload.clone()).unwrap();
    assert_eq!(payload.event_type, "device.online");
    assert_eq!(
        payload.data["capabilities"],
        serde_json::json!(["exec", "browser"])
    );
}

#[tokio::test]
async fn registered_event_serializes_capabilities() {
    let store: Arc<dyn ahand_hub_store::webhook_delivery_store::WebhookDeliveryStore> =
        Arc::new(MemoryWebhookDeliveryStore::new());
    let (webhook, handle) = Webhook::new(store.clone(), noop_config());
    drop(handle);

    let caps = vec!["exec".to_string(), "browser".to_string()];
    webhook
        .enqueue_registered("device-2", Some("user-2"), &caps)
        .await
        .unwrap();

    let rows = store.lease_due(chrono::Utc::now(), 10).await.unwrap();
    assert_eq!(rows.len(), 1);
    let payload: WebhookPayload = serde_json::from_value(rows[0].payload.clone()).unwrap();
    assert_eq!(payload.event_type, "device.registered");
    assert_eq!(
        payload.data["capabilities"],
        serde_json::json!(["exec", "browser"])
    );
}

#[tokio::test]
async fn online_event_serializes_empty_capabilities_when_none_declared() {
    let store: Arc<dyn ahand_hub_store::webhook_delivery_store::WebhookDeliveryStore> =
        Arc::new(MemoryWebhookDeliveryStore::new());
    let (webhook, handle) = Webhook::new(store.clone(), noop_config());
    drop(handle);

    webhook
        .enqueue_online("device-3", None, &[])
        .await
        .unwrap();

    let rows = store.lease_due(chrono::Utc::now(), 10).await.unwrap();
    assert_eq!(rows.len(), 1);
    let payload: WebhookPayload = serde_json::from_value(rows[0].payload.clone()).unwrap();
    assert_eq!(payload.event_type, "device.online");
    assert_eq!(payload.data["capabilities"], serde_json::json!([]));
}

#[tokio::test]
async fn heartbeat_data_unaffected_by_capabilities_change() {
    // Regression guard: heartbeat must not carry capabilities.
    let store: Arc<dyn ahand_hub_store::webhook_delivery_store::WebhookDeliveryStore> =
        Arc::new(MemoryWebhookDeliveryStore::new());
    let (webhook, handle) = Webhook::new(store.clone(), noop_config());
    drop(handle);

    webhook
        .enqueue_heartbeat("device-1", None, 42, 90)
        .await
        .unwrap();

    let rows = store.lease_due(chrono::Utc::now(), 10).await.unwrap();
    let payload: WebhookPayload = serde_json::from_value(rows[0].payload.clone()).unwrap();
    assert_eq!(payload.event_type, "device.heartbeat");
    assert!(
        payload.data.get("capabilities").is_none(),
        "heartbeat must not include capabilities: {:?}",
        payload.data
    );
}

#[tokio::test]
async fn offline_event_unaffected_by_capabilities_change() {
    // Regression guard: offline must not carry capabilities.
    let store: Arc<dyn ahand_hub_store::webhook_delivery_store::WebhookDeliveryStore> =
        Arc::new(MemoryWebhookDeliveryStore::new());
    let (webhook, handle) = Webhook::new(store.clone(), noop_config());
    drop(handle);

    webhook
        .enqueue_offline("device-1", Some("user-1"))
        .await
        .unwrap();

    let rows = store.lease_due(chrono::Utc::now(), 10).await.unwrap();
    let payload: WebhookPayload = serde_json::from_value(rows[0].payload.clone()).unwrap();
    assert_eq!(payload.event_type, "device.offline");
    assert!(
        payload.data.get("capabilities").is_none(),
        "offline must not include capabilities: {:?}",
        payload.data
    );
}

#[tokio::test]
async fn revoked_event_unaffected_by_capabilities_change() {
    // Regression guard: revoked must not carry capabilities.
    let store: Arc<dyn ahand_hub_store::webhook_delivery_store::WebhookDeliveryStore> =
        Arc::new(MemoryWebhookDeliveryStore::new());
    let (webhook, handle) = Webhook::new(store.clone(), noop_config());
    drop(handle);

    webhook
        .enqueue_revoked("device-1", None)
        .await
        .unwrap();

    let rows = store.lease_due(chrono::Utc::now(), 10).await.unwrap();
    let payload: WebhookPayload = serde_json::from_value(rows[0].payload.clone()).unwrap();
    assert_eq!(payload.event_type, "device.revoked");
    assert!(
        payload.data.get("capabilities").is_none(),
        "revoked must not include capabilities: {:?}",
        payload.data
    );
}
