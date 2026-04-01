use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use ahand_hub_core::HubError;
use async_stream::stream;
use axum::response::sse::Event;
use dashmap::DashMap;
use futures_util::Stream;
use tokio::sync::{Mutex, broadcast};

#[derive(Clone)]
enum OutputItem {
    Stdout(String),
    Stderr(String),
    Progress(u32),
    Finished { exit_code: i32, error: String },
}

impl OutputItem {
    fn to_event(&self) -> Event {
        match self {
            Self::Stdout(chunk) => Event::default().event("stdout").data(chunk.clone()),
            Self::Stderr(chunk) => Event::default().event("stderr").data(chunk.clone()),
            Self::Progress(progress) => Event::default()
                .event("progress")
                .data(progress.to_string()),
            Self::Finished { exit_code, error } => Event::default()
                .event("finished")
                .data(serde_json::json!({ "exit_code": exit_code, "error": error }).to_string()),
        }
    }

    fn is_terminal(&self) -> bool {
        matches!(self, Self::Finished { .. })
    }
}

#[derive(Clone)]
struct SequencedOutputItem {
    seq: u64,
    item: OutputItem,
}

struct JobOutputState {
    history: Mutex<Vec<SequencedOutputItem>>,
    tx: broadcast::Sender<SequencedOutputItem>,
    finished: AtomicBool,
    next_seq: AtomicU64,
}

impl JobOutputState {
    fn new() -> Self {
        let (tx, _) = broadcast::channel(64);
        Self {
            history: Mutex::new(Vec::new()),
            tx,
            finished: AtomicBool::new(false),
            next_seq: AtomicU64::new(0),
        }
    }
}

#[derive(Clone)]
pub struct OutputStream {
    jobs: Arc<DashMap<String, Arc<JobOutputState>>>,
    finished_retention: Duration,
    max_history: usize,
}

impl OutputStream {
    pub fn new(finished_retention: Duration, max_history: usize) -> Self {
        Self {
            jobs: Arc::new(DashMap::new()),
            finished_retention,
            max_history: max_history.max(1),
        }
    }

    pub fn prime(&self, job_id: &str) {
        self.prune_expired();
        self.job_state(job_id);
    }

    pub async fn subscribe(
        &self,
        job_id: String,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>> {
        self.prune_expired();
        let state = self
            .jobs
            .get(&job_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| HubError::JobNotFound(job_id.clone()))?;
        let finished = state.finished.load(Ordering::Relaxed);
        let mut rx = state.tx.subscribe();
        let history = state.history.lock().await.clone();
        let mut last_seq = history.last().map(|entry| entry.seq).unwrap_or(0);

        Ok(Box::pin(stream! {
            for item in history {
                let is_terminal = item.item.is_terminal();
                yield Ok(item.item.to_event());
                if is_terminal {
                    return;
                }
            }

            if finished {
                return;
            }

            while let Ok(item) = rx.recv().await {
                if item.seq <= last_seq {
                    continue;
                }
                last_seq = item.seq;
                let is_terminal = item.item.is_terminal();
                yield Ok(item.item.to_event());
                if is_terminal {
                    break;
                }
            }
        }))
    }

    pub async fn push_stdout(&self, job_id: &str, chunk: Vec<u8>) -> anyhow::Result<()> {
        self.record(
            job_id,
            OutputItem::Stdout(String::from_utf8_lossy(&chunk).to_string()),
        )
        .await
    }

    pub async fn push_stderr(&self, job_id: &str, chunk: Vec<u8>) -> anyhow::Result<()> {
        self.record(
            job_id,
            OutputItem::Stderr(String::from_utf8_lossy(&chunk).to_string()),
        )
        .await
    }

    pub async fn push_progress(&self, job_id: &str, progress: u32) -> anyhow::Result<()> {
        self.record(job_id, OutputItem::Progress(progress)).await
    }

    pub async fn push_finished(
        &self,
        job_id: &str,
        exit_code: i32,
        error: &str,
    ) -> anyhow::Result<()> {
        self.record(
            job_id,
            OutputItem::Finished {
                exit_code,
                error: error.into(),
            },
        )
        .await
    }

    async fn record(&self, job_id: &str, item: OutputItem) -> anyhow::Result<()> {
        self.prune_expired();
        let state = self.job_state(job_id);
        let sequenced = SequencedOutputItem {
            seq: state.next_seq.fetch_add(1, Ordering::Relaxed) + 1,
            item,
        };
        {
            let mut history = state.history.lock().await;
            history.push(sequenced.clone());
            if history.len() > self.max_history {
                let excess = history.len() - self.max_history;
                history.drain(0..excess);
            }
        }
        if sequenced.item.is_terminal() {
            state.finished.store(true, Ordering::Relaxed);
            let output_stream = self.clone();
            let job_id = job_id.to_string();
            tokio::spawn(async move {
                tokio::time::sleep(output_stream.finished_retention).await;
                output_stream.jobs.remove(&job_id);
            });
        }
        let _ = state.tx.send(sequenced);
        Ok(())
    }

    fn job_state(&self, job_id: &str) -> Arc<JobOutputState> {
        self.jobs
            .entry(job_id.into())
            .or_insert_with(|| Arc::new(JobOutputState::new()))
            .clone()
    }

    fn prune_expired(&self) {}
}

impl Default for OutputStream {
    fn default() -> Self {
        Self::new(Duration::from_secs(60 * 60), 256)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures_util::StreamExt;

    use super::*;

    #[tokio::test]
    async fn finished_jobs_are_evicted_from_history() {
        let stream = OutputStream::new(Duration::from_millis(25), 16);
        stream
            .push_stdout("job-1", b"hello\n".to_vec())
            .await
            .unwrap();
        stream.push_finished("job-1", 0, "").await.unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(
            !stream.jobs.contains_key("job-1"),
            "terminal job history should be evicted"
        );
    }

    #[tokio::test]
    async fn subscribe_replays_without_dropping_gap_events() {
        let stream = OutputStream::new(Duration::from_secs(60), 16);
        stream
            .push_stdout("job-1", b"before\n".to_vec())
            .await
            .unwrap();

        let stream_clone = stream.clone();
        let subscriber = tokio::spawn(async move {
            let mut output = stream_clone.subscribe("job-1".into()).await.unwrap();
            let mut body = Vec::new();
            while let Some(Ok(event)) = output.next().await {
                body.push(format!("{event:?}"));
                if body.len() == 2 {
                    break;
                }
            }
            body
        });

        tokio::time::sleep(Duration::from_millis(10)).await;
        stream
            .push_stdout("job-1", b"after\n".to_vec())
            .await
            .unwrap();

        let events = subscriber.await.unwrap();
        assert_eq!(events.len(), 2);
    }
}
