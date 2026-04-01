mod support;

use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, HeaderName};
use support::spawn_test_server;

#[tokio::test]
async fn reconnecting_sse_with_last_event_id_does_not_duplicate_output_history() {
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
    let _ = device.recv_job_request().await;

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/api/jobs/{job_id}/output", server.http_base_url()))
        .header(AUTHORIZATION, "Bearer service-test-token")
        .send()
        .await
        .unwrap();
    let mut stream = response.bytes_stream();

    device.send_stdout(&job_id, b"first\n").await;

    let mut first_body = String::new();
    let mut first_event_id = None;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.unwrap();
        first_body.push_str(&String::from_utf8_lossy(&chunk));
        if let Some(line) = first_body
            .lines()
            .find(|line| line.strip_prefix("id: ").is_some())
        {
            first_event_id = line.strip_prefix("id: ").map(str::to_string);
        }
        if first_body.contains("data: first") {
            break;
        }
    }
    drop(stream);

    device.send_stdout(&job_id, b"second\n").await;
    device.send_finished(&job_id, 0, "").await;

    let resumed = client
        .get(format!("{}/api/jobs/{job_id}/output", server.http_base_url()))
        .header(AUTHORIZATION, "Bearer service-test-token")
        .header(
            HeaderName::from_static("last-event-id"),
            first_event_id.expect("first event id should be captured"),
        )
        .send()
        .await
        .unwrap();

    let mut resumed_stream = resumed.bytes_stream();
    let mut resumed_body = String::new();
    while let Some(chunk) = resumed_stream.next().await {
        let chunk = chunk.unwrap();
        resumed_body.push_str(&String::from_utf8_lossy(&chunk));
        if resumed_body.contains("event: finished") {
            break;
        }
    }

    assert!(resumed_body.contains("data: second"));
    assert!(resumed_body.contains("event: finished"));
    assert!(!resumed_body.contains("data: first"));
}
