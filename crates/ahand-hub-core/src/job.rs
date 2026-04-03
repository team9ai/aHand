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

    pub fn apply_status_transition(
        &mut self,
        requested_status: JobStatus,
        at: DateTime<Utc>,
    ) -> Result<JobStatus> {
        let next_status = resolve_status_transition(self.status, requested_status)?;
        if next_status == JobStatus::Running && self.started_at.is_none() {
            self.started_at = Some(at);
        }
        if is_terminal_status(next_status) {
            self.finished_at = Some(at);
        }
        self.status = next_status;
        Ok(next_status)
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

#[cfg(test)]
mod tests {
    use super::{Job, JobStatus, NewJob, is_terminal_status, resolve_status_transition};
    use crate::HubError;
    use chrono::Utc;
    use std::collections::HashMap;

    #[test]
    fn terminal_statuses_are_flagged() {
        assert!(!is_terminal_status(JobStatus::Pending));
        assert!(!is_terminal_status(JobStatus::Sent));
        assert!(!is_terminal_status(JobStatus::Running));
        assert!(is_terminal_status(JobStatus::Finished));
        assert!(is_terminal_status(JobStatus::Failed));
        assert!(is_terminal_status(JobStatus::Cancelled));
    }

    #[test]
    fn resolve_status_transition_advances_only_allowed_states() {
        assert_eq!(
            resolve_status_transition(JobStatus::Pending, JobStatus::Sent).unwrap(),
            JobStatus::Sent
        );
        assert_eq!(
            resolve_status_transition(JobStatus::Pending, JobStatus::Running).unwrap(),
            JobStatus::Running
        );
        assert_eq!(
            resolve_status_transition(JobStatus::Pending, JobStatus::Finished).unwrap(),
            JobStatus::Finished
        );
        assert_eq!(
            resolve_status_transition(JobStatus::Pending, JobStatus::Failed).unwrap(),
            JobStatus::Failed
        );
        assert_eq!(
            resolve_status_transition(JobStatus::Pending, JobStatus::Cancelled).unwrap(),
            JobStatus::Cancelled
        );
        assert_eq!(
            resolve_status_transition(JobStatus::Sent, JobStatus::Running).unwrap(),
            JobStatus::Running
        );
        assert_eq!(
            resolve_status_transition(JobStatus::Sent, JobStatus::Finished).unwrap(),
            JobStatus::Finished
        );
        assert_eq!(
            resolve_status_transition(JobStatus::Sent, JobStatus::Failed).unwrap(),
            JobStatus::Failed
        );
        assert_eq!(
            resolve_status_transition(JobStatus::Sent, JobStatus::Cancelled).unwrap(),
            JobStatus::Cancelled
        );
        assert_eq!(
            resolve_status_transition(JobStatus::Running, JobStatus::Finished).unwrap(),
            JobStatus::Finished
        );
        assert_eq!(
            resolve_status_transition(JobStatus::Running, JobStatus::Failed).unwrap(),
            JobStatus::Failed
        );
        assert_eq!(
            resolve_status_transition(JobStatus::Running, JobStatus::Cancelled).unwrap(),
            JobStatus::Cancelled
        );
    }

    #[test]
    fn resolve_status_transition_keeps_current_for_noop_requests() {
        assert_eq!(
            resolve_status_transition(JobStatus::Sent, JobStatus::Sent).unwrap(),
            JobStatus::Sent
        );
    }

    #[test]
    fn resolve_status_transition_rejects_illegal_requests() {
        assert_eq!(
            resolve_status_transition(JobStatus::Sent, JobStatus::Pending),
            Err(HubError::IllegalJobTransition {
                current: JobStatus::Sent,
                requested: JobStatus::Pending,
            })
        );
        assert_eq!(
            resolve_status_transition(JobStatus::Finished, JobStatus::Running),
            Err(HubError::IllegalJobTransition {
                current: JobStatus::Finished,
                requested: JobStatus::Running,
            })
        );
    }

    #[test]
    fn apply_status_transition_rejects_illegal_requests() {
        let mut job = Job::new_pending(
            uuid::Uuid::nil(),
            NewJob {
                device_id: "device-1".into(),
                tool: "echo".into(),
                args: vec!["hello".into()],
                cwd: None,
                env: HashMap::new(),
                timeout_ms: 30_000,
                requested_by: "service".into(),
            },
            Utc::now(),
        );

        let _ = job
            .apply_status_transition(JobStatus::Sent, Utc::now())
            .unwrap();

        assert_eq!(
            job.apply_status_transition(JobStatus::Pending, Utc::now()),
            Err(HubError::IllegalJobTransition {
                current: JobStatus::Sent,
                requested: JobStatus::Pending,
            })
        );
    }

    #[test]
    fn record_terminal_outcome_persists_exit_metadata() {
        let mut job = Job::new_pending(
            uuid::Uuid::nil(),
            NewJob {
                device_id: "device-1".into(),
                tool: "echo".into(),
                args: vec!["hello".into()],
                cwd: None,
                env: HashMap::new(),
                timeout_ms: 30_000,
                requested_by: "service".into(),
            },
            Utc::now(),
        );

        job.exit_code = Some(17);
        job.error = Some("timeout".into());
        job.output_summary = Some("stderr tail".into());

        assert_eq!(job.exit_code, Some(17));
        assert_eq!(job.error.as_deref(), Some("timeout"));
        assert_eq!(job.output_summary.as_deref(), Some("stderr tail"));
    }

    #[test]
    fn apply_status_transition_sets_timestamps_without_replacing_existing_start() {
        let created_at = Utc::now();
        let started_at = created_at + chrono::Duration::seconds(5);
        let finished_at = started_at + chrono::Duration::seconds(5);
        let mut job = Job::new_pending(
            uuid::Uuid::nil(),
            NewJob {
                device_id: "device-1".into(),
                tool: "echo".into(),
                args: vec!["hello".into()],
                cwd: None,
                env: HashMap::new(),
                timeout_ms: 30_000,
                requested_by: "service".into(),
            },
            created_at,
        );

        assert_eq!(
            job.apply_status_transition(JobStatus::Running, started_at)
                .unwrap(),
            JobStatus::Running
        );
        assert_eq!(job.started_at, Some(started_at));
        assert_eq!(job.finished_at, None);

        assert_eq!(
            job.apply_status_transition(JobStatus::Finished, finished_at)
                .unwrap(),
            JobStatus::Finished
        );
        assert_eq!(job.started_at, Some(started_at));
        assert_eq!(job.finished_at, Some(finished_at));

        job.exit_code = Some(0);
        job.error = Some(String::new());
        job.output_summary = Some("ok".into());
        assert_eq!(job.error.as_deref(), Some(""));

        let cloned = job.clone();
        let json = serde_json::to_string(&job).unwrap();
        let roundtrip: Job = serde_json::from_str(&json).unwrap();
        assert!(format!("{cloned:?}").contains("Finished"));
        assert_eq!(roundtrip.status, JobStatus::Finished);
        assert_eq!(roundtrip.finished_at, Some(finished_at));
        assert_eq!(format!("{:?}", roundtrip.status), "Finished");
    }

    #[test]
    fn resolve_status_transition_rejects_all_backward_transitions() {
        let backward_cases = vec![
            (JobStatus::Running, JobStatus::Sent),
            (JobStatus::Running, JobStatus::Pending),
            (JobStatus::Cancelled, JobStatus::Running),
            (JobStatus::Cancelled, JobStatus::Pending),
            (JobStatus::Cancelled, JobStatus::Sent),
            (JobStatus::Failed, JobStatus::Sent),
            (JobStatus::Failed, JobStatus::Pending),
            (JobStatus::Failed, JobStatus::Running),
            (JobStatus::Finished, JobStatus::Sent),
            (JobStatus::Finished, JobStatus::Pending),
        ];

        for (current, requested) in backward_cases {
            let result = resolve_status_transition(current, requested);
            assert_eq!(
                result,
                Err(HubError::IllegalJobTransition { current, requested }),
                "expected {current:?} -> {requested:?} to be rejected"
            );
        }
    }
}
