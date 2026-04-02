mod support;

use std::sync::Arc;
use std::time::Duration;

use ahand_hub::output_stream::OutputStream;
use ahand_hub_core::job::NewJob;
use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, HeaderName};
use support::{persistent_test_config, spawn_server_with_state, spawn_test_server};

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
        .get(format!(
            "{}/api/jobs/{job_id}/output",
            server.http_base_url()
        ))
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
        .get(format!(
            "{}/api/jobs/{job_id}/output",
            server.http_base_url()
        ))
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

#[tokio::test]
async fn reconnecting_sse_with_stale_last_event_id_emits_resync_event() {
    let mut state = support::test_state().await;
    state.output_stream = Arc::new(OutputStream::new(Duration::from_secs(60), 2));
    let job = state
        .jobs_store
        .insert(NewJob {
            device_id: "device-1".into(),
            tool: "echo".into(),
            args: vec!["hello".into()],
            cwd: None,
            env: Default::default(),
            timeout_ms: 30_000,
            requested_by: "service:test".into(),
        })
        .await
        .unwrap();
    let job_id = job.id.to_string();

    state
        .output_stream
        .push_stdout(&job_id, b"one\n".to_vec())
        .await
        .unwrap();
    state
        .output_stream
        .push_stdout(&job_id, b"two\n".to_vec())
        .await
        .unwrap();
    state
        .output_stream
        .push_stdout(&job_id, b"three\n".to_vec())
        .await
        .unwrap();
    state
        .output_stream
        .push_stdout(&job_id, b"four\n".to_vec())
        .await
        .unwrap();

    let server = spawn_server_with_state(state).await;
    let client = reqwest::Client::new();
    let response = client
        .get(format!(
            "{}/api/jobs/{job_id}/output",
            server.http_base_url()
        ))
        .header(AUTHORIZATION, "Bearer service-test-token")
        .header(HeaderName::from_static("last-event-id"), "1")
        .send()
        .await
        .unwrap();

    let mut stream = response.bytes_stream();
    let mut body = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, stream.next()).await {
            Ok(Some(Ok(chunk))) => {
                body.push_str(&String::from_utf8_lossy(&chunk));
                if body.contains("data: four") {
                    break;
                }
            }
            Ok(Some(Err(err))) => panic!("failed reading SSE chunk: {err}"),
            Ok(None) | Err(_) => break,
        }
    }

    assert!(body.contains("event: resync"));
    assert!(body.contains("data: three"));
    assert!(body.contains("data: four"));
    assert!(!body.contains("data: one"));
}

#[tokio::test]
async fn existing_job_without_live_output_state_still_streams_with_200() {
    let state = support::test_state().await;
    let job = state
        .jobs_store
        .insert(NewJob {
            device_id: "device-1".into(),
            tool: "echo".into(),
            args: vec!["hello".into()],
            cwd: None,
            env: Default::default(),
            timeout_ms: 30_000,
            requested_by: "service:test".into(),
        })
        .await
        .unwrap();
    let server = spawn_server_with_state(state).await;

    let response = reqwest::Client::new()
        .get(format!(
            "{}/api/jobs/{}/output",
            server.http_base_url(),
            job.id
        ))
        .header(AUTHORIZATION, "Bearer service-test-token")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
}

#[tokio::test]
async fn persistent_output_history_survives_restart() -> anyhow::Result<()> {
    let stack = ahand_hub_store::test_support::TestStack::start().await?;
    let config = persistent_test_config(&stack);

    let state = ahand_hub::state::AppState::from_config(config.clone()).await?;
    let server = spawn_server_with_state(state).await;
    let mut device = server
        .attach_bootstrap_device("device-2", "bootstrap-test-token")
        .await;

    let created = server
        .post_json(
            "/api/jobs",
            "service-test-token",
            serde_json::json!({
                "device_id": "device-2",
                "tool": "echo",
                "args": ["hello"],
                "timeout_ms": 30_000
            }),
        )
        .await;
    let job_id = created["job_id"].as_str().unwrap().to_string();
    let _ = device.recv_job_request().await;
    device.send_stdout(&job_id, b"first\n").await;
    device.send_stdout(&job_id, b"second\n").await;
    device.send_finished(&job_id, 0, "").await;

    tokio::time::sleep(Duration::from_millis(100)).await;
    server.shutdown().await;

    let restarted = ahand_hub::state::AppState::from_config(config).await?;
    let restarted = spawn_server_with_state(restarted).await;
    let response = reqwest::Client::new()
        .get(format!(
            "{}/api/jobs/{job_id}/output",
            restarted.http_base_url()
        ))
        .header(AUTHORIZATION, "Bearer service-test-token")
        .send()
        .await?;

    let mut stream = response.bytes_stream();
    let mut body = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, stream.next()).await {
            Ok(Some(Ok(chunk))) => {
                body.push_str(&String::from_utf8_lossy(&chunk));
                if body.contains("event: finished") {
                    break;
                }
            }
            Ok(Some(Err(err))) => panic!("failed reading SSE chunk after restart: {err}"),
            Ok(None) | Err(_) => break,
        }
    }

    assert!(body.contains("data: first"));
    assert!(body.contains("data: second"));
    assert!(body.contains("event: finished"));
    Ok(())
}
