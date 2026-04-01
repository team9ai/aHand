use std::convert::Infallible;
use std::sync::Arc;

use ahand_hub_core::job::{Job, JobStatus, NewJob};
use ahand_hub_core::services::job_dispatcher::JobDispatcher;
use ahand_hub_core::traits::JobStore;
use ahand_hub_core::HubError;
use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use futures_util::Stream;
use prost::Message;
use serde::{Deserialize, Serialize};

use crate::auth::AuthContextExt;
use crate::state::{AppState, MemoryJobStore};
use crate::ws::device_gateway::ConnectionRegistry;

#[derive(Deserialize)]
pub struct CreateJobRequest {
    pub device_id: String,
    pub tool: String,
    pub args: Vec<String>,
    pub timeout_ms: u64,
}

impl CreateJobRequest {
    pub fn into_new_job(self, requested_by: &str) -> NewJob {
        NewJob {
            device_id: self.device_id,
            tool: self.tool,
            args: self.args,
            cwd: None,
            env: Default::default(),
            timeout_ms: self.timeout_ms,
            requested_by: requested_by.into(),
        }
    }
}

#[derive(Serialize)]
pub struct CreateJobResponse {
    pub job_id: String,
    pub status: String,
}

pub struct JobRuntime {
    dispatcher: Arc<JobDispatcher>,
    jobs: Arc<MemoryJobStore>,
    connections: Arc<ConnectionRegistry>,
    output_stream: Arc<crate::output_stream::OutputStream>,
}

impl JobRuntime {
    pub fn new(
        dispatcher: Arc<JobDispatcher>,
        jobs: Arc<MemoryJobStore>,
        connections: Arc<ConnectionRegistry>,
        output_stream: Arc<crate::output_stream::OutputStream>,
    ) -> Self {
        Self {
            dispatcher,
            jobs,
            connections,
            output_stream,
        }
    }

    pub async fn create_job(&self, job: NewJob) -> anyhow::Result<Job> {
        let job = self.dispatcher.create_job(job).await?;
        let envelope = ahand_protocol::Envelope {
            device_id: job.device_id.clone(),
            msg_id: format!("job-{}", job.id),
            ts_ms: now_ms(),
            payload: Some(ahand_protocol::envelope::Payload::JobRequest(
                ahand_protocol::JobRequest {
                    job_id: job.id.to_string(),
                    tool: job.tool.clone(),
                    args: job.args.clone(),
                    cwd: job.cwd.clone().unwrap_or_default(),
                    env: job.env.clone(),
                    timeout_ms: job.timeout_ms,
                },
            )),
            ..Default::default()
        };
        if let Err(err) = self.connections.send(&job.device_id, envelope) {
            self.dispatcher
                .transition(&job.id.to_string(), JobStatus::Failed)
                .await?;
            self.output_stream
                .push_finished(&job.id.to_string(), -1, &err.to_string())
                .await?;
            return Err(err);
        }
        self.dispatcher
            .transition(&job.id.to_string(), JobStatus::Sent)
            .await?;
        Ok(job)
    }

    pub async fn handle_device_frame(&self, device_id: &str, frame: &[u8]) -> anyhow::Result<()> {
        let envelope = ahand_protocol::Envelope::decode(frame)?;
        match envelope.payload {
            Some(ahand_protocol::envelope::Payload::JobEvent(event)) => {
                let Some(job) = self.job_for_device(device_id, &event.job_id).await? else {
                    anyhow::bail!("job {} not found", event.job_id);
                };
                if is_terminal(job.status) {
                    return Ok(());
                }
                if let Some(event_kind) = event.event {
                    match event_kind {
                        ahand_protocol::job_event::Event::StdoutChunk(chunk) => {
                            self.dispatcher
                                .transition(&event.job_id, JobStatus::Running)
                                .await?;
                            self.output_stream.push_stdout(&event.job_id, chunk).await?;
                        }
                        ahand_protocol::job_event::Event::StderrChunk(chunk) => {
                            self.dispatcher
                                .transition(&event.job_id, JobStatus::Running)
                                .await?;
                            self.output_stream.push_stderr(&event.job_id, chunk).await?;
                        }
                        ahand_protocol::job_event::Event::Progress(progress) => {
                            self.output_stream
                                .push_progress(&event.job_id, progress)
                                .await?;
                        }
                    }
                }
            }
            Some(ahand_protocol::envelope::Payload::JobFinished(finished)) => {
                let Some(job) = self.job_for_device(device_id, &finished.job_id).await? else {
                    anyhow::bail!("job {} not found", finished.job_id);
                };
                if is_terminal(job.status) {
                    return Ok(());
                }
                let status = if finished.exit_code == 0 && finished.error.is_empty() {
                    JobStatus::Finished
                } else {
                    JobStatus::Failed
                };
                self.dispatcher.transition(&finished.job_id, status).await?;
                self.output_stream
                    .push_finished(&finished.job_id, finished.exit_code, &finished.error)
                    .await?;
            }
            Some(ahand_protocol::envelope::Payload::JobRejected(rejected)) => {
                let Some(job) = self.job_for_device(device_id, &rejected.job_id).await? else {
                    anyhow::bail!("job {} not found", rejected.job_id);
                };
                if is_terminal(job.status) {
                    return Ok(());
                }
                self.dispatcher
                    .transition(&rejected.job_id, JobStatus::Failed)
                    .await?;
                self.output_stream
                    .push_finished(&rejected.job_id, -1, &rejected.reason)
                    .await?;
            }
            _ => {}
        }

        Ok(())
    }

    #[allow(dead_code)]
    pub async fn get_job(&self, job_id: &str) -> anyhow::Result<Option<Job>> {
        Ok(self.jobs.get(job_id).await?)
    }

