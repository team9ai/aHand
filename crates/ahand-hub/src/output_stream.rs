use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use ahand_hub_store::job_output_store::{JobOutputRecord, RedisJobOutputStore};
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
    fn to_event(&self, seq: u64) -> Event {
        match self {
            Self::Stdout(chunk) => Event::default()
                .id(seq.to_string())
                .event("stdout")
                .data(chunk.clone()),
            Self::Stderr(chunk) => Event::default()
                .id(seq.to_string())
                .event("stderr")
                .data(chunk.clone()),
            Self::Progress(progress) => Event::default()
                .id(seq.to_string())
                .event("progress")
                .data(progress.to_string()),
            Self::Finished { exit_code, error } => Event::default()
                .id(seq.to_string())
                .event("finished")
                .data(serde_json::json!({ "exit_code": exit_code, "error": error }).to_string()),
        }
    }

    fn is_terminal(&self) -> bool {
        matches!(self, Self::Finished { .. })
    }
}

impl From<JobOutputRecord> for OutputItem {
    fn from(value: JobOutputRecord) -> Self {
        match value {
            JobOutputRecord::Stdout(chunk) => Self::Stdout(chunk),
            JobOutputRecord::Stderr(chunk) => Self::Stderr(chunk),
            JobOutputRecord::Progress(progress) => Self::Progress(progress),
            JobOutputRecord::Finished { exit_code, error } => Self::Finished { exit_code, error },
        }
    }
}

