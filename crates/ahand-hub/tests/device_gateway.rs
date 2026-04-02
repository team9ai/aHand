mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ed25519_dalek::SigningKey;
use futures_util::{SinkExt, StreamExt};
use prost::Message;
use ahand_hub_core::traits::DeviceStore;

use support::{
    bootstrap_hello, read_hello_accepted, read_hello_challenge, signed_hello, signed_hello_at,
    signed_hello_with_key_at, signed_hello_with_last_ack, spawn_server_with_state,
    spawn_test_server, test_config, test_state,
};

#[tokio::test]
async fn device_ws_accepts_signed_hello_and_registers_presence() {
    let server = spawn_test_server().await;
    let (mut socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();

    let challenge = read_hello_challenge(&mut socket).await;
    let hello = signed_hello("device-1", &challenge.nonce);
    socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();

    let listed = server.get_json("/api/devices", "service-test-token").await;
    assert_eq!(listed[0]["id"], "device-1");
    assert_eq!(listed[0]["online"], true);

    let _ = socket.close(None).await;
}

#[tokio::test]
async fn idle_device_connection_times_out_and_marks_presence_offline() {
    let mut config = test_config();
    config.device_heartbeat_interval_ms = 20;
    config.device_heartbeat_timeout_ms = 60;
    let state = ahand_hub::state::AppState::from_config(config)
        .await
        .expect("test state should build");
    state
        .devices
        .insert(ahand_hub_core::device::NewDevice {
            id: "device-1".into(),
            public_key: Some(
                SigningKey::from_bytes(&[7u8; 32])
                    .verifying_key()
                    .to_bytes()
                    .to_vec(),
            ),
            hostname: "seeded-device".into(),
            os: "linux".into(),
            capabilities: vec!["exec".into()],
            version: Some("0.1.2".into()),
            auth_method: "ed25519".into(),
        })
        .await
        .unwrap();
    state.devices.mark_offline("device-1").await.unwrap();
    let server = spawn_server_with_state(state).await;
    let (mut socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();

    let challenge = read_hello_challenge(&mut socket).await;
    let hello = signed_hello("device-1", &challenge.nonce);
    socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();
    let _ = read_hello_accepted(&mut socket).await;

    tokio::time::sleep(std::time::Duration::from_millis(140)).await;

    let listed = server.get_json("/api/devices", "service-test-token").await;
    let device = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|device| device["id"] == "device-1")
        .unwrap();
    assert_eq!(device["online"], false);

    let _ = socket.close(None).await;
}

#[tokio::test]
async fn pong_responses_keep_idle_device_connection_online() {
    let mut config = test_config();
    config.device_heartbeat_interval_ms = 20;
    config.device_heartbeat_timeout_ms = 60;
    let state = ahand_hub::state::AppState::from_config(config)
        .await
        .expect("test state should build");
    state
        .devices
        .insert(ahand_hub_core::device::NewDevice {
            id: "device-1".into(),
            public_key: Some(
                SigningKey::from_bytes(&[7u8; 32])
                    .verifying_key()
                    .to_bytes()
                    .to_vec(),
            ),
            hostname: "seeded-device".into(),
            os: "linux".into(),
            capabilities: vec!["exec".into()],
            version: Some("0.1.2".into()),
            auth_method: "ed25519".into(),
        })
        .await
        .unwrap();
    state.devices.mark_offline("device-1").await.unwrap();
    let server = spawn_server_with_state(state).await;
    let (mut socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();

    let challenge = read_hello_challenge(&mut socket).await;
    let hello = signed_hello("device-1", &challenge.nonce);
    socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();
    let _ = read_hello_accepted(&mut socket).await;

    let ping_count = Arc::new(AtomicUsize::new(0));
    let reader_ping_count = ping_count.clone();
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
    let reader = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    let _ = socket.close(None).await;
                    break;
                }
                message = socket.next() => {
                    let Some(Ok(message)) = message else {
                        break;
                    };
                    match message {
                        tokio_tungstenite::tungstenite::Message::Ping(_) => {
                            reader_ping_count.fetch_add(1, Ordering::Relaxed);
                        }
                        tokio_tungstenite::tungstenite::Message::Binary(_) => {}
                        tokio_tungstenite::tungstenite::Message::Close(frame) => {
                            panic!("unexpected close frame: {frame:?}");
                        }
                        _ => {}
                    }
                }
            }
        }
    });

    tokio::time::sleep(std::time::Duration::from_millis(140)).await;

    let listed = server.get_json("/api/devices", "service-test-token").await;
    let device = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|device| device["id"] == "device-1")
        .unwrap();
    assert_eq!(device["online"], true);
    assert!(
        ping_count.load(Ordering::Relaxed) > 0,
        "expected heartbeat ping frames"
    );

    let _ = shutdown_tx.send(true);
    reader.await.unwrap();
}