    async fn job_for_device(&self, device_id: &str, job_id: &str) -> anyhow::Result<Option<Job>> {
        let Some(job) = self.jobs.get(job_id).await? else {
            return Ok(None);
        };
        if job.device_id != device_id {
            anyhow::bail!("job {job_id} does not belong to connected device {device_id}");
        }
        Ok(Some(job))
    }
}

pub async fn create_job(
    auth: AuthContextExt,
    State(state): State<AppState>,
    Json(body): Json<CreateJobRequest>,
) -> Result<(StatusCode, Json<CreateJobResponse>), StatusCode> {
    auth.require_admin()?;
    let job = state
        .jobs
        .create_job(body.into_new_job("service:api"))
        .await
        .map_err(job_error_status)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(CreateJobResponse {
            job_id: job.id.to_string(),
            status: "pending".into(),
        }),
    ))
}

pub async fn stream_output(
    auth: AuthContextExt,
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, StatusCode> {
    auth.require_read_jobs()?;
    if state
        .jobs
        .get_job(&job_id)
        .await
        .map_err(job_error_status)?
        .is_none()
    {
        return Err(StatusCode::NOT_FOUND);
    }
    let stream = state
        .output_stream
        .subscribe(job_id)
        .await
        .map_err(job_error_status)?;
    Ok(Sse::new(stream))
}

fn job_error_status(err: anyhow::Error) -> StatusCode {
    match err.downcast_ref::<HubError>() {
        Some(HubError::DeviceNotFound(_)) | Some(HubError::JobNotFound(_)) => StatusCode::NOT_FOUND,
        Some(HubError::DeviceOffline(_)) => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn is_terminal(status: JobStatus) -> bool {
    matches!(
        status,
        JobStatus::Finished | JobStatus::Failed | JobStatus::Cancelled
    )
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ahand_hub_core::device::NewDevice;
    use ahand_hub_core::job::{JobFilter, JobStatus};
    use ahand_hub_core::traits::{DeviceStore, JobStore};
    use futures_util::StreamExt;

    use super::*;
    use crate::state::{MemoryAuditStore, MemoryDeviceStore};

    async fn insert_online_device(devices: &MemoryDeviceStore, device_id: &str) {
        devices
            .insert(NewDevice {
                id: device_id.into(),
                public_key: Some(vec![7; 32]),
                hostname: format!("{device_id}-host"),
                os: "linux".into(),
                capabilities: vec!["exec".into()],
                version: Some("0.1.2".into()),
                auth_method: "ed25519".into(),
            })
            .await
            .unwrap();
    }

    async fn build_runtime() -> (JobRuntime, Arc<ConnectionRegistry>) {
        let devices = Arc::new(MemoryDeviceStore::default());
        insert_online_device(&devices, "device-1").await;
        insert_online_device(&devices, "device-2").await;

        let jobs = Arc::new(MemoryJobStore::default());
        let audit = Arc::new(MemoryAuditStore::default());
        let connections = Arc::new(ConnectionRegistry::default());
        let output_stream = Arc::new(crate::output_stream::OutputStream::default());
        let dispatcher = Arc::new(JobDispatcher::new(devices, jobs.clone(), audit));

        (
            JobRuntime::new(dispatcher, jobs, connections.clone(), output_stream),
            connections,
        )
    }

    #[tokio::test]
    async fn create_job_marks_failed_and_emits_terminal_output_when_dispatch_fails() {
        let (runtime, connections) = build_runtime().await;
        let (_connection_id, rx) = connections.register("device-1".into());
        drop(rx);

        let err = runtime
            .create_job(NewJob {
                device_id: "device-1".into(),
                tool: "echo".into(),
                args: vec!["hello".into()],
                cwd: None,
                env: Default::default(),
                timeout_ms: 30_000,
                requested_by: "service:test".into(),
            })
            .await
            .unwrap_err();
        assert!(err
            .to_string()
            .contains("device device-1 connection closed"));

        let jobs = runtime.jobs.list(JobFilter::default()).await.unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].status, JobStatus::Failed);

        let mut stream = runtime
            .output_stream
            .subscribe(jobs[0].id.to_string())
            .await
            .unwrap();
        let first_event = tokio::time::timeout(Duration::from_millis(100), stream.next())
            .await
            .unwrap();
        assert!(first_event.is_some());
        let terminal = tokio::time::timeout(Duration::from_millis(100), stream.next())
            .await
            .unwrap();
        assert!(terminal.is_none());
    }

    #[tokio::test]
    async fn handle_device_frame_rejects_job_updates_for_other_devices() {
        let (runtime, connections) = build_runtime().await;
        let (_connection_id, _rx) = connections.register("device-2".into());
        let job = runtime
            .create_job(NewJob {
                device_id: "device-2".into(),
                tool: "echo".into(),
                args: vec!["hello".into()],
                cwd: None,
                env: Default::default(),
                timeout_ms: 30_000,
                requested_by: "service:test".into(),
            })
            .await
            .unwrap();

        let frame = ahand_protocol::Envelope {
            device_id: "device-1".into(),
            payload: Some(ahand_protocol::envelope::Payload::JobFinished(
                ahand_protocol::JobFinished {
                    job_id: job.id.to_string(),
                    exit_code: 0,
                    error: String::new(),
                },
            )),
            ..Default::default()
        }
        .encode_to_vec();

        let err = runtime
            .handle_device_frame("device-1", &frame)
            .await
            .unwrap_err();
        assert!(err
            .to_string()
            .contains("does not belong to connected device"));

        let stored = runtime
            .jobs
            .get(&job.id.to_string())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.status, JobStatus::Sent);
    }
}
