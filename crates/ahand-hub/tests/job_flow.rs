mod support;

use std::time::{Duration, Instant};

use reqwest::StatusCode;
use support::spawn_test_server;

async fn wait_for_job_status(
    server: &support::TestServer,
    job_id: &str,
    expected_status: &str,
    timeout: Duration,
) -> serde_json::Value {
    let deadline = Instant::now() + timeout;
    loop {
        let job = server
            .get_json(&format!("/api/jobs/{job_id}"), "service-test-token")
            .await;
        if job["status"] == expected_status {
            return job;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for job {job_id} to reach status {expected_status}, last payload: {job}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

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
    let payload: serde_json::Value = response.json().await.unwrap();
    assert_eq!(payload["status"], "cancelled");

    let cancel = device.recv_cancel_request().await;
    assert_eq!(cancel.job_id, job_id);

    let stored = wait_for_job_status(&server, &job_id, "cancelled", Duration::from_secs(1)).await;
    assert_eq!(stored["status"], "cancelled");

    let body = server
        .read_sse_for(
            &format!("/api/jobs/{job_id}/output"),
            "service-test-token",
            Duration::from_millis(250),
        )
        .await;
    assert!(body.contains("event: finished"));
    assert!(body.contains("\"error\":\"cancelled\""));
}

#[tokio::test]
async fn timed_out_job_sends_cancel_and_then_fails_with_timeout_output() {
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
                "timeout_ms": 25
            }),
        )
        .await;

    let job_id = created["job_id"].as_str().unwrap().to_string();
    let request = device.recv_job_request().await;
    assert_eq!(request.tool, "sleep");

    let cancel = tokio::time::timeout(Duration::from_secs(1), device.recv_cancel_request())
        .await
        .expect("timed out waiting for timeout-triggered cancel");
    assert_eq!(cancel.job_id, job_id);

    let stored = wait_for_job_status(&server, &job_id, "failed", Duration::from_secs(2)).await;
    assert_eq!(stored["status"], "failed");

    let body = server
        .read_sse_for(
            &format!("/api/jobs/{job_id}/output"),
            "service-test-token",
            Duration::from_millis(500),
        )
        .await;
    assert!(body.contains("event: finished"));
    assert!(body.contains("\"error\":\"timeout\""));
}

#[tokio::test]
async fn sent_job_fails_after_disconnect_grace_without_reconnect() {
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
    drop(device);

    tokio::time::sleep(Duration::from_millis(40)).await;
    let still_sent = server
        .get_json(&format!("/api/jobs/{job_id}"), "service-test-token")
        .await;
    assert_eq!(still_sent["status"], "sent");

    let stored = wait_for_job_status(&server, &job_id, "failed", Duration::from_secs(1)).await;
    assert_eq!(stored["status"], "failed");

    let body = server
        .read_sse_for(
            &format!("/api/jobs/{job_id}/output"),
            "service-test-token",
            Duration::from_millis(500),
        )
        .await;
    assert!(body.contains("event: finished"));
    assert!(body.contains("\"error\":\"device disconnected\""));
}

#[tokio::test]
async fn sent_job_replays_after_reconnect_within_disconnect_grace() {
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
    drop(device);

    tokio::time::sleep(Duration::from_millis(40)).await;

    let mut replacement = server.attach_test_device("device-1").await;
    let replayed = replacement.recv_job_request().await;
    assert_eq!(replayed.job_id, job_id);
    replacement.send_finished(&job_id, 0, "").await;

    let stored = wait_for_job_status(&server, &job_id, "finished", Duration::from_secs(1)).await;
    assert_eq!(stored["status"], "finished");
}

#[tokio::test]
async fn running_job_fails_after_disconnect_grace_without_reconnect() {
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
    let _ = device.recv_job_request().await;
    device.send_stdout(&job_id, b"tick\n").await;
    let stored = wait_for_job_status(&server, &job_id, "running", Duration::from_secs(1)).await;
    assert_eq!(stored["status"], "running");

    drop(device);

    let stored = wait_for_job_status(&server, &job_id, "failed", Duration::from_secs(1)).await;
    assert_eq!(stored["status"], "failed");

    let body = server
        .read_sse_for(
            &format!("/api/jobs/{job_id}/output"),
            "service-test-token",
            Duration::from_millis(500),
        )
        .await;
    assert!(body.contains("event: finished"));
    assert!(body.contains("\"error\":\"device disconnected\""));
}

#[tokio::test]
async fn running_job_survives_disconnect_when_device_reconnects_within_grace() {
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
    let _ = device.recv_job_request().await;
    device.send_stdout(&job_id, b"tick\n").await;
    let stored = wait_for_job_status(&server, &job_id, "running", Duration::from_secs(1)).await;
    assert_eq!(stored["status"], "running");

    drop(device);
    tokio::time::sleep(Duration::from_millis(40)).await;

    let replacement = server.attach_test_device("device-1").await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let stored = server
        .get_json(&format!("/api/jobs/{job_id}"), "service-test-token")
        .await;
    assert_eq!(stored["status"], "running");
    drop(replacement);
}
