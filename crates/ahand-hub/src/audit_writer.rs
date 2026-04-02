use std::sync::Arc;

use ahand_hub_core::audit::AuditEntry;
use ahand_hub_core::traits::AuditStore;
use tokio::sync::mpsc;

pub async fn run_audit_writer(
    store: Arc<dyn AuditStore>,
    mut rx: mpsc::UnboundedReceiver<AuditEntry>,
) {
    while let Some(entry) = rx.recv().await {
        let mut batch = vec![entry];
        while batch.len() < 100 {
            match rx.try_recv() {
                Ok(next) => batch.push(next),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
            }
        }
        let _ = store.append(&batch).await;
    }
}
