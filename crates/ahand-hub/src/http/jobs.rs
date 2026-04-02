use std::convert::Infallible;
use std::sync::Arc;

use ahand_hub_core::HubError;
use ahand_hub_core::job::{Job, JobFilter, JobStatus, NewJob, is_terminal_status};
use ahand_hub_core::services::job_dispatcher::JobDispatcher;
use ahand_hub_core::traits::JobStore;
use axum::extract::{Json, Path, Query, State};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::http::header::HeaderName;
use axum::response::sse::{Event, Sse};
use futures_util::Stream;
use prost::Message;
use serde::{Deserialize, Serialize};

use crate::auth::AuthContextExt;
use crate::events::EventBus;
use crate::state::AppState;
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

pub struct JobRuntime {
    dispatcher: Arc<JobDispatcher>,
    jobs: Arc<dyn JobStore>,
    connections: Arc<ConnectionRegistry>,
    events: Arc<EventBus>,
    output_stream: Arc<crate::output_stream::OutputStream>,
}

impl JobRuntime {
    pub fn new(
        dispatcher: Arc<JobDispatcher>,
        jobs: Arc<dyn JobStore>,
        connections: Arc<ConnectionRegistry>,
        events: Arc<EventBus>,
        output_stream: Arc<crate::output_stream::OutputStream>,
    ) -> Self {
        Self {
            dispatcher,
            jobs,
            connections,
            events,
            output_stream,
        }
    }

    pub async fn create_job(&self, job: NewJob) -> anyhow::Result<Job> {
        let job = self.dispatcher.create_job(job).await?;
        self.output_stream.prime(&job.id.to_string());
        self.events.publish_job_created(&job);
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
        Ok(job)
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
                if is_terminal_status(job.status) {
                    self.connections.observe_inbound(device_id, seq, ack)?;
                    return Ok(());
                }
                if let Some(event_kind) = event.event {
                    match event_kind {
                        ahand_protocol::job_event::Event::StdoutChunk(chunk) => {
                            self.transition_job(
                                &event.job_id,
                                JobStatus::Running,
                                &format!("device:{device_id}"),
                            )
                            .await?;
                            self.output_stream.push_stdout(&event.job_id, chunk).await?;
                        }
                        ahand_protocol::job_event::Event::StderrChunk(chunk) => {
                            self.transition_job(
                                &event.job_id,
                                JobStatus::Running,
                                &format!("device:{device_id}"),
                            )
                            .await?;
                            self.output_stream.push_stderr(&event.job_id, chunk).await?;
                        }
                        ahand_protocol::job_event::Event::Progress(progress) => {
                            self.transition_job(
                                &event.job_id,
                                JobStatus::Running,
                                &format!("device:{device_id}"),
                            )
                            .await?;
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
                if is_terminal_status(job.status) {
                    self.connections.observe_inbound(device_id, seq, ack)?;
                    return Ok(());
                }
                if job.status != JobStatus::Running {
                    self.transition_job(
                        &finished.job_id,
                        JobStatus::Running,
                        &format!("device:{device_id}"),
                    )
                    .await?;
                }
                let status = if finished.error == "cancelled" {
                    JobStatus::Cancelled
                } else if finished.exit_code == 0 && finished.error.is_empty() {
                    JobStatus::Finished
                } else {
                    JobStatus::Failed
                };
                self.transition_job(&finished.job_id, status, &format!("device:{device_id}"))
                    .await?;
                self.record_terminal_state(&finished.job_id, finished.exit_code, &finished.error)
                    .await?;
                self.output_stream
                    .push_finished(&finished.job_id, finished.exit_code, &finished.error)
                    .await?;
            }
            Some(ahand_protocol::envelope::Payload::JobRejected(rejected)) => {
                let Some(job) = self.job_for_device(device_id, &rejected.job_id).await? else {
                    anyhow::bail!("job {} not found", rejected.job_id);
                };
                if is_terminal_status(job.status) {
                    self.connections.observe_inbound(device_id, seq, ack)?;
                    return Ok(());
                }
                self.transition_job(
                    &rejected.job_id,
                    JobStatus::Failed,
                    &format!("device:{device_id}"),
                )
                .await?;
                self.record_terminal_state(&rejected.job_id, -1, &rejected.reason)
                    .await?;
                self.output_stream
                    .push_finished(&rejected.job_id, -1, &rejected.reason)
                    .await?;
            }
            _ => {}
        }

        self.connections.observe_inbound(device_id, seq, ack)?;
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
            status: job_status_name(job.status).into(),
        }),
    ))
}

