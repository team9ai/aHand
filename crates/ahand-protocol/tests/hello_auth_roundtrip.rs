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
fn hello_bearer_token_roundtrip() {
    let envelope = Envelope {
        device_id: "dev-123".into(),
        msg_id: "hello-2".into(),
        ts_ms: 1_717_000_000_001,
        payload: Some(ahand_protocol::envelope::Payload::Hello(Hello {
            version: "0.1.2".into(),
            hostname: "mbp".into(),
            os: "macos".into(),
            capabilities: vec!["exec".into()],
            last_ack: 8,
            auth: Some(hello::Auth::BearerToken("token-123".into())),
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
        hello::Auth::BearerToken(token) => {
            assert_eq!(token, "token-123");
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
    let payload = ahand_protocol::build_hello_auth_payload("device-7", 1_717_000_000_123, b"xyz");

    assert_eq!(payload, b"ahand-hub|device-7|1717000000123|xyz");
}

#[test]
fn hello_auth_payload_changes_when_nonce_changes() {
    let first = ahand_protocol::build_hello_auth_payload("device-7", 1_717_000_000_123, b"abc");
    let second = ahand_protocol::build_hello_auth_payload("device-7", 1_717_000_000_123, b"abd");

    assert_ne!(first, second);
}
