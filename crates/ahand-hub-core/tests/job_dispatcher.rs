use std::collections::HashMap;

use ahand_hub_core::HubError;
use ahand_hub_core::job::NewJob;
use ahand_hub_core::services::job_dispatcher::JobDispatcher;

#[tokio::test]
async fn create_job_requires_online_device() {
    let stores = ahand_hub_core::tests::fakes::offline_job_stores();
    let dispatcher = JobDispatcher::new(stores.devices, stores.jobs, stores.audit);

    let err = dispatcher
        .create_job(NewJob {
            device_id: "device-1".into(),
            tool: "git".into(),
            args: vec!["status".into()],
            cwd: Some("/tmp/demo".into()),
            env: HashMap::new(),
            timeout_ms: 30_000,
            requested_by: "service:test".into(),
        })
        .await
        .unwrap_err();

    assert_eq!(err, HubError::DeviceOffline("device-1".into()));
}
