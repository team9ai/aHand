use ahand_protocol::hello;
use ahandd::config::Config;
use ahandd::device_identity::DeviceIdentity;

#[test]
fn config_parses_bootstrap_token_and_key_paths() {
    let cfg: Config = toml::from_str(
        r#"
mode = "ahand-cloud"
server_url = "ws://localhost:8080/ws"

[hub]
bootstrap_token = "bootstrap-token"
private_key_path = "/tmp/ahand/id_ed25519"
"#,
    )
    .unwrap();

    let hub = cfg.hub.unwrap();
    assert_eq!(hub.bootstrap_token.as_deref(), Some("bootstrap-token"));
    assert_eq!(
        hub.private_key_path.as_deref(),
        Some("/tmp/ahand/id_ed25519")
    );
}

#[tokio::test]
async fn build_hello_envelope_includes_ed25519_auth() {
    let identity = DeviceIdentity::generate_for_tests();
    let hello = ahandd::ahand_client::build_hello_envelope(
        "device-1",
        &identity,
        42,
        true,
        &[1, 2, 3, 4],
        None,
    );

    let payload = match hello.payload.unwrap() {
        ahand_protocol::envelope::Payload::Hello(hello) => hello,
        other => panic!("unexpected payload: {other:?}"),
    };

    match payload.auth.unwrap() {
        hello::Auth::Ed25519(auth) => {
            assert_eq!(auth.public_key.len(), 32);
            assert_eq!(auth.signature.len(), 64);
            assert!(auth.signed_at_ms > 0);
        }
        other => panic!("unexpected auth payload: {other:?}"),
    }
}

#[tokio::test]
async fn build_hello_envelope_includes_bootstrap_auth() {
    let identity = DeviceIdentity::generate_for_tests();
    let hello = ahandd::ahand_client::build_hello_envelope(
        "device-2",
        &identity,
        7,
        false,
        &[9, 8, 7, 6],
        Some("bootstrap-token".into()),
    );

    let payload = match hello.payload.unwrap() {
        ahand_protocol::envelope::Payload::Hello(hello) => hello,
        other => panic!("unexpected payload: {other:?}"),
    };

    match payload.auth.unwrap() {
        hello::Auth::Bootstrap(auth) => {
            assert_eq!(auth.bearer_token, "bootstrap-token");
            assert_eq!(auth.public_key.len(), 32);
            assert_eq!(auth.signature.len(), 64);
            assert!(auth.signed_at_ms > 0);
        }
        other => panic!("unexpected auth payload: {other:?}"),
    }
}