#[tokio::test]
async fn device_ws_accepts_bootstrap_signed_hello() {
    let server = spawn_test_server().await;
    let (mut socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();

    let challenge = read_hello_challenge(&mut socket).await;
    let hello = bootstrap_hello("device-2", "bootstrap-test-token", &challenge.nonce);
    socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();

    let listed = server.get_json("/api/devices", "service-test-token").await;
    let device = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|device| device["id"] == "device-2")
        .unwrap();
    assert_eq!(device["online"], true);

    let _ = socket.close(None).await;
}

#[tokio::test]
async fn device_ws_rejects_replayed_signed_hello() {
    let server = spawn_test_server().await;
    let signed_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    let (mut socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();
    let challenge = read_hello_challenge(&mut socket).await;
    let hello = signed_hello_at("device-1", signed_at_ms, &challenge.nonce);
    socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();
    let _ = socket.close(None).await;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    let (mut replay_socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();
    let _new_challenge = read_hello_challenge(&mut replay_socket).await;
    replay_socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    let listed = server.get_json("/api/devices", "service-test-token").await;
    assert_eq!(listed[0]["id"], "device-1");
    assert_eq!(listed[0]["online"], false);

    let _ = replay_socket.close(None).await;
}

#[tokio::test]
async fn device_ws_rejects_stale_signed_hello() {
    let server = spawn_test_server().await;
    let stale_signed_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        - 120_000;

    let (mut socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();
    let challenge = read_hello_challenge(&mut socket).await;
    let hello = signed_hello_at("device-1", stale_signed_at_ms, &challenge.nonce);
    socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    let listed = server.get_json("/api/devices", "service-test-token").await;
    assert_eq!(listed[0]["id"], "device-1");
    assert_eq!(listed[0]["online"], false);

    let _ = socket.close(None).await;
}

#[tokio::test]
async fn device_ws_rejects_bootstrap_token_for_wrong_device_id() {
    let server = spawn_test_server().await;
    let (mut socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();

    let challenge = read_hello_challenge(&mut socket).await;
    let hello = bootstrap_hello("device-999", "bootstrap-test-token", &challenge.nonce);
    socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    let listed = server.get_json("/api/devices", "service-test-token").await;
    assert!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .all(|device| device["id"] != "device-999")
    );

    let _ = socket.close(None).await;
}

#[tokio::test]
async fn device_ws_rejects_signed_hello_with_unbound_key() {
    let server = spawn_test_server().await;
    let wrong_key = SigningKey::from_bytes(&[9u8; 32]);
    let (mut socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();
    let challenge = read_hello_challenge(&mut socket).await;

    let hello = signed_hello_with_key_at(
        "device-1",
        &wrong_key,
        0,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
        &challenge.nonce,
    );
    socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    let listed = server.get_json("/api/devices", "service-test-token").await;
    assert_eq!(listed[0]["id"], "device-1");
    assert_eq!(listed[0]["online"], false);

    let _ = socket.close(None).await;
}

#[tokio::test]
async fn device_ws_rejects_hello_signed_for_old_challenge() {
    let server = spawn_test_server().await;

    let (mut first_socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();
    let first_challenge = read_hello_challenge(&mut first_socket).await;
    let first_signed_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let first_hello = signed_hello_at("device-1", first_signed_at_ms, &first_challenge.nonce);
    first_socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            first_hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();
    let _ = read_hello_accepted(&mut first_socket).await;
    let _ = first_socket.close(None).await;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    let (mut second_socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();
    let _second_challenge = read_hello_challenge(&mut second_socket).await;
    let forged_hello = signed_hello_at("device-1", first_signed_at_ms + 1, &first_challenge.nonce);
    second_socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            forged_hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    let listed = server.get_json("/api/devices", "service-test-token").await;
    assert_eq!(listed[0]["id"], "device-1");
    assert_eq!(listed[0]["online"], false);

    let _ = second_socket.close(None).await;
}

#[tokio::test]
async fn bootstrap_registration_is_one_time_and_reconnect_switches_to_ed25519() {
    let server = spawn_test_server().await;

    let (mut bootstrap_socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();
    let bootstrap_challenge = read_hello_challenge(&mut bootstrap_socket).await;
    let first_bootstrap_hello = bootstrap_hello(
        "device-2",
        "bootstrap-test-token",
        &bootstrap_challenge.nonce,
    );
    bootstrap_socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            first_bootstrap_hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();
    let _ = bootstrap_socket.close(None).await;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    let (mut rejected_socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();
    let rejected_challenge = read_hello_challenge(&mut rejected_socket).await;
    let replay_bootstrap = bootstrap_hello(
        "device-2",
        "bootstrap-test-token",
        &rejected_challenge.nonce,
    );
    rejected_socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            replay_bootstrap.encode_to_vec().into(),
        ))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    let listed = server.get_json("/api/devices", "service-test-token").await;
    let device = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|device| device["id"] == "device-2")
        .unwrap();
    assert_eq!(device["online"], false);

    let _ = rejected_socket.close(None).await;

    let (mut reconnect_socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();
    let reconnect_challenge = read_hello_challenge(&mut reconnect_socket).await;
    let reconnect_hello = signed_hello("device-2", &reconnect_challenge.nonce);
    reconnect_socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            reconnect_hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    let listed = server.get_json("/api/devices", "service-test-token").await;
    let device = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|device| device["id"] == "device-2")
        .unwrap();
    assert_eq!(device["online"], true);
    assert_eq!(device["auth_method"], "ed25519");

    let _ = reconnect_socket.close(None).await;
}

#[tokio::test]
async fn device_ws_rejects_hello_with_tampered_hostname_after_signing() {
    let server = spawn_test_server().await;
    let (mut socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();

    let challenge = read_hello_challenge(&mut socket).await;
    let mut hello = signed_hello("device-1", &challenge.nonce);
    let Some(ahand_protocol::envelope::Payload::Hello(payload)) = hello.payload.as_mut() else {
        panic!("expected hello payload");
    };
    payload.hostname = "tampered-host".into();
    socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    let listed = server.get_json("/api/devices", "service-test-token").await;
    let device = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|device| device["id"] == "device-1")
        .unwrap();
    assert_eq!(device["online"], false);

    let _ = socket.close(None).await;
}

#[tokio::test]
async fn device_ws_rejects_hello_with_tampered_last_ack_after_signing() {
    let server = spawn_test_server().await;
    let (mut socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();

    let challenge = read_hello_challenge(&mut socket).await;
    let mut hello = signed_hello("device-1", &challenge.nonce);
    let Some(ahand_protocol::envelope::Payload::Hello(payload)) = hello.payload.as_mut() else {
        panic!("expected hello payload");
    };
    payload.last_ack = 99;
    socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    let listed = server.get_json("/api/devices", "service-test-token").await;
    let device = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|device| device["id"] == "device-1")
        .unwrap();
    assert_eq!(device["online"], false);

    let _ = socket.close(None).await;
}

#[tokio::test]
async fn old_connection_closing_does_not_mark_new_connection_offline() {
    let server = spawn_test_server().await;

    let (mut first_socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();
    let first_challenge = read_hello_challenge(&mut first_socket).await;
    let first_hello = signed_hello("device-1", &first_challenge.nonce);
    first_socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            first_hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();

    let (mut second_socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();
    let second_challenge = read_hello_challenge(&mut second_socket).await;
    let second_hello = signed_hello("device-1", &second_challenge.nonce);
    second_socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            second_hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();
    let _ = read_hello_accepted(&mut second_socket).await;

    let _ = first_socket.close(None).await;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    let listed = server.get_json("/api/devices", "service-test-token").await;
    let device = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|device| device["id"] == "device-1")
        .unwrap();
    assert_eq!(device["online"], true);

    let _ = second_socket.close(None).await;
}

#[tokio::test]
async fn invalid_device_frame_marks_device_offline() {
    let server = spawn_test_server().await;

    let (mut socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();
    let challenge = read_hello_challenge(&mut socket).await;
    let hello = signed_hello("device-1", &challenge.nonce);
    socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();

    socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            vec![0xde, 0xad, 0xbe, 0xef].into(),
        ))
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    let listed = server.get_json("/api/devices", "service-test-token").await;
    let device = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|device| device["id"] == "device-1")
        .unwrap();
    assert_eq!(device["online"], false);

    let _ = socket.close(None).await;
}

#[tokio::test]
async fn reconnect_replays_only_unacked_job_requests() {
    let server = spawn_test_server().await;

    let (mut first_socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();
    let first_challenge = read_hello_challenge(&mut first_socket).await;
    let first_hello = signed_hello_with_last_ack("device-1", 0, &first_challenge.nonce);
    first_socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            first_hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();
    let _ = read_hello_accepted(&mut first_socket).await;

    let created_first = server
        .post_json(
            "/api/jobs",
            "service-test-token",
            serde_json::json!({
                "device_id": "device-1",
                "tool": "echo",
                "args": ["one"],
                "timeout_ms": 30_000
            }),
        )
        .await;
    let created_second = server
        .post_json(
            "/api/jobs",
            "service-test-token",
            serde_json::json!({
                "device_id": "device-1",
                "tool": "echo",
                "args": ["two"],
                "timeout_ms": 30_000
            }),
        )
        .await;

    let first_frame = first_socket.next().await.unwrap().unwrap();
    let tokio_tungstenite::tungstenite::Message::Binary(first_data) = first_frame else {
        panic!("expected first job request");
    };
    let first_envelope = ahand_protocol::Envelope::decode(first_data.as_ref()).unwrap();
    assert_eq!(first_envelope.seq, 1);
    let first_job_id = created_first["job_id"].as_str().unwrap();
    let Some(ahand_protocol::envelope::Payload::JobRequest(first_job)) = first_envelope.payload
    else {
        panic!("expected first job request payload");
    };
    assert_eq!(first_job.job_id, first_job_id);

    let second_frame = first_socket.next().await.unwrap().unwrap();
    let tokio_tungstenite::tungstenite::Message::Binary(second_data) = second_frame else {
        panic!("expected second job request");
    };
    let second_envelope = ahand_protocol::Envelope::decode(second_data.as_ref()).unwrap();
    assert_eq!(second_envelope.seq, 2);
    let second_job_id = created_second["job_id"].as_str().unwrap();
    let Some(ahand_protocol::envelope::Payload::JobRequest(second_job)) = second_envelope.payload
    else {
        panic!("expected second job request payload");
    };
    assert_eq!(second_job.job_id, second_job_id);

    let _ = first_socket.close(None).await;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    let (mut reconnect_socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();
    let reconnect_challenge = read_hello_challenge(&mut reconnect_socket).await;
    let reconnect_hello = signed_hello_with_last_ack("device-1", 1, &reconnect_challenge.nonce);
    reconnect_socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            reconnect_hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();
    let _ = read_hello_accepted(&mut reconnect_socket).await;

    let replayed = reconnect_socket.next().await.unwrap().unwrap();
    let tokio_tungstenite::tungstenite::Message::Binary(replayed_data) = replayed else {
        panic!("expected replayed job request");
    };
    let replayed_envelope = ahand_protocol::Envelope::decode(replayed_data.as_ref()).unwrap();
    assert_eq!(replayed_envelope.seq, 2);
    let Some(ahand_protocol::envelope::Payload::JobRequest(replayed_job)) =
        replayed_envelope.payload
    else {
        panic!("expected replayed job request payload");
    };
    assert_eq!(replayed_job.job_id, second_job_id);

    let _ = reconnect_socket.close(None).await;
}

#[tokio::test]
async fn superseded_connection_cannot_mutate_job_state() {
    let server = spawn_test_server().await;

    let (mut first_socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();
    let first_challenge = read_hello_challenge(&mut first_socket).await;
    let first_hello = signed_hello("device-1", &first_challenge.nonce);
    first_socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            first_hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();
    let _ = read_hello_accepted(&mut first_socket).await;

    let (mut second_socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();
    let second_challenge = read_hello_challenge(&mut second_socket).await;
    let second_hello = signed_hello("device-1", &second_challenge.nonce);
    second_socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            second_hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();
    let _ = read_hello_accepted(&mut second_socket).await;

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
    let _ = second_socket.next().await.unwrap().unwrap();

    first_socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            ahand_protocol::Envelope {
                device_id: "device-1".into(),
                msg_id: format!("{job_id}-stale-finished"),
                ts_ms: 0,
                payload: Some(ahand_protocol::envelope::Payload::JobFinished(
                    ahand_protocol::JobFinished {
                        job_id: job_id.clone(),
                        exit_code: 0,
                        error: String::new(),
                    },
                )),
                ..Default::default()
            }
            .encode_to_vec()
            .into(),
        ))
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    let job = server
        .get_json(&format!("/api/jobs/{job_id}"), "service-test-token")
        .await;
    assert_eq!(job["status"], "sent");

    let _ = second_socket.close(None).await;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    let listed = server.get_json("/api/devices", "service-test-token").await;
    let device = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|device| device["id"] == "device-1")
        .unwrap();
    assert_eq!(device["online"], false);

    let _ = first_socket.close(None).await;
}

#[tokio::test]
async fn superseded_connection_stops_refreshing_presence() {
    let mut state = test_state().await;
    state.device_presence_refresh_ms = 25;
    let server = spawn_server_with_state(state).await;

    let (mut first_socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();
    let first_challenge = read_hello_challenge(&mut first_socket).await;
    let first_hello = signed_hello("device-1", &first_challenge.nonce);
    first_socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            first_hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();
    let _ = read_hello_accepted(&mut first_socket).await;

    let (mut second_socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();
    let second_challenge = read_hello_challenge(&mut second_socket).await;
    let second_hello = signed_hello("device-1", &second_challenge.nonce);
    second_socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            second_hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();
    let _ = read_hello_accepted(&mut second_socket).await;

    let _ = second_socket.close(None).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let listed = server.get_json("/api/devices", "service-test-token").await;
    let device = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|device| device["id"] == "device-1")
        .unwrap();
    assert_eq!(device["online"], false);

    let _ = first_socket.close(None).await;
}

#[tokio::test]
async fn invalid_frame_ack_does_not_clear_replay_buffer() {
    let server = spawn_test_server().await;

    let (mut first_socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();
    let first_challenge = read_hello_challenge(&mut first_socket).await;
    let first_hello = signed_hello_with_last_ack("device-1", 0, &first_challenge.nonce);
    first_socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            first_hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();
    let _ = read_hello_accepted(&mut first_socket).await;

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
    let _ = first_socket.next().await.unwrap().unwrap();
    let job_id = created["job_id"].as_str().unwrap().to_string();

    first_socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            ahand_protocol::Envelope {
                device_id: "device-1".into(),
                msg_id: "bogus-ack".into(),
                ts_ms: 0,
                ack: 99,
                payload: Some(ahand_protocol::envelope::Payload::JobFinished(
                    ahand_protocol::JobFinished {
                        job_id: "missing-job".into(),
                        exit_code: 0,
                        error: String::new(),
                    },
                )),
                ..Default::default()
            }
            .encode_to_vec()
            .into(),
        ))
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    let _ = first_socket.close(None).await;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    let (mut reconnect_socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();
    let reconnect_challenge = read_hello_challenge(&mut reconnect_socket).await;
    let reconnect_hello = signed_hello_with_last_ack("device-1", 0, &reconnect_challenge.nonce);
    reconnect_socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            reconnect_hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();
    let _ = read_hello_accepted(&mut reconnect_socket).await;

    let replayed = reconnect_socket.next().await.unwrap().unwrap();
    let tokio_tungstenite::tungstenite::Message::Binary(replayed_data) = replayed else {
        panic!("expected replayed job request");
    };
    let replayed_envelope = ahand_protocol::Envelope::decode(replayed_data.as_ref()).unwrap();
    let Some(ahand_protocol::envelope::Payload::JobRequest(replayed_job)) =
        replayed_envelope.payload
    else {
        panic!("expected replayed job request payload");
    };
    assert_eq!(replayed_job.job_id, job_id);

    let _ = reconnect_socket.close(None).await;
}

#[tokio::test]
async fn duplicate_frame_with_invalid_ack_does_not_clear_replay_buffer() {
    let server = spawn_test_server().await;

    let (mut socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();
    let challenge = read_hello_challenge(&mut socket).await;
    let hello = signed_hello_with_last_ack("device-1", 0, &challenge.nonce);
    socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();
    let _ = read_hello_accepted(&mut socket).await;

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
    let first_frame = socket.next().await.unwrap().unwrap();
    let tokio_tungstenite::tungstenite::Message::Binary(first_data) = first_frame else {
        panic!("expected first job request");
    };
    let first_envelope = ahand_protocol::Envelope::decode(first_data.as_ref()).unwrap();

    socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            ahand_protocol::Envelope {
                device_id: "device-1".into(),
                msg_id: format!("{job_id}-stdout"),
                ts_ms: 0,
                seq: 1,
                ack: 0,
                payload: Some(ahand_protocol::envelope::Payload::JobEvent(
                    ahand_protocol::JobEvent {
                        job_id: job_id.clone(),
                        event: Some(ahand_protocol::job_event::Event::StdoutChunk(
                            b"hello\n".to_vec(),
                        )),
                    },
                )),
                ..Default::default()
            }
            .encode_to_vec()
            .into(),
        ))
        .await
        .unwrap();

    socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            ahand_protocol::Envelope {
                device_id: "device-1".into(),
                msg_id: format!("{job_id}-stdout-duplicate"),
                ts_ms: 0,
                seq: 1,
                ack: first_envelope.seq + 98,
                payload: Some(ahand_protocol::envelope::Payload::JobEvent(
                    ahand_protocol::JobEvent {
                        job_id: job_id.clone(),
                        event: Some(ahand_protocol::job_event::Event::StdoutChunk(
                            b"ignored\n".to_vec(),
                        )),
                    },
                )),
                ..Default::default()
            }
            .encode_to_vec()
            .into(),
        ))
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    let _ = socket.close(None).await;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    let (mut reconnect_socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();
    let reconnect_challenge = read_hello_challenge(&mut reconnect_socket).await;
    let reconnect_hello = signed_hello_with_last_ack("device-1", 0, &reconnect_challenge.nonce);
    reconnect_socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            reconnect_hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();
    let _ = read_hello_accepted(&mut reconnect_socket).await;

    let replayed = reconnect_socket.next().await.unwrap().unwrap();
    let tokio_tungstenite::tungstenite::Message::Binary(replayed_data) = replayed else {
        panic!("expected replayed job request");
    };
    let replayed_envelope = ahand_protocol::Envelope::decode(replayed_data.as_ref()).unwrap();
    let Some(ahand_protocol::envelope::Payload::JobRequest(replayed_job)) =
        replayed_envelope.payload
    else {
        panic!("expected replayed job request payload");
    };
    assert_eq!(replayed_job.job_id, job_id);

    let _ = reconnect_socket.close(None).await;
}
