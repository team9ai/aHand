use ahand_hub_core::job::{JobStatus, is_terminal_status, resolve_status_transition};

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
        resolve_status_transition(JobStatus::Pending, JobStatus::Sent),
        JobStatus::Sent
    );
    assert_eq!(
        resolve_status_transition(JobStatus::Pending, JobStatus::Running),
        JobStatus::Running
    );
    assert_eq!(
        resolve_status_transition(JobStatus::Pending, JobStatus::Finished),
        JobStatus::Finished
    );
    assert_eq!(
        resolve_status_transition(JobStatus::Pending, JobStatus::Failed),
        JobStatus::Failed
    );
    assert_eq!(
        resolve_status_transition(JobStatus::Pending, JobStatus::Cancelled),
        JobStatus::Cancelled
    );
    assert_eq!(
        resolve_status_transition(JobStatus::Sent, JobStatus::Running),
        JobStatus::Running
    );
    assert_eq!(
        resolve_status_transition(JobStatus::Sent, JobStatus::Finished),
        JobStatus::Finished
    );
    assert_eq!(
        resolve_status_transition(JobStatus::Sent, JobStatus::Failed),
        JobStatus::Failed
    );
    assert_eq!(
        resolve_status_transition(JobStatus::Sent, JobStatus::Cancelled),
        JobStatus::Cancelled
    );
    assert_eq!(
        resolve_status_transition(JobStatus::Running, JobStatus::Finished),
        JobStatus::Finished
    );
    assert_eq!(
        resolve_status_transition(JobStatus::Running, JobStatus::Failed),
        JobStatus::Failed
    );
    assert_eq!(
        resolve_status_transition(JobStatus::Running, JobStatus::Cancelled),
        JobStatus::Cancelled
    );
}

#[test]
fn resolve_status_transition_keeps_current_for_noop_invalid_and_terminal_requests() {
    assert_eq!(
        resolve_status_transition(JobStatus::Sent, JobStatus::Sent),
        JobStatus::Sent
    );
    assert_eq!(
        resolve_status_transition(JobStatus::Sent, JobStatus::Pending),
        JobStatus::Sent
    );
    assert_eq!(
        resolve_status_transition(JobStatus::Finished, JobStatus::Running),
        JobStatus::Finished
    );
}
