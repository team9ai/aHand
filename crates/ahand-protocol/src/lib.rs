pub mod ahand {
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/ahand.v1.rs"));
    }
}

pub use ahand::v1::*;

pub fn build_hello_auth_payload(
    device_id: &str,
    hello: &Hello,
    signed_at_ms: u64,
    challenge_nonce: &[u8],
) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(b"ahand-hub\0hello-auth\0");
    push_field(&mut payload, device_id.as_bytes());
    push_field(&mut payload, hello.version.as_bytes());
    push_field(&mut payload, hello.hostname.as_bytes());
    push_field(&mut payload, hello.os.as_bytes());
    payload.extend_from_slice(
        &u32::try_from(hello.capabilities.len())
            .expect("capability count should fit into u32")
            .to_le_bytes(),
    );
    for capability in &hello.capabilities {
        push_field(&mut payload, capability.as_bytes());
    }
    payload.extend_from_slice(&hello.last_ack.to_le_bytes());
    payload.extend_from_slice(&signed_at_ms.to_le_bytes());
    push_field(&mut payload, challenge_nonce);
    payload
}

fn push_field(payload: &mut Vec<u8>, value: &[u8]) {
    payload.extend_from_slice(
        &u32::try_from(value.len())
            .expect("field length should fit into u32")
            .to_le_bytes(),
    );
    payload.extend_from_slice(value);
}