impl From<OutputItem> for JobOutputRecord {
    fn from(value: OutputItem) -> Self {
        match value {
            OutputItem::Stdout(chunk) => Self::Stdout(chunk),
            OutputItem::Stderr(chunk) => Self::Stderr(chunk),
            OutputItem::Progress(progress) => Self::Progress(progress),
            OutputItem::Finished { exit_code, error } => Self::Finished { exit_code, error },
        }
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
struct MemoryOutputStream {
    jobs: Arc<DashMap<String, Arc<JobOutputState>>>,
    finished_retention: Duration,
    max_history: usize,
}

impl MemoryOutputStream {
    fn new(finished_retention: Duration, max_history: usize) -> Self {
        Self {
            jobs: Arc::new(DashMap::new()),
            finished_retention,
            max_history: max_history.max(1),
        }
    }

    fn prime(&self, job_id: &str) {
        self.prune_expired();
        self.job_state(job_id);
    }

    async fn subscribe_from(
        &self,
        job_id: String,
        last_event_id: Option<u64>,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>> {
        self.prune_expired();
        let state = self.job_state(&job_id);
        let finished = state.finished.load(Ordering::Relaxed);
        let mut rx = state.tx.subscribe();
        let history = state.history.lock().await.clone();
        let mut last_seq = last_event_id.unwrap_or(0);
        let needs_resync = history
            .first()
            .and_then(|first| {
                last_event_id.map(|last_event_id| first.seq > last_event_id.saturating_add(1))
            })
            .unwrap_or(false);

        Ok(Box::pin(stream! {
            if needs_resync {
                yield Ok(resync_event("history_trimmed"));
            }

            for item in history {
                if item.seq <= last_seq {
                    continue;
                }
                last_seq = item.seq;
                let is_terminal = item.item.is_terminal();
                yield Ok(item.item.to_event(item.seq));
                if is_terminal {
                    return;
                }
            }

            if finished {
                return;
            }

            loop {
                match rx.recv().await {
                    Ok(item) => {
                        if item.seq <= last_seq {
                            continue;
                        }
                        last_seq = item.seq;
                        let is_terminal = item.item.is_terminal();
                        yield Ok(item.item.to_event(item.seq));
                        if is_terminal {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        yield Ok(resync_event("lagged"));
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }))
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

#[derive(Clone)]
enum OutputBackend {
    Memory(MemoryOutputStream),
    Persistent(RedisJobOutputStore),
}

#[derive(Clone)]
pub struct OutputStream {
    backend: OutputBackend,
}

impl OutputStream {
    pub fn new(finished_retention: Duration, max_history: usize) -> Self {
        Self {
            backend: OutputBackend::Memory(MemoryOutputStream::new(
                finished_retention,
                max_history,
            )),
        }
    }

    pub fn persistent(store: RedisJobOutputStore) -> Self {
        Self {
            backend: OutputBackend::Persistent(store),
        }
    }

    pub fn prime(&self, job_id: &str) {
        match &self.backend {
            OutputBackend::Memory(memory) => memory.prime(job_id),
            OutputBackend::Persistent(_) => {}
        }
    }

    pub async fn subscribe(
        &self,
        job_id: String,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>> {
        self.subscribe_from(job_id, None).await
    }

    pub async fn subscribe_from(
        &self,
        job_id: String,
        last_event_id: Option<u64>,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>> {
        match &self.backend {
            OutputBackend::Memory(memory) => memory.subscribe_from(job_id, last_event_id).await,
            OutputBackend::Persistent(store) => {
                let store = store.clone();
                let history = store.read_history(&job_id).await?;
                let live_job_id = job_id.clone();
                let needs_resync = persistent_history_needs_resync(
                    history.first().map(|item| item.seq),
                    last_event_id,
                );

                Ok(Box::pin(stream! {
                    let mut last_seq = last_event_id.unwrap_or(0);
                    let mut last_stream_id = history
                        .last()
                        .map(|item| item.stream_id.clone())
                        .unwrap_or_else(|| "0-0".to_string());

                    if needs_resync {
                        yield Ok(resync_event("history_trimmed"));
                    }

                    for item in history {
                        last_stream_id = item.stream_id.clone();
                        if item.seq <= last_seq {
                            continue;
                        }
                        last_seq = item.seq;
                        let output_item: OutputItem = item.record.into();
                        let is_terminal = output_item.is_terminal();
                        yield Ok(output_item.to_event(item.seq));
                        if is_terminal {
                            return;
                        }
                    }

                    loop {
                        match store.read_live(&live_job_id, &last_stream_id, 5_000).await {
                            Ok(items) if items.is_empty() => continue,
                            Ok(items) => {
                                for item in items {
                                    last_stream_id = item.stream_id.clone();
                                    if item.seq <= last_seq {
                                        continue;
                                    }
                                    last_seq = item.seq;
                                    let output_item: OutputItem = item.record.into();
                                    let is_terminal = output_item.is_terminal();
                                    yield Ok(output_item.to_event(item.seq));
                                    if is_terminal {
                                        return;
                                    }
                                }
                            }
                            Err(err) => {
                                tracing::warn!(job_id, error = %err, "failed reading persistent output stream");
                                yield Ok(resync_event("stream_error"));
                                tokio::time::sleep(Duration::from_millis(200)).await;
                            }
                        }
                    }
                }))
            }
        }
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
        match &self.backend {
            OutputBackend::Memory(memory) => memory.record(job_id, item).await,
            OutputBackend::Persistent(store) => {
                store.append(job_id, item.into()).await?;
                Ok(())
            }
        }
    }
}

fn resync_event(reason: &str) -> Event {
    Event::default().event("resync").data(reason)
}

fn persistent_history_needs_resync(history_first_seq: Option<u64>, last_event_id: Option<u64>) -> bool {
    match (history_first_seq, last_event_id) {
        (None, Some(last_event_id)) => last_event_id > 0,
        (Some(first_seq), Some(last_event_id)) => first_seq > last_event_id.saturating_add(1),
        _ => false,
    }
}

impl Default for OutputStream {
    fn default() -> Self {
        Self::new(Duration::from_secs(60 * 60), 256)
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::time::Duration;

    use axum::response::{IntoResponse, Sse};
    use futures_util::StreamExt;

    use super::*;

    async fn render_event(event: Event) -> String {
        let response = Sse::new(futures_util::stream::iter(vec![Ok::<Event, Infallible>(
            event,
        )]))
        .into_response();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8_lossy(&body).to_string()
    }

    #[tokio::test]
    async fn finished_jobs_are_evicted_from_history() {
        let stream = OutputStream::new(Duration::from_millis(25), 16);
        stream
            .push_stdout("job-1", b"hello\n".to_vec())
            .await
            .unwrap();
        stream.push_finished("job-1", 0, "").await.unwrap();

        tokio::time::sleep(Duration::from_millis(40)).await;

        let mut subscriber = stream.subscribe("job-1".into()).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), subscriber.next())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn subscribe_resumes_after_last_event_id_without_replaying_history() {
        let stream = OutputStream::new(Duration::from_secs(60), 16);
        stream
            .push_stdout("job-1", b"first\n".to_vec())
            .await
            .unwrap();
        stream
            .push_stdout("job-1", b"second\n".to_vec())
            .await
            .unwrap();
        stream.push_finished("job-1", 0, "").await.unwrap();

        let mut subscriber = stream
            .subscribe_from("job-1".into(), Some(1))
            .await
            .unwrap();
        let mut body = String::new();
        while let Some(event) = subscriber.next().await {
            let event = event.unwrap();
            body.push_str(&render_event(event).await);
            if body.contains("event: finished") {
                break;
            }
        }

        assert!(body.contains("data: second"));
        assert!(body.contains("event: finished"));
        assert!(!body.contains("data: first"));
    }

    #[tokio::test]
    async fn subscribe_emits_resync_when_history_gap_exceeds_retention() {
        let stream = OutputStream::new(Duration::from_secs(60), 2);
        stream
            .push_stdout("job-2", b"one\n".to_vec())
            .await
            .unwrap();
        stream
            .push_stdout("job-2", b"two\n".to_vec())
            .await
            .unwrap();
        stream
            .push_stdout("job-2", b"three\n".to_vec())
            .await
            .unwrap();
        stream
            .push_stdout("job-2", b"four\n".to_vec())
            .await
            .unwrap();

        let mut subscriber = stream
            .subscribe_from("job-2".into(), Some(1))
            .await
            .unwrap();
        let mut body = String::new();
        while let Some(event) = subscriber.next().await {
            let event = event.unwrap();
            body.push_str(&render_event(event).await);
            if body.contains("data: four") {
                break;
            }
        }

        assert!(body.contains("event: resync"));
        assert!(body.contains("data: three"));
        assert!(body.contains("data: four"));
        assert!(!body.contains("data: one"));
    }

    #[tokio::test]
    async fn subscribe_replays_without_dropping_gap_events() {
        let stream = OutputStream::new(Duration::from_secs(60), 8);
        stream
            .push_stdout("job-3", b"one\n".to_vec())
            .await
            .unwrap();
        stream
            .push_stdout("job-3", b"two\n".to_vec())
            .await
            .unwrap();
        stream
            .push_stdout("job-3", b"three\n".to_vec())
            .await
            .unwrap();

        let mut subscriber = stream
            .subscribe_from("job-3".into(), Some(1))
            .await
            .unwrap();
        let mut body = String::new();
        while let Some(event) = subscriber.next().await {
            let event = event.unwrap();
            body.push_str(&render_event(event).await);
            if body.contains("data: three") {
                break;
            }
        }

        assert!(!body.contains("event: resync"));
        assert!(body.contains("data: two"));
        assert!(body.contains("data: three"));
        assert!(!body.contains("data: one"));
    }

    #[test]
    fn persistent_history_requires_resync_when_resume_cursor_has_no_remaining_history() {
        assert!(persistent_history_needs_resync(None, Some(3)));
        assert!(!persistent_history_needs_resync(None, None));
        assert!(!persistent_history_needs_resync(None, Some(0)));
        assert!(persistent_history_needs_resync(Some(5), Some(3)));
        assert!(!persistent_history_needs_resync(Some(4), Some(3)));
    }

    #[tokio::test]
    async fn lagged_live_subscribers_receive_resync_event() {
        let stream = OutputStream::new(Duration::from_secs(60), 8);
        let mut subscriber = stream.subscribe("job-4".into()).await.unwrap();

        for index in 0..80 {
            stream
                .push_stdout("job-4", format!("chunk-{index}\n").into_bytes())
                .await
                .unwrap();
        }
        stream.push_finished("job-4", 0, "").await.unwrap();

        let mut body = String::new();
        while let Some(event) = subscriber.next().await {
            let event = event.unwrap();
            body.push_str(&render_event(event).await);
            if body.contains("event: finished") {
                break;
            }
        }

        assert!(body.contains("event: resync"));
        assert!(body.contains("data: lagged"));
    }

    #[tokio::test]
    async fn persistent_subscribe_does_not_miss_first_live_event_after_empty_history() {
        let stack = ahand_hub_store::test_support::TestStack::start()
            .await
            .unwrap();
        let store = ahand_hub_store::job_output_store::RedisJobOutputStore::new(
            stack.redis_url(),
            Duration::from_secs(60),
        )
        .await
        .unwrap();
        let stream = OutputStream::persistent(store.clone());
        let mut subscriber = stream.subscribe("job-persist-gap".into()).await.unwrap();

        store
            .append(
                "job-persist-gap",
                ahand_hub_store::job_output_store::JobOutputRecord::Finished {
                    exit_code: 0,
                    error: String::new(),
                },
            )
            .await
            .unwrap();

        let event = tokio::time::timeout(Duration::from_millis(250), subscriber.next())
            .await
            .expect("persistent subscriber should observe the first live event")
            .expect("persistent subscriber should emit an event")
            .unwrap();
        let body = render_event(event).await;

        assert!(body.contains("event: finished"));
    }
}
