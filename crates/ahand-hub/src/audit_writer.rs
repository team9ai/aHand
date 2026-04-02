use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ahand_hub_core::audit::{AuditEntry, AuditFilter};
use ahand_hub_core::traits::AuditStore;
use ahand_hub_core::{HubError, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const AUDIT_BATCH_SIZE: usize = 100;
const AUDIT_FLUSH_INTERVAL: Duration = Duration::from_millis(500);
const AUDIT_QUEUE_CAPACITY: usize = 1024;

#[derive(Clone)]
pub struct BufferedAuditStore {
    inner: Arc<dyn AuditStore>,
    writer: Arc<WriterState>,
    fallback_path: Arc<PathBuf>,
    fallback_lock: Arc<tokio::sync::Mutex<()>>,
}

struct WriterState {
    tx: Mutex<Option<mpsc::Sender<AuditEntry>>>,
    task: tokio::sync::Mutex<Option<JoinHandle<()>>>,
}

impl BufferedAuditStore {
    pub fn new(inner: Arc<dyn AuditStore>) -> Self {
        Self::new_with_fallback_path(
            inner,
            std::env::temp_dir().join("ahand-hub-audit-fallback.jsonl"),
        )
    }

    pub fn new_with_fallback_path(inner: Arc<dyn AuditStore>, fallback_path: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel(AUDIT_QUEUE_CAPACITY);
        let fallback_path = Arc::new(fallback_path);
        let fallback_lock = Arc::new(tokio::sync::Mutex::new(()));
        let task = tokio::spawn(run_audit_writer(
            inner.clone(),
            rx,
            fallback_path.clone(),
            fallback_lock.clone(),
        ));
        Self {
            inner,
            writer: Arc::new(WriterState {
                tx: Mutex::new(Some(tx)),
                task: tokio::sync::Mutex::new(Some(task)),
            }),
            fallback_path,
            fallback_lock,
        }
    }

    pub async fn shutdown(&self) -> Result<()> {
        let sender = self
            .writer
            .tx
            .lock()
            .map_err(|err| HubError::Internal(err.to_string()))?
            .take();
        drop(sender);

        let task = self.writer.task.lock().await.take();
        if let Some(task) = task {
            task.await
                .map_err(|err| HubError::Internal(format!("audit writer join failed: {err}")))?;
        }

        Ok(())
    }

    async fn append_direct_or_fallback(&self, entries: &[AuditEntry]) -> Result<()> {
        if self.inner.append(entries).await.is_ok() {
            return Ok(());
        }

        tracing::error!(
            path = %self.fallback_path.display(),
            "audit queue unavailable, writing entry to fallback file"
        );
        write_fallback_entries(
            self.fallback_path.as_ref(),
            entries,
            self.fallback_lock.as_ref(),
        )
        .await
    }
}

#[async_trait]
impl AuditStore for BufferedAuditStore {
    async fn append(&self, entries: &[AuditEntry]) -> Result<()> {
        let sender = self
            .writer
            .tx
            .lock()
            .map_err(|err| HubError::Internal(err.to_string()))?
            .clone();
        let Some(tx) = sender else {
            return self.append_direct_or_fallback(entries).await;
        };

        for entry in entries {
            match tx.try_send(entry.clone()) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(entry)) => {
                    let fallback_path = self.fallback_path.clone();
                    let fallback_lock = self.fallback_lock.clone();
                    tokio::spawn(async move {
                        tracing::error!(
                            path = %fallback_path.display(),
                            "audit queue unavailable, writing entry to fallback file"
                        );
                        if let Err(err) = write_fallback_entries(
                            fallback_path.as_ref(),
                            std::slice::from_ref(&entry),
                            fallback_lock.as_ref(),
                        )
                        .await
                        {
                            tracing::error!(error = %err, path = %fallback_path.display(), "failed to write audit fallback entry");
                        }
                    });
                }
                Err(mpsc::error::TrySendError::Closed(entry)) => {
                    self.append_direct_or_fallback(std::slice::from_ref(&entry))
                        .await?;
                }
            }
        }
        Ok(())
    }

    async fn query(&self, filter: AuditFilter) -> Result<Vec<AuditEntry>> {
        self.inner.query(filter).await
    }

    async fn prune_before(&self, cutoff: DateTime<Utc>) -> Result<u64> {
        self.inner.prune_before(cutoff).await
    }
}

pub async fn run_audit_writer(
    store: Arc<dyn AuditStore>,
    mut rx: mpsc::Receiver<AuditEntry>,
    fallback_path: Arc<PathBuf>,
    fallback_lock: Arc<tokio::sync::Mutex<()>>,
) {
    let mut buffer = Vec::with_capacity(AUDIT_BATCH_SIZE);

    loop {
        let Some(entry) = rx.recv().await else {
            if !buffer.is_empty() {
                let _ = flush_batch(
                    store.as_ref(),
                    &buffer,
                    fallback_path.as_ref(),
                    fallback_lock.as_ref(),
                )
                .await;
            }
            break;
        };
        buffer.push(entry);

        let timer = tokio::time::sleep(AUDIT_FLUSH_INTERVAL);
        tokio::pin!(timer);

        while buffer.len() < AUDIT_BATCH_SIZE {
            tokio::select! {
                maybe_entry = rx.recv() => {
                    match maybe_entry {
                        Some(entry) => buffer.push(entry),
                        None => break,
                    }
                }
                _ = &mut timer => break,
            }
        }

        if let Err(err) = flush_batch(
            store.as_ref(),
            &buffer,
            fallback_path.as_ref(),
            fallback_lock.as_ref(),
        )
        .await
        {
            tracing::error!(error = %err, path = %fallback_path.display(), "failed to flush audit batch");
        }
        buffer.clear();
    }
}

