//! Frozen wire-format captures from deployed daemons.
//!
//! `golden_envelope` locks down that the encoder and decoder agree on the
//! current proto. It does NOT lock down the proto against the wire format
//! used by deployed daemons — `AHAND_FIXTURE_REGENERATE=1` will happily
//! regenerate every `.bin` to match a renumbered tag, so a wire-breaking
//! change can land with the test suite green.
//!
//! This file is the second layer: it embeds bytes that DEPLOYED daemons
//! have shipped on the wire and asserts they continue to decode as the
//! correct variant under the current proto. The byte arrays are sourced
//! from real daemon output and MUST NOT be regenerated. If a test in this
//! file starts failing, you've broken wire compat with every daemon in
//! the field — undo the proto change instead of editing this file.
//!
//! Caught: dev hub regression on 2026-04-28 — PR #1 (`feat: device file
//! operations`, c9b8a4d) renumbered `Heartbeat` from oneof tag 31 to 33
//! to insert FileRequest/FileResponse at 31/32. The team9 desktop client,
//! pinned to ahandd rev 6dac902, kept sending Heartbeat at tag 31; the
//! upgraded hub interpreted those bytes as `FileRequest` and closed the
//! WS with "FileRequest.request_id: invalid wire type: Varint (expected
//! LengthDelimited)". Every device flapped at ~5s cadence.

use ahand_protocol::{Envelope, Heartbeat, envelope};
use prost::Message;

/// Bytes a daemon at ahandd rev 6dac902 (Heartbeat at oneof tag 31) sends
/// for the canonical `golden_heartbeat` envelope:
///
/// ```text
/// Envelope {
///   device_id:  "device-golden",
///   trace_id:   "trace-golden",
///   msg_id:     "msg-golden",
///   seq:        7,
///   ack:        8,
///   ts_ms:      1_700_000_000_000,
///   payload:    Heartbeat { sent_at_ms: 1_700_000_000_000, daemon_version: "0.1.2" },
/// }
/// ```
///
/// Captured on the `feat/hub-outbox-persistence` branch (which still has
/// the pre-renumber proto) by reading
/// `crates/ahand-protocol/tests/fixtures/heartbeat.bin`. The two key bytes
/// `0xFA, 0x01` are the varint encoding of `(31 << 3) | 2 = 250`, i.e.
/// "oneof tag 31, length-delimited".
///
/// **Do not regenerate.** If a future proto change makes this no longer
/// decode as Heartbeat, that change is wire-incompatible with every
/// deployed ahandd <= rev 6dac902 (≈ all team9 desktop clients shipped
/// before the file-ops feature). Fix the proto.
#[rustfmt::skip]
const HEARTBEAT_FROM_DEPLOYED_DAEMON: &[u8] = &[
    0x0a, 0x0d, b'd', b'e', b'v', b'i', b'c', b'e', b'-', b'g', b'o', b'l', b'd', b'e', b'n',
    0x12, 0x0c, b't', b'r', b'a', b'c', b'e', b'-', b'g', b'o', b'l', b'd', b'e', b'n',
    0x1a, 0x0a, b'm', b's', b'g', b'-', b'g', b'o', b'l', b'd', b'e', b'n',
    0x20, 0x07,
    0x28, 0x08,
    0x30, 0x80, 0xd0, 0x95, 0xff, 0xbc, 0x31,
    // ── Envelope.payload oneof: tag 31, wire type 2 (LengthDelimited) ──
    // (31 << 3) | 2 = 250 → varint 0xFA, 0x01.
    0xfa, 0x01,
    0x0e,
    // Inner Heartbeat:
    0x08, 0x80, 0xd0, 0x95, 0xff, 0xbc, 0x31, // sent_at_ms = 1_700_000_000_000
    0x12, 0x05, b'0', b'.', b'1', b'.', b'2', // daemon_version = "0.1.2"
];

#[test]
fn deployed_heartbeat_bytes_decode_as_heartbeat() {
    let env = Envelope::decode(HEARTBEAT_FROM_DEPLOYED_DAEMON).unwrap_or_else(|err| {
        panic!(
            "frozen Heartbeat bytes failed to decode under current proto: {err}\n\n\
             These bytes are an exact capture of what ahandd <= rev 6dac902 \
             sends as a Heartbeat. A decode failure here means the current proto \
             is wire-incompatible with deployed daemons. Fix the proto — do NOT \
             edit this byte array.",
        )
    });

    assert_eq!(env.device_id, "device-golden");
    assert_eq!(env.seq, 7);
    assert_eq!(env.ack, 8);

    match env.payload {
        Some(envelope::Payload::Heartbeat(hb)) => {
            assert_eq!(hb.sent_at_ms, 1_700_000_000_000);
            assert_eq!(hb.daemon_version, "0.1.2");
        }
        other => panic!(
            "frozen Heartbeat bytes decoded as the wrong variant: {other:?}\n\n\
             ahandd <= rev 6dac902 puts Heartbeat at oneof tag 31. If this \
             decoded as anything else, a proto change has stolen tag 31 from \
             Heartbeat (e.g. PR #1 c9b8a4d on 2026-04-28 moved Heartbeat to \
             tag 33 to make room for FileRequest at 31; that broke every \
             deployed device). Restore Heartbeat to tag 31 and put the new \
             variant at a fresh, never-used tag.",
        ),
    }
}

#[test]
fn current_heartbeat_encoding_still_uses_oneof_tag_31() {
    // Positive direction: encode a Heartbeat with the current proto and
    // verify the resulting bytes contain the oneof-tag-31 marker (varint
    // 0xFA 0x01). This catches a renumber even if `golden_envelope` was
    // regenerated to match the broken state.
    let env = Envelope {
        payload: Some(envelope::Payload::Heartbeat(Heartbeat::default())),
        ..Envelope::default()
    };
    let bytes = env.encode_to_vec();

    let tag_31_marker: [u8; 2] = [0xfa, 0x01];
    assert!(
        bytes.windows(2).any(|w| w == tag_31_marker),
        "encoded Heartbeat envelope does not contain the oneof-tag-31 marker \
         (0xFA, 0x01). Heartbeat was renumbered, which is a wire-incompatible \
         change for every deployed daemon. Got bytes: {bytes:02x?}",
    );
}
