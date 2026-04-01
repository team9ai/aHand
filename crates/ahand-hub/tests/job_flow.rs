mod support;

use reqwest::StatusCode;
use support::spawn_test_server;

#[tokio::test]
async fn job_api_streams_stdout_and_completion_over_sse() {
    let server = spawn_test_server().await;
    let mut device = server.attach_test_device("device-1").await;

    let created = server
        .post_json(
            "/api/jobs",
            "service-test-token",
            serde_json::json!({
                "device_id": "device-1",
                "tool": "echo",
                "args": ["hello"],
                "timeout_ms": 30_000
            }),
        )
        .await;

    let job_id = created["job_id"].as_str().unwrap().to_string();
    assert_eq!(created["status"], "sent");
    let request = device.recv_job_request().await;
    assert_eq!(request.tool, "echo");

    device.send_stdout(&job_id, b"hello\n").await;
    device.send_finished(&job_id, 0, "").await;

    let body = server
        .read_sse(&format!("/api/jobs/{job_id}/output"), "service-test-token")
        .await;
    assert!(body.contains("event: stdout"));
    assert!(body.contains("event: finished"));
}

#[tokio::test]
async fn job_api_returns_not_found_for_unknown_device() {
    let server = spawn_test_server().await;
    let response = server
        .post(
            "/api/jobs",
            "service-test-token",
            serde_json::json!({
                "device_id": "device-999",
                "tool": "echo",
                "args": ["hello"],
                "timeout_ms": 30_000
            }),
        )
        .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn job_api_returns_conflict_for_offline_device() {
    let server = spawn_test_server().await;
    let response = server
        .post(
            "/api/jobs",
            "service-test-token",
            serde_json::json!({
                "device_id": "device-1",
                "tool": "echo",
                "args": ["hello"],
                "timeout_ms": 30_000
            }),
        )
        .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn output_stream_returns_not_found_for_unknown_job() {
    let server = spawn_test_server().await;
    let response = server
        .get("/api/jobs/unknown-job/output", "service-test-token")
        .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn device_cannot_spoof_another_devices_job_output() {
    let server = spawn_test_server().await;
    let mut device = server.attach_test_device("device-1").await;
    let mut attacker = server
        .attach_bootstrap_device("device-2", "bootstrap-test-token")
        .await;

    let created = server
        .post_json(
            "/api/jobs",
            "service-test-token",
            serde_json::json!({
                "device_id": "device-1",
                "tool": "echo",
                "args": ["hello"],
                "timeout_ms": 30_000
            }),
        )
        .await;

    let job_id = created["job_id"].as_str().unwrap().to_string();
    let request = device.recv_job_request().await;
    assert_eq!(request.tool, "echo");

    attacker.send_stdout(&job_id, b"spoofed\n").await;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    device.send_stdout(&job_id, b"hello\n").await;
    device.send_finished(&job_id, 0, "").await;

    let body = server
        .read_sse(&format!("/api/jobs/{job_id}/output"), "service-test-token")
        .await;
    assert!(!body.contains("spoofed"));
    assert!(body.contains("hello"));
}

#[tokio::test]
async fn cancel_job_api_forwards_cancel_request_and_streams_cancelled_finish() {
    let server = spawn_test_server().await;
    let mut device = server.attach_test_device("device-1").await;

    let created = server
        .post_json(
            "/api/jobs",
            "service-test-token",
            serde_json::json!({
                "device_id": "device-1",
                "tool": "sleep",
                "args": ["30"],
                "timeout_ms": 30_000
            }),
        )
        .await;

    let job_id = created["job_id"].as_str().unwrap().to_string();
    let request = device.recv_job_request().await;
    assert_eq!(request.tool, "sleep");

    let response = server
        .post(
            &format!("/api/jobs/{job_id}/cancel"),
            "service-test-token",
            serde_json::json!({}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    let cancel = device.recv_cancel_request().await;
    assert_eq!(cancel.job_id, job_id);

    device.send_finished(&job_id, -1, "cancelled").await;

    let body = server
        .read_sse(&format!("/api/jobs/{job_id}/output"), "service-test-token")
        .await;
    assert!(body.contains("event: finished"));
    assert!(body.contains("\"error\":\"cancelled\""));
}
