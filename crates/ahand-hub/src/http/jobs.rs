use std::convert::Infallible;
use std::sync::Arc;

use ahand_hub_core::HubError;
use ahand_hub_core::job::{Job, JobFilter, JobStatus, NewJob, is_terminal_status};
use ahand_hub_core::services::job_dispatcher::JobDispatcher;
use ahand_hub_core::traits::JobStore;
use axum::body::Bytes;
use axum::extract::rejection::JsonRejection;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Json, Path, Query, State};
use axum::http::HeaderMap;
use axum::http::header::HeaderName;
use axum::response::sse::{Event, Sse};
use dashmap::DashMap;
use futures_util::Stream;
use prost::Message;
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

use crate::auth::AuthContextExt;
use crate::events::EventBus;
use crate::http::api_error::{ApiError, ApiResult};
use crate::state::AppState;
use crate::ws::device_gateway::ConnectionRegistry;

#[derive(Deserialize)]
pub struct CreateJobRequest {
    pub device_id: String,
    pub tool: String,
    pub args: Vec<String>,
    pub timeout_ms: u64,
    #[serde(default)]
    pub interactive: bool,
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
            interactive: self.interactive,
        }
    }
}

#[derive(Serialize)]
pub struct CreateJobResponse {
    pub job_id: String,
    pub status: String,
}

#[derive(Serialize)]
pub struct DashboardJobResponse {
    pub id: String,
    pub device_id: String,
    pub tool: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub timeout_ms: u64,
    pub status: String,
}

#[derive(Clone)]
pub struct JobRuntime {
    dispatcher: Arc<JobDispatcher>,
    jobs: Arc<dyn JobStore>,
    connections: Arc<ConnectionRegistry>,
    events: Arc<EventBus>,
    output_stream: Arc<crate::output_stream::OutputStream>,
    timeout_grace_ms: u64,
    disconnect_grace_ms: u64,
    timeout_tasks: Arc<DashMap<String, JoinHandle<()>>>,
    disconnect_tasks: Arc<DashMap<String, JoinHandle<()>>>,
}

impl JobRuntime {
    pub fn new(
        dispatcher: Arc<JobDispatcher>,
        jobs: Arc<dyn JobStore>,
        connections: Arc<ConnectionRegistry>,
        events: Arc<EventBus>,
        output_stream: Arc<crate::output_stream::OutputStream>,
        timeout_grace_ms: u64,
        disconnect_grace_ms: u64,
    ) -> Self {
        Self {
            dispatcher,
            jobs,
            connections,
            events,
            output_stream,
            timeout_grace_ms,
            disconnect_grace_ms,
            timeout_tasks: Arc::new(DashMap::new()),
            disconnect_tasks: Arc::new(DashMap::new()),
        }
    }

