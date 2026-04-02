use ahand_protocol::{
    BootstrapAuth, Ed25519Auth, Envelope, Hello, HelloAccepted, HelloChallenge, hello,
};
use prost::Message;

#[test]
fn hello_auth_roundtrip() {
    let envelope = Envelope {
        device_id: "dev-123".into(),
        msg_id: "hello-1".into(),
        ts_ms: 1_717_000_000_000,
        payload: Some(ahand_protocol::envelope::Payload::Hello(Hello {
            version: "0.1.2".into(),
            hostname: "mbp".into(),
            os: "macos".into(),
            capabilities: vec!["exec".into()],
            last_ack: 7,
            auth: Some(hello::Auth::Ed25519(Ed25519Auth {
                public_key: vec![1; 32],
                signature: vec![2; 64],
                signed_at_ms: 1_717_000_000_000,
            })),
        })),
        ..Default::default()
    };

    let encoded = envelope.encode_to_vec();
    let decoded = Envelope::decode(encoded.as_slice()).unwrap();
    let hello = match decoded.payload.unwrap() {
        ahand_protocol::envelope::Payload::Hello(hello) => hello,
        other => panic!("unexpected payload: {other:?}"),
    };

    match hello.auth.unwrap() {
        hello::Auth::Ed25519(auth) => {
            assert_eq!(auth.public_key.len(), 32);
            assert_eq!(auth.signature.len(), 64);
            assert_eq!(auth.signed_at_ms, 1_717_000_000_000);
        }
        other => panic!("unexpected auth payload: {other:?}"),
    }
}

#[test]
fn hello_bootstrap_auth_roundtrip() {
    let envelope = Envelope {
        device_id: "dev-456".into(),
        msg_id: "hello-3".into(),
        ts_ms: 1_717_000_000_002,
        payload: Some(ahand_protocol::envelope::Payload::Hello(Hello {
            version: "0.1.2".into(),
            hostname: "mbp".into(),
            os: "macos".into(),
            capabilities: vec!["exec".into()],
            last_ack: 9,
            auth: Some(hello::Auth::Bootstrap(BootstrapAuth {
                bearer_token: "token-456".into(),
                public_key: vec![3; 32],
                signature: vec![4; 64],
                signed_at_ms: 1_717_000_000_002,
            })),
        })),
        ..Default::default()
    };

    let encoded = envelope.encode_to_vec();
    let decoded = Envelope::decode(encoded.as_slice()).unwrap();
    let hello = match decoded.payload.unwrap() {
        ahand_protocol::envelope::Payload::Hello(hello) => hello,
        other => panic!("unexpected payload: {other:?}"),
    };

    match hello.auth.unwrap() {
        hello::Auth::Bootstrap(auth) => {
            assert_eq!(auth.bearer_token, "token-456");
            assert_eq!(auth.public_key.len(), 32);
            assert_eq!(auth.signature.len(), 64);
            assert_eq!(auth.signed_at_ms, 1_717_000_000_002);
        }
        other => panic!("unexpected auth payload: {other:?}"),
    }
}

#[test]
fn hello_challenge_roundtrip() {
    let envelope = Envelope {
        msg_id: "hello-challenge-1".into(),
        ts_ms: 1_717_000_000_003,
        payload: Some(ahand_protocol::envelope::Payload::HelloChallenge(
            HelloChallenge {
                nonce: vec![9; 16],
                issued_at_ms: 1_717_000_000_003,
            },
        )),
        ..Default::default()
    };

    let encoded = envelope.encode_to_vec();
    let decoded = Envelope::decode(encoded.as_slice()).unwrap();

    match decoded.payload.unwrap() {
        ahand_protocol::envelope::Payload::HelloChallenge(challenge) => {
            assert_eq!(challenge.nonce, vec![9; 16]);
            assert_eq!(challenge.issued_at_ms, 1_717_000_000_003);
        }
        other => panic!("unexpected payload: {other:?}"),
    }
}

