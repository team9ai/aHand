use std::convert::Infallible;
use std::sync::Arc;

use ahand_hub_core::job::{Job, JobStatus, NewJob};
use ahand_hub_core::services::job_dispatcher::JobDispatcher;
use ahand_hub_core::traits::JobStore;
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
            return Err(err);
        }
        self.dispatcher
            .transition(&job.id.to_string(), JobStatus::Sent)
            .await?;
        Ok(job)
    }

    pub async fn handle_device_frame(&self, _device_id: &str, frame: &[u8]) -> anyhow::Result<()> {
        let envelope = ahand_protocol::Envelope::decode(frame)?;
        match envelope.payload {
            Some(ahand_protocol::envelope::Payload::JobEvent(event)) => {
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
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
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
    let stream = state
        .output_stream
        .subscribe(job_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Sse::new(stream))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
