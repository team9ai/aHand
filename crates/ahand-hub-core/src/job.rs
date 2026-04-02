use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{HubError, Result};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum JobStatus {
    Pending,
    Sent,
    Running,
    Finished,
    Failed,
    Cancelled,
}

pub fn is_terminal_status(status: JobStatus) -> bool {
    matches!(
        status,
        JobStatus::Finished | JobStatus::Failed | JobStatus::Cancelled
    )
}

pub fn resolve_status_transition(current: JobStatus, requested: JobStatus) -> Result<JobStatus> {
    if current == requested {
        return Ok(current);
    }

    if is_terminal_status(current) {
        return Err(HubError::IllegalJobTransition { current, requested });
    }

    match (current, requested) {
        (JobStatus::Pending, JobStatus::Sent)
        | (JobStatus::Pending, JobStatus::Running)
        | (JobStatus::Pending, JobStatus::Finished)
        | (JobStatus::Pending, JobStatus::Failed)
        | (JobStatus::Pending, JobStatus::Cancelled)
        | (JobStatus::Sent, JobStatus::Running)
        | (JobStatus::Sent, JobStatus::Finished)
        | (JobStatus::Sent, JobStatus::Failed)
        | (JobStatus::Sent, JobStatus::Cancelled)
        | (JobStatus::Running, JobStatus::Finished)
        | (JobStatus::Running, JobStatus::Failed)
        | (JobStatus::Running, JobStatus::Cancelled) => Ok(requested),
        _ => Err(HubError::IllegalJobTransition { current, requested }),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: uuid::Uuid,
    pub device_id: String,
    pub tool: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: HashMap<String, String>,
    pub timeout_ms: u64,
    pub status: JobStatus,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub output_summary: Option<String>,
    pub requested_by: String,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl Job {
    pub fn new_pending(id: uuid::Uuid, job: NewJob, created_at: DateTime<Utc>) -> Self {
        Self {
            id,
            device_id: job.device_id,
            tool: job.tool,
            args: job.args,
            cwd: job.cwd,
            env: job.env,
            timeout_ms: job.timeout_ms,
            status: JobStatus::Pending,
            exit_code: None,
            error: None,
            output_summary: None,
            requested_by: job.requested_by,
            created_at,
            started_at: None,
            finished_at: None,
        }
    }

    pub fn apply_status_transition(&mut self, next_status: JobStatus, at: DateTime<Utc>) {
        if next_status == JobStatus::Running && self.started_at.is_none() {
            self.started_at = Some(at);
        }
        if is_terminal_status(next_status) {
            self.finished_at = Some(at);
        }
        self.status = next_status;
    }

    pub fn record_terminal_outcome(
        &mut self,
        exit_code: i32,
        error: String,
        output_summary: String,
    ) {
        self.exit_code = Some(exit_code);
        self.error = Some(error);
        self.output_summary = Some(output_summary);
    }
}

#[derive(Debug, Clone)]
pub struct NewJob {
    pub device_id: String,
    pub tool: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: HashMap<String, String>,
    pub timeout_ms: u64,
    pub requested_by: String,
}

#[derive(Debug, Clone, Default)]
pub struct JobFilter {
    pub device_id: Option<String>,
    pub status: Option<JobStatus>,
}
