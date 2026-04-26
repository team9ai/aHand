//! Integration test for Task 1.2: daemon-driven heartbeats fan out as
//! `device.heartbeat` events on the hub's `EventBus`.
//!
//! A fake daemon performs the ed25519 Hello handshake against a live hub,
//! then sends two `Heartbeat` envelopes back-to-back. An `EventBus`
//! subscriber (the contract Task 1.5's webhook sender will consume) must
//! see both, carrying `sentAtMs` and `presenceTtlSeconds = expected × 3`.

mod support;

use std::time::Duration;

use ahand_hub::events::DashboardEvent;
use ahand_protocol::{Envelope, Heartbeat, envelope};
use futures_util::SinkExt;
use prost::Message;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use support::{
    read_hello_accepted, read_hello_challenge, signed_hello, spawn_server_with_state, test_state,
};

/// Expected TTL = `device_expected_heartbeat_secs × 3`. The default in
/// `test_config()` is 60s, so the TTL hint must be 180.
const EXPECTED_PRESENCE_TTL_SECONDS: u64 = 180;

#[tokio::test]
async fn hub_forwards_heartbeat_to_event_bus() {
    // Spin up a hub with the default 60s expected heartbeat interval so
    // the presenceTtlSeconds in forwarded events is 180.
    let state = test_state().await;
    let mut events_rx = state.events.subscribe();
    let server = spawn_server_with_state(state).await;

    // Handshake: challenge → signed Hello → accepted.
    let (mut socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .expect("connect");
    let challenge = read_hello_challenge(&mut socket).await;
    let hello = signed_hello("device-1", &challenge.nonce);
    socket
        .send(WsMessage::Binary(hello.encode_to_vec().into()))
        .await
        .expect("send hello");
    let _ = read_hello_accepted(&mut socket).await;

    // Drain any non-heartbeat events that arrive pre-handshake (e.g.
    // device.online audit emission) so the assertion below only looks at
    // events from the heartbeats we explicitly send.
    flush_non_heartbeat(&mut events_rx).await;

    // Emit two heartbeats with distinct sent_at_ms values.
    let ts_a: u64 = 1_745_318_400_000;
    let ts_b: u64 = 1_745_318_460_000;
    send_heartbeat(&mut socket, "device-1", ts_a, 1, 0).await;
    send_heartbeat(&mut socket, "device-1", ts_b, 2, 0).await;

    let forwarded = collect_heartbeats(&mut events_rx, 2, Duration::from_secs(3)).await;
    assert!(
        forwarded.len() >= 2,
        "expected >=2 forwarded device.heartbeat events in 3s, got {}",
        forwarded.len()
    );
    let expected_ttl = serde_json::json!(EXPECTED_PRESENCE_TTL_SECONDS);
    for event in forwarded.iter().take(2) {
        assert_eq!(event.event, "device.heartbeat");
        assert_eq!(event.resource_type, "device");
        assert_eq!(event.resource_id, "device-1");
        assert_eq!(event.detail["presenceTtlSeconds"], expected_ttl);
    }
    let first_sent_at = forwarded[0].detail["sentAtMs"].as_u64().unwrap();
    let second_sent_at = forwarded[1].detail["sentAtMs"].as_u64().unwrap();
    assert_eq!(first_sent_at, ts_a);
    assert_eq!(second_sent_at, ts_b);

    let _ = socket.close(None).await;
    server.shutdown().await;
}

/// The hub emits `device.online` (and possibly other bookkeeping) when the
/// test device attaches; flush those before the heartbeats are sent so the
/// assertions downstream aren't racing against unrelated traffic.
async fn flush_non_heartbeat(rx: &mut broadcast::Receiver<DashboardEvent>) {
    loop {
        match tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
            Ok(Ok(event)) if event.event == "device.heartbeat" => {
                // Should be impossible — we haven't sent any yet.
                panic!(
                    "unexpected early device.heartbeat event: {:?}",
                    event.detail
                );
            }
            Ok(Ok(_)) => continue,
            _ => break,
        }
    }
}

async fn send_heartbeat<S>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
    device_id: &str,
    sent_at_ms: u64,
    seq: u64,
    ack: u64,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let envelope = Envelope {
        device_id: device_id.into(),
        msg_id: format!("hb-{sent_at_ms}"),
        seq,
        ack,
        ts_ms: sent_at_ms,
        payload: Some(envelope::Payload::Heartbeat(Heartbeat {
            sent_at_ms,
            daemon_version: "0.1.2".into(),
        })),
        ..Default::default()
    };
    socket
        .send(WsMessage::Binary(envelope.encode_to_vec().into()))
        .await
        .expect("send heartbeat");
}

/// Block until either `min` events have been observed or the deadline
/// expires. Returns all events seen so far (including any that arrived
/// after the `min` threshold but before the next `recv` timed out).
async fn collect_heartbeats(
    rx: &mut broadcast::Receiver<DashboardEvent>,
    min: usize,
    deadline: Duration,
) -> Vec<DashboardEvent> {
    let mut collected = Vec::new();
    let end = tokio::time::Instant::now() + deadline;
    while tokio::time::Instant::now() < end {
        let remaining = end.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(event)) => {
                if event.event == "device.heartbeat" {
                    collected.push(event);
                    if collected.len() >= min {
                        break;
                    }
                }
            }
            Ok(Err(_)) | Err(_) => break,
        }
    }
    // Caller decides how to handle under-counts so the error message can
    // point at what was missing vs what arrived.
    collected
}
