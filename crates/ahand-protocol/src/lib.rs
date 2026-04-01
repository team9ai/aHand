pub mod ahand {
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/ahand.v1.rs"));
    }
}

pub use ahand::v1::*;

pub fn build_hello_auth_payload(
    device_id: &str,
    signed_at_ms: u64,
    challenge_nonce: &[u8],
) -> Vec<u8> {
    let mut payload = Vec::with_capacity(
        b"ahand-hub||".len()
            + device_id.len()
            + signed_at_ms.to_string().len()
            + challenge_nonce.len(),
    );
    payload.extend_from_slice(b"ahand-hub|");
    payload.extend_from_slice(device_id.as_bytes());
    payload.push(b'|');
    payload.extend_from_slice(signed_at_ms.to_string().as_bytes());
    payload.push(b'|');
    payload.extend_from_slice(challenge_nonce);
    payload
}