async fn flush_batch(
    store: &dyn AuditStore,
    batch: &[AuditEntry],
    fallback_path: &PathBuf,
    fallback_lock: &tokio::sync::Mutex<()>,
) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }

    if store.append(batch).await.is_ok() {
        return Ok(());
    }

    tokio::time::sleep(Duration::from_millis(100)).await;
    if store.append(batch).await.is_ok() {
        return Ok(());
    }

    tracing::error!(
        path = %fallback_path.display(),
        "audit store remained unavailable after retry, writing fallback batch"
    );
    write_fallback_entries(fallback_path, batch, fallback_lock).await
}

async fn write_fallback_entries(
    path: &PathBuf,
    batch: &[AuditEntry],
    fallback_lock: &tokio::sync::Mutex<()>,
) -> Result<()> {
    let _guard = fallback_lock.lock().await;
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .map_err(|err| HubError::Internal(err.to_string()))?;

    let mut body = String::new();
    for entry in batch {
        body.push_str(
            &serde_json::to_string(entry).map_err(|err| HubError::Internal(err.to_string()))?,
        );
        body.push('\n');
    }
    file.write_all(body.as_bytes())
        .await
        .map_err(|err| HubError::Internal(err.to_string()))?;

    file.flush()
        .await
        .map_err(|err| HubError::Internal(err.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use chrono::Utc;

    use super::*;

    #[derive(Default)]
    struct RecordingAuditStore {
        entries: Mutex<Vec<AuditEntry>>,
    }

    #[async_trait]
    impl AuditStore for RecordingAuditStore {
        async fn append(&self, entries: &[AuditEntry]) -> Result<()> {
            self.entries
                .lock()
                .map_err(|err| HubError::Internal(err.to_string()))?
                .extend(entries.iter().cloned());
            Ok(())
        }

        async fn query(&self, filter: AuditFilter) -> Result<Vec<AuditEntry>> {
            let entries = self
                .entries
                .lock()
                .map_err(|err| HubError::Internal(err.to_string()))?;
            Ok(filter.apply(entries.iter().cloned()))
        }
    }

    struct FailingAuditStore;

    #[async_trait]
    impl AuditStore for FailingAuditStore {
        async fn append(&self, _entries: &[AuditEntry]) -> Result<()> {
            Err(HubError::Internal("append failed".into()))
        }

        async fn query(&self, _filter: AuditFilter) -> Result<Vec<AuditEntry>> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn buffered_store_flushes_entries_asynchronously() {
        let inner = Arc::new(RecordingAuditStore::default());
        let buffered = BufferedAuditStore::new(inner.clone());

        buffered
            .append(&[AuditEntry {
                timestamp: Utc::now(),
                action: "job.created".into(),
                resource_type: "job".into(),
                resource_id: "job-1".into(),
                actor: "service:test".into(),
                detail: serde_json::json!({ "tool": "echo" }),
                source_ip: None,
            }])
            .await
            .unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        loop {
            let entries = inner
                .query(AuditFilter {
                    action: Some("job.created".into()),
                    ..Default::default()
                })
                .await
                .unwrap();
            if !entries.is_empty() || tokio::time::Instant::now() >= deadline {
                assert_eq!(entries.len(), 1);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn buffered_store_falls_back_to_file_after_store_failure() {
        let fallback_path = std::env::temp_dir().join(format!(
            "ahand-hub-audit-fallback-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        let buffered = BufferedAuditStore::new_with_fallback_path(
            Arc::new(FailingAuditStore),
            fallback_path.clone(),
        );

        buffered
            .append(&[AuditEntry {
                timestamp: Utc::now(),
                action: "job.created".into(),
                resource_type: "job".into(),
                resource_id: "job-1".into(),
                actor: "service:test".into(),
                detail: serde_json::json!({ "tool": "echo" }),
                source_ip: None,
            }])
            .await
            .unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        loop {
            if tokio::fs::metadata(&fallback_path).await.is_ok() {
                let body = tokio::fs::read_to_string(&fallback_path).await.unwrap();
                assert!(body.contains("\"action\":\"job.created\""));
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("fallback file was not written");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let _ = tokio::fs::remove_file(fallback_path).await;
    }

    #[tokio::test]
    async fn buffered_store_shutdown_drains_pending_entries() {
        let inner = Arc::new(RecordingAuditStore::default());
        let buffered = BufferedAuditStore::new(inner.clone());

        buffered
            .append(&[AuditEntry {
                timestamp: Utc::now(),
                action: "job.created".into(),
                resource_type: "job".into(),
                resource_id: "job-1".into(),
                actor: "service:test".into(),
                detail: serde_json::json!({ "tool": "echo" }),
                source_ip: None,
            }])
            .await
            .unwrap();

        buffered.shutdown().await.unwrap();

        let entries = inner
            .query(AuditFilter {
                action: Some("job.created".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
    }
}
