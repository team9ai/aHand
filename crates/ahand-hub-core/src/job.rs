use std::collections::HashMap;

use serde::{Deserialize, Serialize};

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

pub fn resolve_status_transition(current: JobStatus, requested: JobStatus) -> JobStatus {
    if current == requested || is_terminal_status(current) {
        return current;
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
        | (JobStatus::Running, JobStatus::Cancelled) => requested,
        _ => current,
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
    pub requested_by: String,
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
