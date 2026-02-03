use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, Mutex, OwnedSemaphorePermit, Semaphore};
use tracing::{info, warn};

/// Handle kept per running job, used to send a cancel signal.
struct JobHandle {
    cancel_tx: mpsc::Sender<()>,
}

/// Tracks running jobs and enforces concurrency limits.
pub struct JobRegistry {
    jobs: Mutex<HashMap<String, JobHandle>>,
    semaphore: Arc<Semaphore>,
}

impl JobRegistry {
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            jobs: Mutex::new(HashMap::new()),
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
        }
    }

    /// Acquire a concurrency permit. Blocks until one is available.
    pub async fn acquire_permit(&self) -> OwnedSemaphorePermit {
        self.semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore closed")
    }

    /// Register a running job with its cancel sender.
    pub async fn register(&self, job_id: String, cancel_tx: mpsc::Sender<()>) {
        let mut jobs = self.jobs.lock().await;
        jobs.insert(job_id, JobHandle { cancel_tx });
    }

    /// Send a cancel signal to a running job.
    pub async fn cancel(&self, job_id: &str) {
        let jobs = self.jobs.lock().await;
        if let Some(handle) = jobs.get(job_id) {
            if handle.cancel_tx.send(()).await.is_ok() {
                info!(job_id = %job_id, "cancel signal sent");
            } else {
                warn!(job_id = %job_id, "cancel channel closed (job may have already finished)");
            }
        } else {
            warn!(job_id = %job_id, "job not found in registry");
        }
    }

    /// Remove a completed job from the registry.
    pub async fn remove(&self, job_id: &str) {
        let mut jobs = self.jobs.lock().await;
        jobs.remove(job_id);
    }

    /// Number of currently running jobs.
    pub async fn active_count(&self) -> usize {
        let jobs = self.jobs.lock().await;
        jobs.len()
    }
}