    pub async fn create_job(&self, job: NewJob) -> anyhow::Result<Job> {
        let job = self.dispatcher.create_job(job).await?;
        self.output_stream.prime(&job.id.to_string());
        self.events.publish_job_created(&job).await?;
        self.transition_job(&job.id.to_string(), JobStatus::Sent, "service:api")
            .await?;

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
                    interactive: job.interactive,
                },
            )),
            ..Default::default()
        };
        if let Err(err) = self.connections.send(&job.device_id, envelope).await {
            self.transition_job(&job.id.to_string(), JobStatus::Failed, "hub:dispatch")
                .await?;
            self.record_terminal_state(&job.id.to_string(), -1, &err.to_string())
                .await?;
            self.output_stream
                .push_finished(&job.id.to_string(), -1, &err.to_string())
                .await?;
            return Err(HubError::DeviceOffline(job.device_id.clone()).into());
        }
        self.schedule_timeout(&job);
        Ok(self.jobs.get(&job.id.to_string()).await?.unwrap_or(job))
    }

    pub async fn cancel_job(&self, job_id: &str) -> anyhow::Result<Job> {
        let job = self
            .jobs
            .get(job_id)
            .await?
            .ok_or_else(|| HubError::JobNotFound(job_id.into()))?;
        if is_terminal_status(job.status) {
            return Err(HubError::JobNotCancellable(job_id.into()).into());
        }

        if self
            .connections
            .send(
                &job.device_id,
                ahand_protocol::Envelope {
                    device_id: job.device_id.clone(),
                    msg_id: format!("cancel-{job_id}"),
                    ts_ms: now_ms(),
                    payload: Some(ahand_protocol::envelope::Payload::CancelJob(
                        ahand_protocol::CancelJob {
                            job_id: job_id.into(),
                        },
                    )),
                    ..Default::default()
                },
            )
            .await
            .is_err()
        {
            return Err(HubError::DeviceOffline(job.device_id.clone()).into());
        }
        self.finish_job(job_id, JobStatus::Cancelled, -1, "cancelled", "service:api")
            .await?;
        self.jobs
            .get(job_id)
            .await?
            .ok_or_else(|| HubError::JobNotFound(job_id.into()).into())
    }

    pub async fn handle_device_frame(&self, device_id: &str, frame: &[u8]) -> anyhow::Result<()> {
        let envelope = ahand_protocol::Envelope::decode(frame)?;
        if self.connections.has_seen_inbound(device_id, envelope.seq) {
            self.connections.observe_ack(device_id, envelope.ack)?;
            return Ok(());
        }

        let seq = envelope.seq;
        let ack = envelope.ack;
        match envelope.payload {
            Some(ahand_protocol::envelope::Payload::JobEvent(event)) => {
                let Some(job) = self.job_for_device(device_id, &event.job_id).await? else {
                    anyhow::bail!("job {} not found", event.job_id);
                };
                self.clear_disconnect_task(&event.job_id);
                if is_terminal_status(job.status) {
                    self.connections.observe_inbound(device_id, seq, ack)?;
                    return Ok(());
                }
                if let Some(event_kind) = event.event {
                    match event_kind {
                        ahand_protocol::job_event::Event::StdoutChunk(chunk) => {
                            if let Err(err) = self
                                .transition_job(
                                    &event.job_id,
                                    JobStatus::Running,
                                    &format!("device:{device_id}"),
                                )
                                .await
                            {
                                return self
                                    .handle_stale_device_frame_error(device_id, seq, ack, err);
                            }
                            self.output_stream.push_stdout(&event.job_id, chunk).await?;
                        }
                        ahand_protocol::job_event::Event::StderrChunk(chunk) => {
                            if let Err(err) = self
                                .transition_job(
                                    &event.job_id,
                                    JobStatus::Running,
                                    &format!("device:{device_id}"),
                                )
                                .await
                            {
                                return self
                                    .handle_stale_device_frame_error(device_id, seq, ack, err);
                            }
                            self.output_stream.push_stderr(&event.job_id, chunk).await?;
                        }
                        ahand_protocol::job_event::Event::Progress(progress) => {
                            if let Err(err) = self
                                .transition_job(
                                    &event.job_id,
                                    JobStatus::Running,
                                    &format!("device:{device_id}"),
                                )
                                .await
                            {
                                return self
                                    .handle_stale_device_frame_error(device_id, seq, ack, err);
                            }
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
                self.clear_disconnect_task(&finished.job_id);
                if is_terminal_status(job.status) {
                    self.connections.observe_inbound(device_id, seq, ack)?;
                    return Ok(());
                }
                if job.status != JobStatus::Running {
                    if let Err(err) = self
                        .transition_job(
                            &finished.job_id,
                            JobStatus::Running,
                            &format!("device:{device_id}"),
                        )
                        .await
                    {
                        return self.handle_stale_device_frame_error(device_id, seq, ack, err);
                    }
                }
                let status = if finished.error == "cancelled" {
                    JobStatus::Cancelled
                } else if finished.exit_code == 0 && finished.error.is_empty() {
                    JobStatus::Finished
                } else {
                    JobStatus::Failed
                };
                if let Err(err) = self
                    .finish_job(
                        &finished.job_id,
                        status,
                        finished.exit_code,
                        &finished.error,
                        &format!("device:{device_id}"),
                    )
                    .await
                {
                    return self.handle_stale_device_frame_error(device_id, seq, ack, err);
                }
            }
            Some(ahand_protocol::envelope::Payload::JobRejected(rejected)) => {
                let Some(job) = self.job_for_device(device_id, &rejected.job_id).await? else {
                    anyhow::bail!("job {} not found", rejected.job_id);
                };
                self.clear_disconnect_task(&rejected.job_id);
                if is_terminal_status(job.status) {
                    self.connections.observe_inbound(device_id, seq, ack)?;
                    return Ok(());
                }
                if let Err(err) = self
                    .finish_job(
                        &rejected.job_id,
                        JobStatus::Failed,
                        -1,
                        &rejected.reason,
                        &format!("device:{device_id}"),
                    )
                    .await
                {
                    return self.handle_stale_device_frame_error(device_id, seq, ack, err);
                }
            }
            _ => {}
        }

        self.connections.observe_inbound(device_id, seq, ack)?;
        Ok(())
    }

    pub async fn handle_device_connected(&self, device_id: &str) -> anyhow::Result<()> {
        for job in self
            .jobs
            .list(JobFilter {
                device_id: Some(device_id.into()),
                status: None,
            })
            .await?
        {
            if matches!(job.status, JobStatus::Sent | JobStatus::Running) {
                self.clear_disconnect_task(&job.id.to_string());
            }
        }
        Ok(())
    }

    pub async fn handle_device_disconnected(&self, device_id: &str) -> anyhow::Result<()> {
        for job in self
            .jobs
            .list(JobFilter {
                device_id: Some(device_id.into()),
                status: None,
            })
            .await?
        {
            let job_id = job.id.to_string();
            match job.status {
                JobStatus::Pending => {
                    self.finish_job(
                        &job_id,
                        JobStatus::Failed,
                        -1,
                        "device disconnected",
                        "hub:disconnect",
                    )
                    .await?;
                }
                JobStatus::Sent | JobStatus::Running => self.schedule_disconnect_failure(&job),
                JobStatus::Finished | JobStatus::Failed | JobStatus::Cancelled => {}
            }
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

    async fn transition_job(
        &self,
        job_id: &str,
        status: JobStatus,
        actor: &str,
    ) -> anyhow::Result<JobStatus> {
        let transitioned = self.dispatcher.transition(job_id, status).await?;
        let job = self
            .jobs
            .get(job_id)
            .await?
            .ok_or_else(|| HubError::JobNotFound(job_id.into()))?;
        if transitioned.is_some()
            && let Err(err) = self.events.emit_job_status(&job, actor).await
        {
            tracing::warn!(job_id, error = %err, "failed to write job audit event");
        }
        Ok(job.status)
    }

    async fn record_terminal_state(
        &self,
        job_id: &str,
        exit_code: i32,
        error: &str,
    ) -> anyhow::Result<()> {
        let output_summary = build_output_summary(exit_code, error);
        self.jobs
            .update_terminal(job_id, exit_code, error, &output_summary)
            .await?;
        Ok(())
    }

    async fn finish_job(
        &self,
        job_id: &str,
        status: JobStatus,
        exit_code: i32,
        error: &str,
        actor: &str,
    ) -> anyhow::Result<()> {
        if let Some(job) = self.jobs.get(job_id).await?
            && is_terminal_status(job.status)
        {
            self.clear_lifecycle_tasks(job_id);
            return Ok(());
        }
        self.clear_lifecycle_tasks(job_id);
        self.transition_job(job_id, status, actor).await?;
        self.record_terminal_state(job_id, exit_code, error).await?;
        self.output_stream
            .push_finished(job_id, exit_code, error)
            .await?;
        Ok(())
    }

    fn schedule_timeout(&self, job: &Job) {
        let runtime = self.clone();
        let job_id = job.id.to_string();
        let device_id = job.device_id.clone();
        let timeout_ms = job.timeout_ms;
        self.replace_task(
            &self.timeout_tasks,
            job_id.clone(),
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)).await;
                let _ = runtime.handle_job_timeout(&job_id, &device_id).await;
            }),
        );
    }

    fn schedule_disconnect_failure(&self, job: &Job) {
        let runtime = self.clone();
        let job_id = job.id.to_string();
        let device_id = job.device_id.clone();
        self.replace_task(
            &self.disconnect_tasks,
            job_id.clone(),
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(
                    runtime.disconnect_grace_ms,
                ))
                .await;
                let _ = runtime.handle_disconnect_expired(&job_id, &device_id).await;
            }),
        );
    }

    async fn handle_job_timeout(&self, job_id: &str, device_id: &str) -> anyhow::Result<()> {
        let Some(job) = self.jobs.get(job_id).await? else {
            self.remove_timeout_task(job_id);
            return Ok(());
        };
        if is_terminal_status(job.status) {
            self.remove_timeout_task(job_id);
            return Ok(());
        }

        let _ = self
            .connections
            .send(
                device_id,
                ahand_protocol::Envelope {
                    device_id: device_id.into(),
                    msg_id: format!("timeout-{job_id}"),
                    ts_ms: now_ms(),
                    payload: Some(ahand_protocol::envelope::Payload::CancelJob(
                        ahand_protocol::CancelJob {
                            job_id: job_id.into(),
                        },
                    )),
                    ..Default::default()
                },
            )
            .await;

        tokio::time::sleep(std::time::Duration::from_millis(self.timeout_grace_ms)).await;
        let Some(job) = self.jobs.get(job_id).await? else {
            self.remove_timeout_task(job_id);
            return Ok(());
        };
        if is_terminal_status(job.status) {
            self.remove_timeout_task(job_id);
            return Ok(());
        }

        self.remove_timeout_task(job_id);
        self.clear_disconnect_task(job_id);
        self.transition_job(job_id, JobStatus::Failed, "hub:timeout")
            .await?;
        self.record_terminal_state(job_id, -1, "timeout").await?;
        self.output_stream
            .push_finished(job_id, -1, "timeout")
            .await
    }

    async fn handle_disconnect_expired(&self, job_id: &str, device_id: &str) -> anyhow::Result<()> {
        if self.connections.is_connected(device_id) {
            self.remove_disconnect_task(job_id);
            return Ok(());
        }
        let Some(job) = self.jobs.get(job_id).await? else {
            self.remove_disconnect_task(job_id);
            return Ok(());
        };
        if !matches!(job.status, JobStatus::Sent | JobStatus::Running) {
            self.remove_disconnect_task(job_id);
            return Ok(());
        }
        self.remove_disconnect_task(job_id);
        self.clear_timeout_task(job_id);
        self.transition_job(job_id, JobStatus::Failed, "hub:disconnect")
            .await?;
        self.record_terminal_state(job_id, -1, "device disconnected")
            .await?;
        self.output_stream
            .push_finished(job_id, -1, "device disconnected")
            .await
    }

    fn handle_stale_device_frame_error(
        &self,
        device_id: &str,
        seq: u64,
        ack: u64,
        err: anyhow::Error,
    ) -> anyhow::Result<()> {
        if matches!(
            err.downcast_ref::<HubError>(),
            Some(HubError::IllegalJobTransition { current, .. }) if is_terminal_status(*current)
        ) {
            self.connections.observe_inbound(device_id, seq, ack)?;
            return Ok(());
        }
        Err(err)
    }

    fn replace_task(
        &self,
        tasks: &DashMap<String, JoinHandle<()>>,
        job_id: String,
        handle: JoinHandle<()>,
    ) {
        if let Some(existing) = tasks.insert(job_id, handle) {
            existing.abort();
        }
    }

    fn clear_lifecycle_tasks(&self, job_id: &str) {
        self.clear_timeout_task(job_id);
        self.clear_disconnect_task(job_id);
    }

    fn clear_timeout_task(&self, job_id: &str) {
        if let Some((_, task)) = self.timeout_tasks.remove(job_id) {
            task.abort();
        }
    }

    fn clear_disconnect_task(&self, job_id: &str) {
        if let Some((_, task)) = self.disconnect_tasks.remove(job_id) {
            task.abort();
        }
    }

    fn remove_timeout_task(&self, job_id: &str) {
        let _ = self.timeout_tasks.remove(job_id);
    }

    fn remove_disconnect_task(&self, job_id: &str) {
        let _ = self.disconnect_tasks.remove(job_id);
    }
}

