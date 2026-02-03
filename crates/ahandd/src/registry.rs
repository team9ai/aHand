use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use tokio::sync::{mpsc, Mutex, OwnedSemaphorePermit, Semaphore};
use tracing::{info, warn};

/// Handle kept per running job, used to send a cancel signal.
struct JobHandle {
    cancel_tx: mpsc::Sender<()>,
}

/// Cached result for a completed job (for idempotency).
#[derive(Clone)]
pub struct CompletedJob {
    pub exit_code: i32,
    pub error: String,
}

/// Result of checking whether a job_id is known.
pub enum IsKnown {
    /// Job is currently running.
    Running,
    /// Job already completed with this result.
    Completed(CompletedJob),
    /// Job is unknown (safe to start).
    Unknown,
}

/// Tracks running jobs, enforces concurrency limits, and caches completed
/// job results for idempotency.
pub struct JobRegistry {
    jobs: Mutex<HashMap<String, JobHandle>>,
    semaphore: Arc<Semaphore>,
    completed: Mutex<VecDeque<(String, CompletedJob)>>,
    max_completed: usize,
}

impl JobRegistry {
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            jobs: Mutex::new(HashMap::new()),
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            completed: Mutex::new(VecDeque::new()),
            max_completed: 1000,
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

    /// Remove a completed job from the running set.
    pub async fn remove(&self, job_id: &str) {
        let mut jobs = self.jobs.lock().await;
        jobs.remove(job_id);
    }

    /// Check if a job_id is already known (running or completed).
    pub async fn is_known(&self, job_id: &str) -> IsKnown {
        let jobs = self.jobs.lock().await;
        if jobs.contains_key(job_id) {
            return IsKnown::Running;
        }
        drop(jobs);

        let completed = self.completed.lock().await;
        for (id, result) in completed.iter() {
            if id == job_id {
                return IsKnown::Completed(result.clone());
            }
        }

        IsKnown::Unknown
    }

    /// Record a completed job for idempotency. Evicts the oldest entry
    /// when over capacity.
    pub async fn mark_completed(&self, job_id: String, exit_code: i32, error: String) {
        let mut completed = self.completed.lock().await;
        completed.push_back((job_id, CompletedJob { exit_code, error }));
        while completed.len() > self.max_completed {
            completed.pop_front();
        }
    }

    /// Number of currently running jobs.
    pub async fn active_count(&self) -> usize {
        let jobs = self.jobs.lock().await;
        jobs.len()
    }
}
