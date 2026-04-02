use ahand_hub_core::HubError;
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
    assert!(matches!(
        resolve_status_transition(JobStatus::Sent, JobStatus::Pending),
        Err(HubError::IllegalJobTransition {
            current: JobStatus::Sent,
            requested: JobStatus::Pending,
        })
    ));
    assert!(matches!(
        resolve_status_transition(JobStatus::Finished, JobStatus::Running),
        Err(HubError::IllegalJobTransition {
            current: JobStatus::Finished,
            requested: JobStatus::Running,
        })
    ));
}
