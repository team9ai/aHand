//! Background worker that drains the `webhook_deliveries` queue.
//!
//! Lifecycle:
//!
//! 1. `AppState::from_config` constructs a [`super::Webhook`] +
//!    [`WorkerHandle`]. The state takes ownership of the webhook; the
//!    handle gets passed to `tokio::spawn(handle.run())`.
//! 2. Each iteration the worker leases up to `LEASE_BATCH` rows whose
//!    `next_retry_at <= now`. For every leased row it acquires a
//!    permit from the per-webhook `Semaphore(max_concurrency)` and
//!    spawns a `send_one` task.
//! 3. Between iterations it waits on either the `Notify` (a freshly
//!    enqueued row) or a sleep until the next due row, whichever
//!    fires first.
//!
//! The DLQ is a plain JSONL file at `<audit_fallback_path sibling>/
//! webhook_dlq.jsonl`. The plan shares the same `audit_fallback_path`
//! for both audit and webhook DLQ, so we derive a sibling filename
//! rather than insisting on a separate env var.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ahand_hub_store::webhook_delivery_store::{WebhookDelivery, WebhookDeliveryStore};
use chrono::Utc;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Notify, Semaphore};

use super::WebhookConfig;
use super::sender::{SendOutcome, send_once};

const LEASE_BATCH: usize = 100;
/// Ceiling on the idle sleep when nothing is due. Caps the worst-case
/// startup latency if the `earliest_next_retry` query lies
/// momentarily.
const MAX_IDLE_SLEEP: Duration = Duration::from_secs(30);

/// Handle the caller spawns as the background worker. Running
/// [`Self::run`] loops forever; the returned future exits only when
/// [`Self::shutdown`] is invoked or the store is dropped.
pub struct WorkerHandle {
    store: Arc<dyn WebhookDeliveryStore>,
    notify: Arc<Notify>,
    config: Arc<WebhookConfig>,
    semaphore: Arc<Semaphore>,
    http: reqwest::Client,
    shutdown: Arc<tokio::sync::Notify>,
}

impl WorkerHandle {
    pub(crate) fn new(
        store: Arc<dyn WebhookDeliveryStore>,
        notify: Arc<Notify>,
        config: Arc<WebhookConfig>,
        semaphore: Arc<Semaphore>,
        http: reqwest::Client,
    ) -> Self {
        Self {
            store,
            notify,
            config,
            semaphore,
            http,
            shutdown: Arc::new(Notify::new()),
        }
    }

    /// Signal the worker to exit at the next iteration boundary.
    /// Useful for tests; the production hub keeps the worker alive
    /// for the process lifetime.
    pub fn shutdown_signal(&self) -> Arc<Notify> {
        self.shutdown.clone()
    }

    /// Drive the worker loop until shutdown is signalled.
    pub async fn run(self) {
        let WorkerHandle {
            store,
            notify,
            config,
            semaphore,
            http,
            shutdown,
        } = self;

        loop {
            // 1. Lease any due rows and fire off send tasks.
            let leased = match store.lease_due(Utc::now(), LEASE_BATCH).await {
                Ok(leased) => leased,
                Err(err) => {
                    tracing::warn!(error = %err, "webhook worker: lease failed");
                    Vec::new()
                }
            };

            for delivery in leased {
                let permit = match semaphore.clone().acquire_owned().await {
                    Ok(permit) => permit,
                    Err(_) => {
                        // Semaphore closed — shutdown path.
                        return;
                    }
                };
                let store = store.clone();
                let config = config.clone();
                let http = http.clone();
                let notify = notify.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    send_and_handle(store, config, http, delivery).await;
                    // Wake the worker up so it re-checks the next-due
                    // timestamp immediately. Without this nudge the
                    // worker may be sleeping on a stale (pre-mark_failed)
                    // earliest-next-retry snapshot.
                    notify.notify_one();
                });
            }

            // 2. Wait for the next trigger: a new enqueue (notify),
            //    shutdown, or the next-due timestamp.
            let sleep_for = match store.earliest_next_retry().await {
                Ok(Some(ts)) => {
                    let delta = ts - Utc::now();
                    if delta.num_milliseconds() <= 0 {
                        Duration::from_millis(0)
                    } else {
                        Duration::from_millis(delta.num_milliseconds() as u64)
                            .min(MAX_IDLE_SLEEP)
                    }
                }
                Ok(None) => MAX_IDLE_SLEEP,
                Err(err) => {
                    tracing::warn!(error = %err, "webhook worker: earliest_next_retry failed");
                    Duration::from_secs(1)
                }
            };

            if sleep_for.is_zero() {
                continue;
            }

            // tokio::Notify coalesces multiple notify_one() calls into a single
            // stored notification. If this select is not yet awaiting (e.g., while
            // the for-loop is processing a batch), the notification is stored and
            // consumed on the next iteration. This means all rows enqueued during
            // a batch pass are correctly processed on the following wake.
            tokio::select! {
                _ = notify.notified() => {}
                _ = shutdown.notified() => return,
                _ = tokio::time::sleep(sleep_for) => {}
            }
        }
    }
}

