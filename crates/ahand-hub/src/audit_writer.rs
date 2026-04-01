use std::sync::Arc;

use ahand_hub_core::audit::AuditEntry;
use ahand_hub_core::traits::AuditStore;
use tokio::sync::mpsc;

pub async fn run_audit_writer(store: Arc<dyn AuditStore>, mut rx: mpsc::Receiver<AuditEntry>) {
    let mut buffer = Vec::with_capacity(100);
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(500));

    loop {
        tokio::select! {
            maybe_entry = rx.recv() => {
                match maybe_entry {
                    Some(entry) => {
                        buffer.push(entry);
                        if buffer.len() >= 100 {
                            let batch = std::mem::take(&mut buffer);
                            let _ = store.append(&batch).await;
                        }
                    }
                    None => break,
                }
            }
            _ = ticker.tick() => {
                if !buffer.is_empty() {
                    let batch = std::mem::take(&mut buffer);
                    let _ = store.append(&batch).await;
                }
            }
        }
    }

    if !buffer.is_empty() {
        let _ = store.append(&buffer).await;
    }
}
