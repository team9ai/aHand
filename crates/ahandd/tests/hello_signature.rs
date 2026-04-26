//! Deterministic signature golden test for the WS Hello handshake.
//!
//! Locks down — to byte equality — the Ed25519 signature produced by
//! `DeviceIdentity::sign_hello` for a fixed identity seed, fixed Hello
//! payload, fixed `signed_at_ms`, and fixed challenge nonce. Any change to
//! either the signature scheme (Ed25519 → something else, key derivation
//! quirks) or to the auth-payload byte format
//! (`ahand_protocol::build_hello_auth_payload`) flips this test.
//!
//! Why call `sign_hello` directly instead of `build_hello_envelope`?
//! `build_hello_envelope` reads `gethostname()`, `std::env::consts::OS`,
//! and `env!("CARGO_PKG_VERSION")` — none of which are stable across
//! machines or releases. Going through `sign_hello` lets us pin every
//! input that actually feeds the signature scheme. The Hello struct used
//! here mirrors what `build_hello_envelope` would assemble, just with
//! hand-fixed values.
//!
//! If this test fails:
//!   * unintentional drift → fix the regression in
//!     `ahand_protocol::build_hello_auth_payload` or
//!     `DeviceIdentity::sign_hello`.
//!   * intentional cryptographic change → the hub also needs to be updated;
//!     once that lands, regenerate by reading the actual signature bytes
//!     from a debug print and pasting them into `EXPECTED_SIGNATURE`.

use ahand_protocol::{Ed25519Auth, Hello, hello};
use ahandd::device_identity::DeviceIdentity;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

const FIXTURE_DEVICE_ID: &str = "device-golden";
const FIXTURE_HOSTNAME: &str = "goldenhost";
const FIXTURE_OS: &str = "linux";
const FIXTURE_VERSION: &str = "0.1.2";
const FIXTURE_SIGNED_AT_MS: u64 = 1_700_000_000_123;
const FIXTURE_LAST_ACK: u64 = 7;
const FIXTURE_CHALLENGE_NONCE: &[u8] = &[
    0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42,
];

// Identity seed used by `DeviceIdentity::generate_for_tests` is `[7u8; 32]`,
// which makes every byte below reproducible from public information.
const EXPECTED_PUBLIC_KEY: [u8; 32] = [
    234, 74, 108, 99, 226, 156, 82, 10, 190, 245, 80, 123, 19, 46, 197, 249, 149, 71, 118, 174,
    190, 190, 123, 146, 66, 30, 234, 105, 20, 70, 210, 44,
];

const EXPECTED_SIGNATURE: [u8; 64] = [
    8, 101, 211, 64, 221, 185, 208, 10, 111, 185, 219, 189, 198, 38, 253, 197, 84, 136, 38, 216,
    20, 108, 238, 78, 203, 88, 204, 171, 110, 31, 41, 3, 43, 0, 155, 160, 112, 26, 176, 45, 154,
    224, 111, 109, 242, 69, 163, 203, 218, 228, 33, 145, 86, 121, 158, 245, 30, 63, 169, 127, 251,
    208, 242, 4,
];

fn fixture_hello() -> Hello {
    Hello {
        version: FIXTURE_VERSION.into(),
        hostname: FIXTURE_HOSTNAME.into(),
        os: FIXTURE_OS.into(),
        capabilities: vec!["exec".into(), "browser".into()],
        last_ack: FIXTURE_LAST_ACK,
        auth: None,
    }
}

#[test]
fn deterministic_identity_yields_known_public_key() {
    // Sanity check: the seed `[7u8; 32]` derives a fixed verifying key.
    // If this fails, the test seed changed and every other golden in this
    // file needs to be regenerated.
    let identity = DeviceIdentity::generate_for_tests();
    assert_eq!(
        identity.public_key_bytes(),
        EXPECTED_PUBLIC_KEY,
        "DeviceIdentity::generate_for_tests seed changed; \
         regenerate EXPECTED_PUBLIC_KEY and EXPECTED_SIGNATURE"
    );
}

#[test]
fn sign_hello_produces_golden_signature_for_fixed_inputs() {
    let identity = DeviceIdentity::generate_for_tests();
    let hello = fixture_hello();

    let signature = identity.sign_hello(
        FIXTURE_DEVICE_ID,
        &hello,
        FIXTURE_SIGNED_AT_MS,
        FIXTURE_CHALLENGE_NONCE,
    );

    assert_eq!(
        signature.len(),
        64,
        "Ed25519 signatures must be 64 bytes; got {}",
        signature.len()
    );
    assert_eq!(
        signature.as_slice(),
        &EXPECTED_SIGNATURE[..],
        "Hello signature drifted. Either build_hello_auth_payload changed its \
         framing, or the signing scheme/key derivation regressed. Regenerate \
         EXPECTED_SIGNATURE only after confirming the change is intentional and \
         the hub is updated in lockstep."
    );
}

