use ahand_protocol::hello;
use ahandd::ahand_client::HelloAuthMode;
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
async fn build_hello_envelope_includes_file_capability_when_enabled() {
    // R21: Hello.capabilities must contain "file" when file_enabled=true,
    // mirroring the existing browser capability handling. Without this
    // test the positive path was completely uncovered.
    let identity = DeviceIdentity::generate_for_tests();
    let hello = ahandd::ahand_client::build_hello_envelope(
        "device-cap",
        &identity,
        0,
        false, // browser disabled
        true,  // file enabled
        &[0xCA, 0xFE],
        None,
    );

    let payload = match hello.payload.unwrap() {
        ahand_protocol::envelope::Payload::Hello(hello) => hello,
        other => panic!("unexpected payload: {other:?}"),
    };

    assert!(
        payload.capabilities.iter().any(|c| c == "file"),
        "capabilities must contain 'file' when file_enabled=true: {:?}",
        payload.capabilities
    );
    assert!(
        !payload
            .capabilities
            .iter()
            .any(|c| c == "browser-playwright-cli"),
        "capabilities must NOT contain 'browser-playwright-cli' when browser_enabled=false"
    );
}

#[tokio::test]
async fn build_hello_envelope_excludes_file_capability_when_disabled() {
    let identity = DeviceIdentity::generate_for_tests();
    let hello = ahandd::ahand_client::build_hello_envelope(
        "device-no-file",
        &identity,
        0,
        false,
        false,
        &[0xBE, 0xEF],
        None,
    );

    let payload = match hello.payload.unwrap() {
        ahand_protocol::envelope::Payload::Hello(hello) => hello,
        other => panic!("unexpected payload: {other:?}"),
    };
    assert!(
        !payload.capabilities.iter().any(|c| c == "file"),
        "capabilities must NOT contain 'file' when file_enabled=false: {:?}",
        payload.capabilities
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
        false,
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

#[test]
fn hello_auth_modes_prefer_ed25519_before_bootstrap() {
    let modes = ahandd::ahand_client::hello_auth_modes(Some("bootstrap-token"));
    assert_eq!(
        modes,
        vec![
            HelloAuthMode::Ed25519,
            HelloAuthMode::Bootstrap("bootstrap-token".into())
        ]
    );
}

#[tokio::test]
async fn build_hello_envelope_advances_signed_at_ms_on_fast_reconnects() {
    let identity = DeviceIdentity::generate_for_tests();
    let first = ahandd::ahand_client::build_hello_envelope(
        "device-1",
        &identity,
        0,
        false,
        false,
        &[1, 2, 3, 4],
        None,
    );
    let second = ahandd::ahand_client::build_hello_envelope(
        "device-1",
        &identity,
        0,
        false,
        false,
        &[5, 6, 7, 8],
        None,
    );

    let first_auth = match first.payload.unwrap() {
        ahand_protocol::envelope::Payload::Hello(hello) => match hello.auth.unwrap() {
            hello::Auth::Ed25519(auth) => auth,
            other => panic!("unexpected auth payload: {other:?}"),
        },
        other => panic!("unexpected payload: {other:?}"),
    };
    let second_auth = match second.payload.unwrap() {
        ahand_protocol::envelope::Payload::Hello(hello) => match hello.auth.unwrap() {
            hello::Auth::Ed25519(auth) => auth,
            other => panic!("unexpected auth payload: {other:?}"),
        },
        other => panic!("unexpected payload: {other:?}"),
    };

    assert!(second_auth.signed_at_ms > first_auth.signed_at_ms);
}