pub async fn create_job(
    auth: AuthContextExt,
    State(state): State<AppState>,
    body: Result<Json<CreateJobRequest>, JsonRejection>,
) -> ApiResult<(axum::http::StatusCode, Json<CreateJobResponse>)> {
    auth.require_dashboard_access()?;
    let Json(body) = body.map_err(ApiError::from_json_rejection)?;
    let job = state
        .jobs
        .create_job(body.into_new_job(&format!("dashboard:{}", auth.0.subject)))
        .await
        .map_err(ApiError::from)?;
    Ok((
        axum::http::StatusCode::ACCEPTED,
        Json(CreateJobResponse {
            job_id: job.id.to_string(),
            status: job_status_name(job.status).into(),
        }),
    ))
}

#[derive(Deserialize, Default)]
pub struct JobListQuery {
    pub device_id: Option<String>,
    pub status: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

pub async fn list_jobs(
    auth: AuthContextExt,
    State(state): State<AppState>,
    query: Result<Query<JobListQuery>, QueryRejection>,
) -> ApiResult<Json<Vec<DashboardJobResponse>>> {
    auth.require_read_jobs()?;
    let Query(query) = query.map_err(ApiError::from_query_rejection)?;
    let filter = JobFilter {
        device_id: query.device_id,
        status: parse_job_status(query.status.as_deref())?,
    };
    let mut jobs = state
        .jobs_store
        .list(filter)
        .await
        .map_err(|_| ApiError::internal("Failed to list jobs"))?;
    apply_pagination(&mut jobs, query.offset.unwrap_or(0), query.limit);
    Ok(Json(
        jobs.into_iter().map(DashboardJobResponse::from).collect(),
    ))
}

pub async fn get_job(
    auth: AuthContextExt,
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> ApiResult<Json<DashboardJobResponse>> {
    auth.require_read_jobs()?;
    let job = state
        .jobs_store
        .get(&job_id)
        .await
        .map_err(|_| ApiError::internal("Failed to load job"))?
        .ok_or_else(|| {
            ApiError::new(
                axum::http::StatusCode::NOT_FOUND,
                "JOB_NOT_FOUND",
                format!("Job {job_id} was not found"),
            )
        })?;
    Ok(Json(DashboardJobResponse::from(job)))
}

pub async fn cancel_job(
    auth: AuthContextExt,
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> ApiResult<(axum::http::StatusCode, Json<CreateJobResponse>)> {
    auth.require_admin()?;
    let job = state
        .jobs
        .cancel_job(&job_id)
        .await
        .map_err(ApiError::from)?;
    Ok((
        axum::http::StatusCode::ACCEPTED,
        Json(CreateJobResponse {
            job_id,
            status: job_status_name(job.status).into(),
        }),
    ))
}

#[derive(Deserialize)]
pub struct ResizeRequest {
    pub cols: u32,
    pub rows: u32,
}

pub async fn send_stdin(
    auth: AuthContextExt,
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    body: Bytes,
) -> ApiResult<axum::http::StatusCode> {
    auth.require_dashboard_access()?;
    let job = state
        .jobs
        .jobs
        .get(&job_id)
        .await
        .map_err(|_| ApiError::internal("Failed to load job"))?
        .ok_or_else(|| ApiError::new(
            axum::http::StatusCode::NOT_FOUND,
            "JOB_NOT_FOUND",
            format!("Job {job_id} was not found"),
        ))?;
    if is_terminal_status(job.status) {
        return Err(ApiError::gone(format!("Job {job_id} has already finished")));
    }
    state
        .jobs
        .connections
        .send(
            &job.device_id,
            ahand_protocol::Envelope {
                device_id: job.device_id.clone(),
                msg_id: format!("stdin-{job_id}"),
                ts_ms: now_ms(),
                payload: Some(ahand_protocol::envelope::Payload::StdinChunk(
                    ahand_protocol::StdinChunk {
                        job_id: job_id.clone(),
                        data: body.to_vec(),
                    },
                )),
                ..Default::default()
            },
        )
        .await
        .map_err(|_| ApiError::new(
            axum::http::StatusCode::CONFLICT,
            "DEVICE_OFFLINE",
            format!("Device {} is not currently connected", job.device_id),
        ))?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub async fn send_resize(
    auth: AuthContextExt,
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    body: Result<Json<ResizeRequest>, JsonRejection>,
) -> ApiResult<axum::http::StatusCode> {
    auth.require_dashboard_access()?;
    let Json(body) = body.map_err(ApiError::from_json_rejection)?;
    let job = state
        .jobs
        .jobs
        .get(&job_id)
        .await
        .map_err(|_| ApiError::internal("Failed to load job"))?
        .ok_or_else(|| ApiError::new(
            axum::http::StatusCode::NOT_FOUND,
            "JOB_NOT_FOUND",
            format!("Job {job_id} was not found"),
        ))?;
    if is_terminal_status(job.status) {
        return Err(ApiError::gone(format!("Job {job_id} has already finished")));
    }
    state
        .jobs
        .connections
        .send(
            &job.device_id,
            ahand_protocol::Envelope {
                device_id: job.device_id.clone(),
                msg_id: format!("resize-{job_id}"),
                ts_ms: now_ms(),
                payload: Some(ahand_protocol::envelope::Payload::TerminalResize(
                    ahand_protocol::TerminalResize {
                        job_id: job_id.clone(),
                        cols: body.cols,
                        rows: body.rows,
                    },
                )),
                ..Default::default()
            },
        )
        .await
        .map_err(|_| ApiError::new(
            axum::http::StatusCode::CONFLICT,
            "DEVICE_OFFLINE",
            format!("Device {} is not currently connected", job.device_id),
        ))?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub async fn stream_output(
    auth: AuthContextExt,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    auth.require_read_jobs()?;
    if state
        .jobs
        .get_job(&job_id)
        .await
        .map_err(ApiError::from)?
        .is_none()
    {
        return Err(ApiError::new(
            axum::http::StatusCode::NOT_FOUND,
            "JOB_NOT_FOUND",
            format!("Job {job_id} was not found"),
        ));
    }
    let stream = state
        .output_stream
        .subscribe_from(job_id, parse_last_event_id(&headers)?)
        .await
        .map_err(ApiError::from)?;
    Ok(Sse::new(stream))
}

fn job_status_name(status: JobStatus) -> &'static str {
    match status {
        JobStatus::Pending => "pending",
        JobStatus::Sent => "sent",
        JobStatus::Running => "running",
        JobStatus::Finished => "finished",
        JobStatus::Failed => "failed",
        JobStatus::Cancelled => "cancelled",
    }
}

fn parse_job_status(status: Option<&str>) -> ApiResult<Option<JobStatus>> {
    match status {
        None => Ok(None),
        Some("pending") => Ok(Some(JobStatus::Pending)),
        Some("sent") => Ok(Some(JobStatus::Sent)),
        Some("running") => Ok(Some(JobStatus::Running)),
        Some("finished") => Ok(Some(JobStatus::Finished)),
        Some("failed") => Ok(Some(JobStatus::Failed)),
        Some("cancelled") => Ok(Some(JobStatus::Cancelled)),
        Some(other) => Err(ApiError::validation(format!("Invalid job status: {other}"))),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn build_output_summary(exit_code: i32, error: &str) -> String {
    let summary = if !error.is_empty() {
        error.to_string()
    } else if exit_code == 0 {
        "completed successfully".to_string()
    } else {
        format!("exit code {exit_code}")
    };

    summary.chars().take(1024).collect()
}

fn parse_last_event_id(headers: &HeaderMap) -> ApiResult<Option<u64>> {
    let Some(value) = headers.get(HeaderName::from_static("last-event-id")) else {
        return Ok(None);
    };
    let value = value
        .to_str()
        .map_err(|_| ApiError::validation("Invalid Last-Event-ID header"))?;
    let id = value
        .parse::<u64>()
        .map_err(|_| ApiError::validation("Invalid Last-Event-ID header"))?;
    Ok(Some(id))
}

fn apply_pagination<T>(items: &mut Vec<T>, offset: usize, limit: Option<usize>) {
    if offset == 0 && limit.is_none() {
        return;
    }

    let take = limit.unwrap_or(usize::MAX);
    let paged = std::mem::take(items)
        .into_iter()
        .skip(offset)
        .take(take)
        .collect();
    *items = paged;
}

impl From<Job> for DashboardJobResponse {
    fn from(job: Job) -> Self {
        Self {
            id: job.id.to_string(),
            device_id: job.device_id,
            tool: job.tool,
            args: job.args,
            cwd: job.cwd,
            timeout_ms: job.timeout_ms,
            status: job_status_name(job.status).into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use ahand_hub_core::audit::AuditFilter;
    use ahand_hub_core::device::NewDevice;
    use ahand_hub_core::job::{JobFilter, JobStatus};
    use ahand_hub_core::traits::{AuditStore, DeviceStore, JobStore};
    use futures_util::StreamExt;

    use super::*;
    use crate::state::{MemoryAuditStore, MemoryDeviceStore, MemoryJobStore};

    #[derive(Clone)]
    struct RacingJobStore {
        inner: Arc<MemoryJobStore>,
        fail_next_running_transition: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl JobStore for RacingJobStore {
        async fn insert(&self, job: NewJob) -> ahand_hub_core::Result<Job> {
            self.inner.insert(job).await
        }

        async fn get(&self, job_id: &str) -> ahand_hub_core::Result<Option<Job>> {
            self.inner.get(job_id).await
        }

        async fn list(&self, filter: JobFilter) -> ahand_hub_core::Result<Vec<Job>> {
            self.inner.list(filter).await
        }

        async fn transition_status(
            &self,
            job_id: &str,
            status: JobStatus,
        ) -> ahand_hub_core::Result<Option<JobStatus>> {
            if status == JobStatus::Running
                && self
                    .fail_next_running_transition
                    .swap(false, Ordering::SeqCst)
            {
                let _ = self
                    .inner
                    .transition_status(job_id, JobStatus::Failed)
                    .await?;
                return Err(HubError::IllegalJobTransition {
                    current: JobStatus::Failed,
                    requested: JobStatus::Running,
                });
            }
            self.inner.transition_status(job_id, status).await
        }

        async fn update_status(
            &self,
            job_id: &str,
            status: JobStatus,
        ) -> ahand_hub_core::Result<()> {
            self.inner.update_status(job_id, status).await
        }

        async fn update_terminal(
            &self,
            job_id: &str,
            exit_code: i32,
            error: &str,
            output_summary: &str,
        ) -> ahand_hub_core::Result<()> {
            self.inner
                .update_terminal(job_id, exit_code, error, output_summary)
                .await
        }
    }

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
        devices.mark_online(device_id, "ws").await.unwrap();
    }

    async fn build_runtime() -> (
        JobRuntime,
        Arc<ConnectionRegistry>,
        Arc<MemoryJobStore>,
        Arc<MemoryAuditStore>,
    ) {
        let devices = Arc::new(MemoryDeviceStore::default());
        insert_online_device(&devices, "device-1").await;
        insert_online_device(&devices, "device-2").await;

        let jobs = Arc::new(MemoryJobStore::default());
        let audit = Arc::new(MemoryAuditStore::default());
        let connections = Arc::new(ConnectionRegistry::default());
        let events = Arc::new(EventBus::new(audit.clone()));
        let output_stream = Arc::new(crate::output_stream::OutputStream::default());
        let dispatcher = Arc::new(JobDispatcher::new(devices, jobs.clone(), audit.clone()));

        (
            JobRuntime::new(
                dispatcher,
                jobs.clone(),
                connections.clone(),
                events,
                output_stream,
                100,
                100,
            ),
            connections,
            jobs,
            audit,
        )
    }

    async fn build_runtime_with_jobs(
        jobs: Arc<dyn JobStore>,
    ) -> (JobRuntime, Arc<ConnectionRegistry>, Arc<MemoryAuditStore>) {
        let devices = Arc::new(MemoryDeviceStore::default());
        insert_online_device(&devices, "device-1").await;
        insert_online_device(&devices, "device-2").await;

        let audit = Arc::new(MemoryAuditStore::default());
        let connections = Arc::new(ConnectionRegistry::default());
        let events = Arc::new(EventBus::new(audit.clone()));
        let output_stream = Arc::new(crate::output_stream::OutputStream::default());
        let dispatcher = Arc::new(JobDispatcher::new(devices, jobs.clone(), audit.clone()));

        (
            JobRuntime::new(
                dispatcher,
                jobs,
                connections.clone(),
                events,
                output_stream,
                100,
                100,
            ),
            connections,
            audit,
        )
    }

    async fn wait_for_audit_entries(
        audit: &Arc<MemoryAuditStore>,
        resource_id: String,
        action: &str,
    ) -> Vec<ahand_hub_core::audit::AuditEntry> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        loop {
            let entries = audit
                .query(AuditFilter {
                    resource_type: Some("job".into()),
                    resource_id: Some(resource_id.clone()),
                    action: Some(action.into()),
                    ..Default::default()
                })
                .await
                .unwrap();
            if !entries.is_empty() || tokio::time::Instant::now() >= deadline {
                return entries;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn attach_live_connection(
        connections: &Arc<ConnectionRegistry>,
        device_id: &str,
    ) -> tokio::task::JoinHandle<()> {
        let (_connection_id, mut rx, _close_rx) =
            connections.register(device_id.into(), 0).unwrap();
        tokio::spawn(async move {
            while let Some(outbound) = rx.recv().await {
                let _ = outbound;
            }
        })
    }

    #[tokio::test]
    async fn create_job_marks_failed_and_emits_terminal_output_when_dispatch_fails() {
        let (runtime, connections, jobs, audit) = build_runtime().await;
        let (_connection_id, rx, _close_rx) = connections.register("device-1".into(), 0).unwrap();
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
                interactive: false,
            })
            .await
            .unwrap_err();
        assert!(matches!(
            err.downcast_ref::<HubError>(),
            Some(HubError::DeviceOffline(device_id)) if device_id == "device-1"
        ));

        let jobs = jobs.list(JobFilter::default()).await.unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].status, JobStatus::Failed);
        assert_eq!(jobs[0].exit_code, Some(-1));
        assert!(
            jobs[0]
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("device-1")
        );
        assert!(jobs[0].output_summary.is_some());
        assert!(jobs[0].started_at.is_none());
        assert!(jobs[0].finished_at.is_some());
        let audit_entries =
            wait_for_audit_entries(&audit, jobs[0].id.to_string(), "job.failed").await;
        assert_eq!(audit_entries.len(), 1);
        let sent_entries = audit
            .query(AuditFilter {
                resource_type: Some("job".into()),
                resource_id: Some(jobs[0].id.to_string()),
                action: Some("job.sent".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(sent_entries.len(), 1);

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
        let (runtime, connections, _jobs, _audit) = build_runtime().await;
        let _transport = attach_live_connection(&connections, "device-2");
        let job = runtime
            .create_job(NewJob {
                device_id: "device-2".into(),
                tool: "echo".into(),
                args: vec!["hello".into()],
                cwd: None,
                env: Default::default(),
                timeout_ms: 30_000,
                requested_by: "service:test".into(),
                interactive: false,
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
        assert!(
            err.to_string()
                .contains("does not belong to connected device")
        );

        let stored = runtime
            .jobs
            .get(&job.id.to_string())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.status, JobStatus::Sent);
    }

    #[tokio::test]
    async fn progress_event_moves_job_to_running_and_writes_audit() {
        let (runtime, connections, jobs, audit) = build_runtime().await;
        let _transport = attach_live_connection(&connections, "device-1");
        let job = runtime
            .create_job(NewJob {
                device_id: "device-1".into(),
                tool: "echo".into(),
                args: vec!["hello".into()],
                cwd: None,
                env: Default::default(),
                timeout_ms: 30_000,
                requested_by: "service:test".into(),
                interactive: false,
            })
            .await
            .unwrap();

        let frame = ahand_protocol::Envelope {
            device_id: "device-1".into(),
            payload: Some(ahand_protocol::envelope::Payload::JobEvent(
                ahand_protocol::JobEvent {
                    job_id: job.id.to_string(),
                    event: Some(ahand_protocol::job_event::Event::Progress(42)),
                },
            )),
            ..Default::default()
        }
        .encode_to_vec();

        runtime
            .handle_device_frame("device-1", &frame)
            .await
            .unwrap();

        let stored = jobs.get(&job.id.to_string()).await.unwrap().unwrap();
        assert_eq!(stored.status, JobStatus::Running);
        assert!(stored.started_at.is_some());
        let audit_entries = wait_for_audit_entries(&audit, job.id.to_string(), "job.running").await;
        assert_eq!(audit_entries.len(), 1);
    }

    #[tokio::test]
    async fn stale_running_transition_from_device_frame_is_ignored() {
        let jobs = Arc::new(RacingJobStore {
            inner: Arc::new(MemoryJobStore::default()),
            fail_next_running_transition: Arc::new(AtomicBool::new(true)),
        });
        let (runtime, connections, _audit) = build_runtime_with_jobs(jobs.clone()).await;
        let _transport = attach_live_connection(&connections, "device-1");
        let job = runtime
            .create_job(NewJob {
                device_id: "device-1".into(),
                tool: "echo".into(),
                args: vec!["hello".into()],
                cwd: None,
                env: Default::default(),
                timeout_ms: 30_000,
                requested_by: "service:test".into(),
                interactive: false,
            })
            .await
            .unwrap();

        let frame = ahand_protocol::Envelope {
            device_id: "device-1".into(),
            payload: Some(ahand_protocol::envelope::Payload::JobEvent(
                ahand_protocol::JobEvent {
                    job_id: job.id.to_string(),
                    event: Some(ahand_protocol::job_event::Event::Progress(7)),
                },
            )),
            ..Default::default()
        }
        .encode_to_vec();

        runtime
            .handle_device_frame("device-1", &frame)
            .await
            .unwrap();

        let stored = jobs.inner.get(&job.id.to_string()).await.unwrap().unwrap();
        assert_eq!(stored.status, JobStatus::Failed);
    }

    #[tokio::test]
    async fn cancelled_finish_event_marks_job_cancelled_and_writes_audit() {
        let (runtime, connections, jobs, audit) = build_runtime().await;
        let _transport = attach_live_connection(&connections, "device-1");
        let job = runtime
            .create_job(NewJob {
                device_id: "device-1".into(),
                tool: "sleep".into(),
                args: vec!["30".into()],
                cwd: None,
                env: Default::default(),
                timeout_ms: 30_000,
                requested_by: "service:test".into(),
                interactive: false,
            })
            .await
            .unwrap();

        let frame = ahand_protocol::Envelope {
            device_id: "device-1".into(),
            payload: Some(ahand_protocol::envelope::Payload::JobFinished(
                ahand_protocol::JobFinished {
                    job_id: job.id.to_string(),
                    exit_code: -1,
                    error: "cancelled".into(),
                },
            )),
            ..Default::default()
        }
        .encode_to_vec();

        runtime
            .handle_device_frame("device-1", &frame)
            .await
            .unwrap();

        let stored = jobs.get(&job.id.to_string()).await.unwrap().unwrap();
        assert_eq!(stored.status, JobStatus::Cancelled);
        assert_eq!(stored.exit_code, Some(-1));
        assert_eq!(stored.error.as_deref(), Some("cancelled"));
        assert_eq!(stored.output_summary.as_deref(), Some("cancelled"));
        assert!(stored.finished_at.is_some());
        let audit_entries =
            wait_for_audit_entries(&audit, job.id.to_string(), "job.cancelled").await;
        assert_eq!(audit_entries.len(), 1);
    }

    #[tokio::test]
    async fn successful_finish_event_marks_job_finished_and_writes_audit() {
        let (runtime, connections, jobs, audit) = build_runtime().await;
        let _transport = attach_live_connection(&connections, "device-1");
        let job = runtime
            .create_job(NewJob {
                device_id: "device-1".into(),
                tool: "echo".into(),
                args: vec!["hello".into()],
                cwd: None,
                env: Default::default(),
                timeout_ms: 30_000,
                requested_by: "service:test".into(),
                interactive: false,
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

        runtime
            .handle_device_frame("device-1", &frame)
            .await
            .unwrap();

        let stored = jobs.get(&job.id.to_string()).await.unwrap().unwrap();
        assert_eq!(stored.status, JobStatus::Finished);
        assert!(stored.started_at.is_some());
        assert_eq!(stored.exit_code, Some(0));
        assert_eq!(stored.error.as_deref(), Some(""));
        assert_eq!(
            stored.output_summary.as_deref(),
            Some("completed successfully")
        );
        assert!(stored.finished_at.is_some());
        let audit_entries =
            wait_for_audit_entries(&audit, job.id.to_string(), "job.finished").await;
        assert_eq!(audit_entries.len(), 1);
    }
}
