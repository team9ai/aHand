mod support;

use futures_util::SinkExt;
use prost::Message;

use support::{bearer_hello, signed_hello, spawn_test_server};

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
async fn device_ws_accepts_bootstrap_bearer_hello() {
    let server = spawn_test_server().await;
    let (mut socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();

    let hello = bearer_hello("device-2", "bootstrap-test-token");
    socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();

    let listed = server.get_json("/api/devices", "service-test-token").await;
    assert_eq!(listed[0]["id"], "device-2");
    assert_eq!(listed[0]["online"], true);

    let _ = socket.close(None).await;
}
