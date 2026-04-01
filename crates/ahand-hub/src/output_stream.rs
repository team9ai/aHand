use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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

struct JobOutputState {
    history: Mutex<Vec<OutputItem>>,
    tx: broadcast::Sender<OutputItem>,
    finished: AtomicBool,
}

impl JobOutputState {
    fn new() -> Self {
        let (tx, _) = broadcast::channel(64);
        Self {
            history: Mutex::new(Vec::new()),
            tx,
            finished: AtomicBool::new(false),
        }
    }
}

#[derive(Default)]
pub struct OutputStream {
    jobs: DashMap<String, Arc<JobOutputState>>,
}

impl OutputStream {
    pub async fn subscribe(
        &self,
        job_id: String,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>> {
        let state = self.job_state(&job_id);
        let history = state.history.lock().await.clone();
        let finished = state.finished.load(Ordering::Relaxed);
        let mut rx = state.tx.subscribe();

        Ok(Box::pin(stream! {
            for item in history {
                let is_terminal = item.is_terminal();
                yield Ok(item.to_event());
                if is_terminal {
                    return;
                }
            }

            if finished {
                return;
            }

            while let Ok(item) = rx.recv().await {
                let is_terminal = item.is_terminal();
                yield Ok(item.to_event());
                if is_terminal {
                    break;
                }
            }
        }))
    }

    pub async fn push_stdout(&self, job_id: &str, chunk: Vec<u8>) -> anyhow::Result<()> {
        self.record(job_id, OutputItem::Stdout(String::from_utf8_lossy(&chunk).to_string()))
            .await
    }

    pub async fn push_stderr(&self, job_id: &str, chunk: Vec<u8>) -> anyhow::Result<()> {
        self.record(job_id, OutputItem::Stderr(String::from_utf8_lossy(&chunk).to_string()))
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
        let state = self.job_state(job_id);
        {
            let mut history = state.history.lock().await;
            history.push(item.clone());
        }
        if item.is_terminal() {
            state.finished.store(true, Ordering::Relaxed);
        }
        let _ = state.tx.send(item);
        Ok(())
    }

    fn job_state(&self, job_id: &str) -> Arc<JobOutputState> {
        self.jobs
            .entry(job_id.into())
            .or_insert_with(|| Arc::new(JobOutputState::new()))
            .clone()
    }
}