#[test]
fn hello_accepted_roundtrip() {
    let envelope = Envelope {
        msg_id: "hello-accepted-1".into(),
        ts_ms: 1_717_000_000_004,
        payload: Some(ahand_protocol::envelope::Payload::HelloAccepted(
            HelloAccepted {
                auth_method: "ed25519".into(),
            },
        )),
        ..Default::default()
    };

    let encoded = envelope.encode_to_vec();
    let decoded = Envelope::decode(encoded.as_slice()).unwrap();

    match decoded.payload.unwrap() {
        ahand_protocol::envelope::Payload::HelloAccepted(accepted) => {
            assert_eq!(accepted.auth_method, "ed25519");
        }
        other => panic!("unexpected payload: {other:?}"),
    }
}

#[test]
fn hello_auth_payload_is_canonical_for_known_input() {
    let hello = Hello {
        version: "0.1.2".into(),
        hostname: "mbp".into(),
        os: "macos".into(),
        capabilities: vec!["exec".into(), "browser".into()],
        last_ack: 7,
        auth: None,
    };
    let payload =
        ahand_protocol::build_hello_auth_payload("device-7", &hello, 1_717_000_000_123, b"xyz");

    assert_eq!(
        payload,
        vec![
            97, 104, 97, 110, 100, 45, 104, 117, 98, 0, 104, 101, 108, 108, 111, 45, 97, 117,
            116, 104, 0, 8, 0, 0, 0, 100, 101, 118, 105, 99, 101, 45, 55, 5, 0, 0, 0, 48, 46,
            49, 46, 50, 3, 0, 0, 0, 109, 98, 112, 5, 0, 0, 0, 109, 97, 99, 111, 115, 2, 0, 0,
            0, 4, 0, 0, 0, 101, 120, 101, 99, 7, 0, 0, 0, 98, 114, 111, 119, 115, 101, 114, 7,
            0, 0, 0, 0, 0, 0, 0, 123, 210, 44, 197, 143, 1, 0, 0, 3, 0, 0, 0, 120, 121, 122
        ]
    );
}

#[test]
fn hello_auth_payload_changes_when_nonce_changes() {
    let hello = Hello {
        version: "0.1.2".into(),
        hostname: "mbp".into(),
        os: "macos".into(),
        capabilities: vec!["exec".into()],
        last_ack: 7,
        auth: None,
    };
    let first =
        ahand_protocol::build_hello_auth_payload("device-7", &hello, 1_717_000_000_123, b"abc");
    let second =
        ahand_protocol::build_hello_auth_payload("device-7", &hello, 1_717_000_000_123, b"abd");

    assert_ne!(first, second);
}

#[test]
fn hello_auth_payload_changes_when_hostname_changes() {
    let first_hello = Hello {
        version: "0.1.2".into(),
        hostname: "mbp".into(),
        os: "macos".into(),
        capabilities: vec!["exec".into()],
        last_ack: 7,
        auth: None,
    };
    let mut second_hello = first_hello.clone();
    second_hello.hostname = "tampered".into();

    let first = ahand_protocol::build_hello_auth_payload(
        "device-7",
        &first_hello,
        1_717_000_000_123,
        b"abc",
    );
    let second = ahand_protocol::build_hello_auth_payload(
        "device-7",
        &second_hello,
        1_717_000_000_123,
        b"abc",
    );

    assert_ne!(first, second);
}

#[test]
fn hello_auth_payload_changes_when_last_ack_changes() {
    let first_hello = Hello {
        version: "0.1.2".into(),
        hostname: "mbp".into(),
        os: "macos".into(),
        capabilities: vec!["exec".into()],
        last_ack: 7,
        auth: None,
    };
    let mut second_hello = first_hello.clone();
    second_hello.last_ack = 8;

    let first = ahand_protocol::build_hello_auth_payload(
        "device-7",
        &first_hello,
        1_717_000_000_123,
        b"abc",
    );
    let second = ahand_protocol::build_hello_auth_payload(
        "device-7",
        &second_hello,
        1_717_000_000_123,
        b"abc",
    );

    assert_ne!(first, second);
}
