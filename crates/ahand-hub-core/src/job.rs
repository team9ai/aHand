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