#[test]
fn golden_signature_actually_verifies_against_the_public_key() {
    // Defence in depth: even if EXPECTED_SIGNATURE is wrong, this catches it
    // — a signature that does not verify under the real public key cannot be
    // a valid golden in any version of the protocol.
    let identity = DeviceIdentity::generate_for_tests();
    let hello = fixture_hello();
    let auth_payload = ahand_protocol::build_hello_auth_payload(
        FIXTURE_DEVICE_ID,
        &hello,
        FIXTURE_SIGNED_AT_MS,
        FIXTURE_CHALLENGE_NONCE,
    );

    let verifying_key = VerifyingKey::from_bytes(&EXPECTED_PUBLIC_KEY)
        .expect("EXPECTED_PUBLIC_KEY must be a valid Ed25519 verifying key");
    let signature = Signature::from_bytes(&EXPECTED_SIGNATURE);

    verifying_key
        .verify(&auth_payload, &signature)
        .expect("EXPECTED_SIGNATURE must verify against EXPECTED_PUBLIC_KEY");

    // And for completeness: a freshly-signed payload should also verify.
    let fresh_signature_bytes = identity.sign_hello(
        FIXTURE_DEVICE_ID,
        &hello,
        FIXTURE_SIGNED_AT_MS,
        FIXTURE_CHALLENGE_NONCE,
    );
    let fresh_signature =
        Signature::from_slice(&fresh_signature_bytes).expect("sign_hello must return 64 bytes");
    verifying_key
        .verify(&auth_payload, &fresh_signature)
        .expect("freshly-signed payload must verify");
}

#[test]
fn signature_changes_when_challenge_nonce_changes() {
    // Negative test: the golden signature should depend on every input.
    // If this assertion goes the wrong way, the signature scheme has lost
    // information and is no longer binding to the challenge.
    let identity = DeviceIdentity::generate_for_tests();
    let hello = fixture_hello();

    let golden = identity.sign_hello(
        FIXTURE_DEVICE_ID,
        &hello,
        FIXTURE_SIGNED_AT_MS,
        FIXTURE_CHALLENGE_NONCE,
    );

    let mut tampered_nonce = FIXTURE_CHALLENGE_NONCE.to_vec();
    tampered_nonce[0] ^= 0x01;
    let other = identity.sign_hello(
        FIXTURE_DEVICE_ID,
        &hello,
        FIXTURE_SIGNED_AT_MS,
        &tampered_nonce,
    );

    assert_ne!(
        golden, other,
        "signature must depend on challenge_nonce — otherwise replay attacks \
         become trivial across handshakes"
    );
}

#[test]
fn build_hello_envelope_signature_verifies_under_deterministic_identity() {
    // Higher-level sanity: whatever `build_hello_envelope` produces in this
    // environment, the resulting Ed25519 signature must verify against the
    // identity's public key. This locks down the assembly path even though
    // its inputs (`gethostname()`, `std::env::consts::OS`,
    // `env!("CARGO_PKG_VERSION")`) are intentionally NOT pinned to byte
    // equality — those are per-machine / per-release and would force a
    // golden refresh on every version bump regardless of whether anything
    // cryptographically changed. Byte-equality on the signature is covered
    // by `sign_hello_produces_golden_signature_for_fixed_inputs` above.
    let identity = DeviceIdentity::generate_for_tests();
    let envelope = ahandd::ahand_client::build_hello_envelope(
        FIXTURE_DEVICE_ID,
        &identity,
        FIXTURE_LAST_ACK,
        true,
        FIXTURE_CHALLENGE_NONCE,
        None,
    );

    let payload = match envelope.payload.expect("hello envelope must have payload") {
        ahand_protocol::envelope::Payload::Hello(h) => h,
        other => panic!("expected Hello payload, got {other:?}"),
    };

    let auth = match payload.auth.clone().expect("hello payload must carry auth") {
        hello::Auth::Ed25519(auth) => auth,
        hello::Auth::Bootstrap(_) => panic!("expected Ed25519 auth (no bootstrap token passed)"),
    };

    let Ed25519Auth {
        public_key,
        signature,
        signed_at_ms,
    } = auth;

    let payload_bytes = ahand_protocol::build_hello_auth_payload(
        FIXTURE_DEVICE_ID,
        &Hello {
            // Re-build the exact auth payload by clearing the auth oneof,
            // mirroring how the daemon signs.
            auth: None,
            ..payload.clone()
        },
        signed_at_ms,
        FIXTURE_CHALLENGE_NONCE,
    );

    let pk_bytes: [u8; 32] = public_key
        .as_slice()
        .try_into()
        .expect("ed25519 public key must be 32 bytes");
    assert_eq!(
        pk_bytes, EXPECTED_PUBLIC_KEY,
        "deterministic identity must yield the canonical public key"
    );

    let verifying_key =
        VerifyingKey::from_bytes(&pk_bytes).expect("public_key bytes must be a valid Ed25519 vk");
    let sig = Signature::from_slice(&signature).expect("signature must be 64 bytes");
    verifying_key
        .verify(&payload_bytes, &sig)
        .expect("build_hello_envelope signature must verify under its own public key");
}
