use ahand_protocol::{hello, Ed25519Auth, Envelope, Hello};
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