#[derive(Deserialize, Default)]
pub struct JobListQuery {
    pub device_id: Option<String>,
    pub status: Option<String>,
}

pub async fn list_jobs(
    auth: AuthContextExt,
    State(state): State<AppState>,
    Query(query): Query<JobListQuery>,
) -> Result<Json<Vec<DashboardJobResponse>>, StatusCode> {
    auth.require_read_jobs()?;
    let filter = JobFilter {
        device_id: query.device_id,
        status: parse_job_status(query.status.as_deref())?,
    };
    let jobs = state
        .jobs_store
        .list(filter)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(
        jobs.into_iter().map(DashboardJobResponse::from).collect(),
    ))
}

pub async fn get_job(
    auth: AuthContextExt,
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Json<DashboardJobResponse>, StatusCode> {
    auth.require_read_jobs()?;
    let job = state
        .jobs_store
        .get(&job_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(DashboardJobResponse::from(job)))
}

pub async fn cancel_job(
    auth: AuthContextExt,
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<(StatusCode, Json<CreateJobResponse>), StatusCode> {
    auth.require_admin()?;
    let job = state
        .jobs
        .cancel_job(&job_id)
        .await
        .map_err(job_error_status)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(CreateJobResponse {
            job_id,
            status: job_status_name(job.status).into(),
        }),
    ))
}

pub async fn stream_output(
    auth: AuthContextExt,
    State(state): State<AppState>,
    headers: HeaderMap,
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
        .subscribe_from(job_id, parse_last_event_id(&headers)?)
        .await
        .map_err(job_error_status)?;
    Ok(Sse::new(stream))
}

fn job_error_status(err: anyhow::Error) -> StatusCode {
    match err.downcast_ref::<HubError>() {
        Some(HubError::DeviceNotFound(_)) | Some(HubError::JobNotFound(_)) => StatusCode::NOT_FOUND,
        Some(HubError::DeviceOffline(_)) | Some(HubError::JobNotCancellable(_)) => {
            StatusCode::CONFLICT
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
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

fn parse_job_status(status: Option<&str>) -> Result<Option<JobStatus>, StatusCode> {
    match status {
        None => Ok(None),
        Some("pending") => Ok(Some(JobStatus::Pending)),
        Some("sent") => Ok(Some(JobStatus::Sent)),
        Some("running") => Ok(Some(JobStatus::Running)),
        Some("finished") => Ok(Some(JobStatus::Finished)),
        Some("failed") => Ok(Some(JobStatus::Failed)),
        Some("cancelled") => Ok(Some(JobStatus::Cancelled)),
        Some(_) => Err(StatusCode::BAD_REQUEST),
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

fn parse_last_event_id(headers: &HeaderMap) -> Result<Option<u64>, StatusCode> {
    let Some(value) = headers.get(HeaderName::from_static("last-event-id")) else {
        return Ok(None);
    };
    let value = value.to_str().map_err(|_| StatusCode::BAD_REQUEST)?;
    let id = value.parse::<u64>().map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(Some(id))
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
    use std::time::Duration;

    use ahand_hub_core::audit::AuditFilter;
    use ahand_hub_core::device::NewDevice;
    use ahand_hub_core::job::{JobFilter, JobStatus};
    use ahand_hub_core::traits::{AuditStore, DeviceStore, JobStore};
    use futures_util::StreamExt;

    use super::*;
    use crate::state::{MemoryAuditStore, MemoryDeviceStore, MemoryJobStore};
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
            ),
            connections,
            jobs,
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
        let audit_entries =
            wait_for_audit_entries(&audit, job.id.to_string(), "job.running").await;
        assert_eq!(audit_entries.len(), 1);
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