async fn send_and_handle(
    store: Arc<dyn WebhookDeliveryStore>,
    config: Arc<WebhookConfig>,
    http: reqwest::Client,
    delivery: WebhookDelivery,
) {
    let payload: super::WebhookPayload =
        match serde_json::from_value(delivery.payload.clone()) {
            Ok(value) => value,
            Err(err) => {
                tracing::error!(
                    event_id = %delivery.event_id,
                    error = %err,
                    "webhook worker: corrupt payload, moving to DLQ",
                );
                dlq_and_delete(
                    store.as_ref(),
                    &config.dlq_path,
                    &delivery,
                    &format!("payload decode failed: {err}"),
                )
                .await;
                return;
            }
        };

    let outcome = send_once(&http, &config.url, config.secret.as_bytes(), &payload).await;
    match outcome {
        SendOutcome::Success => {
            if let Err(err) = store.delete(&delivery.event_id).await {
                tracing::warn!(
                    event_id = %delivery.event_id,
                    error = %err,
                    "webhook worker: failed to delete delivered row",
                );
            }
        }
        SendOutcome::PermanentFailure { reason } => {
            tracing::warn!(
                event_id = %delivery.event_id,
                reason = %reason,
                "webhook worker: permanent failure, moving to DLQ",
            );
            dlq_and_delete(store.as_ref(), &config.dlq_path, &delivery, &reason).await;
        }
        SendOutcome::RetryLater { reason } => {
            let next_attempts = delivery.attempts + 1;
            if (next_attempts as u32) >= config.max_retries {
                tracing::warn!(
                    event_id = %delivery.event_id,
                    attempts = next_attempts,
                    reason = %reason,
                    "webhook worker: retries exhausted, moving to DLQ",
                );
                dlq_and_delete(store.as_ref(), &config.dlq_path, &delivery, &reason).await;
                return;
            }
            let backoff =
                super::sender::backoff_secs(next_attempts as u32);
            let next_retry_at = Utc::now() + chrono::Duration::seconds(backoff as i64);
            if let Err(err) = store
                .mark_failed(&delivery.event_id, next_retry_at, next_attempts, &reason)
                .await
            {
                tracing::warn!(
                    event_id = %delivery.event_id,
                    error = %err,
                    "webhook worker: failed to record retry",
                );
            }
        }
    }
}

async fn dlq_and_delete(
    store: &dyn WebhookDeliveryStore,
    dlq_path: &PathBuf,
    delivery: &WebhookDelivery,
    reason: &str,
) {
    match append_dlq(dlq_path, delivery, reason).await {
        Ok(()) => {
            // DLQ write succeeded — safe to remove from retry queue.
            if let Err(err) = store.delete(&delivery.event_id).await {
                tracing::error!(
                    event_id = %delivery.event_id,
                    error = %err,
                    "webhook worker: failed to delete after DLQ",
                );
            }
        }
        Err(err) => {
            // DLQ write failed — leave row in retry queue so the
            // operator can recover. Deleting here would silently lose
            // the event with no durable record of the failure.
            tracing::error!(
                event_id = %delivery.event_id,
                error = %err,
                path = %dlq_path.display(),
                "webhook worker: DLQ write failed; leaving row in webhook_deliveries for manual recovery",
            );
            // Do not delete — at-least-once semantics preserved.
        }
    }
}

async fn append_dlq(
    path: &PathBuf,
    delivery: &WebhookDelivery,
    reason: &str,
) -> anyhow::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent).await?;
    }
    let line = serde_json::to_string(&serde_json::json!({
        "eventId": delivery.event_id,
        "payload": delivery.payload,
        "attempts": delivery.attempts,
        "lastError": reason,
        "createdAt": delivery.created_at,
    }))?;
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    file.write_all(line.as_bytes()).await?;
    file.write_all(b"\n").await?;
    Ok(())
}

/// Derive the DLQ file path from the shared `audit_fallback_path`.
/// Given `/var/lib/ahand-hub/audit-fallback.jsonl`, returns
/// `/var/lib/ahand-hub/webhook_dlq.jsonl`. If the input is a file
/// (no parent), returns `webhook_dlq.jsonl` in the cwd-equivalent.
pub fn dlq_path_from_audit_fallback(audit_fallback: &PathBuf) -> PathBuf {
    match audit_fallback.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join("webhook_dlq.jsonl"),
        _ => PathBuf::from("webhook_dlq.jsonl"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dlq_path_uses_sibling_of_audit_fallback() {
        let audit = PathBuf::from("/var/lib/ahand-hub/audit-fallback.jsonl");
        assert_eq!(
            dlq_path_from_audit_fallback(&audit),
            PathBuf::from("/var/lib/ahand-hub/webhook_dlq.jsonl"),
        );
    }

    #[test]
    fn dlq_path_bare_filename_defaults_to_cwd() {
        assert_eq!(
            dlq_path_from_audit_fallback(&PathBuf::from("audit-fallback.jsonl")),
            PathBuf::from("webhook_dlq.jsonl"),
        );
    }
}
