mod support;

use ed25519_dalek::SigningKey;
use futures_util::SinkExt;
use prost::Message;

use support::{
    bootstrap_hello, signed_hello, signed_hello_at, signed_hello_with_key_at, spawn_test_server,
};

#[tokio::test]
async fn device_ws_accepts_signed_hello_and_registers_presence() {
    let server = spawn_test_server().await;
    let (mut socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();

    let hello = signed_hello("device-1");
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
async fn device_ws_accepts_bootstrap_signed_hello() {
    let server = spawn_test_server().await;
    let (mut socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();

    let hello = bootstrap_hello("device-2", "bootstrap-test-token");
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
    let hello = signed_hello_at("device-1", signed_at_ms);
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
    let hello = signed_hello_at("device-1", stale_signed_at_ms);
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

    let hello = bootstrap_hello("device-999", "bootstrap-test-token");
    socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    let listed = server.get_json("/api/devices", "service-test-token").await;
    assert!(listed
        .as_array()
        .unwrap()
        .iter()
        .all(|device| device["id"] != "device-999"));

    let _ = socket.close(None).await;
}

#[tokio::test]
async fn device_ws_rejects_signed_hello_with_unbound_key() {
    let server = spawn_test_server().await;
    let wrong_key = SigningKey::from_bytes(&[9u8; 32]);
    let (mut socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();

    let hello = signed_hello_with_key_at(
        "device-1",
        &wrong_key,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
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
