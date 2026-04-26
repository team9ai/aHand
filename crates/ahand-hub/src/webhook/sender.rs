//! HMAC signing + HTTP POST for a single webhook delivery.
//!
//! The worker in [`super::worker`] leases rows and invokes
//! [`send_once`] on each. This module has no retry logic of its own —
//! that belongs to the worker, which decides based on the
//! [`SendOutcome`] what to do next (retry, DLQ, delete).

use std::time::{SystemTime, UNIX_EPOCH};

use ahand_hub_store::webhook_delivery_store::WebhookDelivery;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use super::WebhookPayload;

type HmacSha256 = Hmac<Sha256>;

/// What happened when we tried to POST. The worker maps these to a
/// decision:
///
/// - `Success` → delete the row.
/// - `PermanentFailure` → delete + DLQ (bad signature or 4xx).
/// - `RetryLater` → bump attempts, schedule next retry.
#[derive(Debug)]
pub enum SendOutcome {
    /// Gateway accepted the event (2xx).
    Success,
    /// Gateway rejected in a way that retrying can't fix. `reason`
    /// is a short human-readable string used for logging / DLQ
    /// metadata; the plan calls out 401 specifically (HMAC
    /// mismatch), but we treat all non-5xx non-2xx status codes as
    /// permanent because the body or URL is server-configured and
    /// retrying won't magically produce a different response.
    PermanentFailure { reason: String },
    /// Transient failure — 5xx, timeout, connect error. The worker
    /// schedules a retry.
    RetryLater { reason: String },
}

/// Sign `raw_body` with HMAC-SHA256 using `secret`, binding the
/// timestamp into the signed material. The input is:
///   `timestamp_secs_as_string + "." + raw_body`
///
/// This matches the Stripe/GitHub webhook pattern and prevents an
/// attacker who captures a valid signed request from replaying it by
/// substituting a fresh `X-AHand-Timestamp` header — because the
/// timestamp is included in the HMAC input, changing the header
/// invalidates the signature.
///
/// Returns the header value (`sha256=<hex>`) directly so the caller
/// can set it without additional formatting.
pub fn sign(secret: &[u8], timestamp_secs: u64, raw_body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(timestamp_secs.to_string().as_bytes());
    mac.update(b".");
    mac.update(raw_body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

/// Verify a `sha256=<hex>` signature against `timestamp_secs` + `raw_body`
/// using constant-time comparison. Public for the test gateway and
/// for any future in-process verifier.
pub fn verify(secret: &[u8], timestamp_secs: u64, raw_body: &[u8], signature_header: &str) -> bool {
    use subtle::ConstantTimeEq;
    let expected = sign(secret, timestamp_secs, raw_body);
    expected
        .as_bytes()
        .ct_eq(signature_header.as_bytes())
        .into()
}

/// POST the payload once. No retries, no store mutation — pure I/O.
///
/// `delivery` is the row the worker leased from the delivery store; its
/// `created_at` is the original event-creation time and travels with
/// every retry in the `X-AHand-Event-Timestamp` header. The freshly
/// computed `X-AHand-Timestamp` is the signing time (included in the
/// HMAC input) and changes on every retry so the gateway can still
/// reject replays.
pub async fn send_once(
    http: &reqwest::Client,
    url: &str,
    secret: &[u8],
    payload: &WebhookPayload,
    delivery: &WebhookDelivery,
) -> SendOutcome {
    let body = match serde_json::to_vec(payload) {
        Ok(bytes) => bytes,
        Err(err) => {
            // A serialization failure means our own payload struct
            // produced invalid JSON — there's no client-visible
            // recovery, so DLQ the row rather than retry forever.
            return SendOutcome::PermanentFailure {
                reason: format!("serialize payload: {err}"),
            };
        }
    };
    // Compute timestamp once so the header value and the HMAC input
    // are guaranteed to be identical (anti-replay per spec § 5.5).
    let timestamp_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let signature = sign(secret, timestamp_secs, &body);
    // `X-AHand-Event-Timestamp` is the stable event-creation time, taken
    // from the delivery row so it survives retries. Unlike
    // `X-AHand-Timestamp`, it is NOT part of the HMAC input — the gateway
    // uses it for latency metrics and late-retry deduplication, not for
    // anti-replay. Negative `created_at` values (before the Unix epoch)
    // can't occur for rows we produce, but `max(0)` keeps the cast safe.
    let event_timestamp_secs = delivery.created_at.timestamp().max(0) as u64;

    let request = http
        .post(url)
        .header("Content-Type", "application/json")
        .header("X-AHand-Event-Id", payload.event_id.as_str())
        .header("X-AHand-Timestamp", timestamp_secs.to_string())
        .header("X-AHand-Event-Timestamp", event_timestamp_secs.to_string())
        .header("X-AHand-Signature", signature)
        .body(body);

    match request.send().await {
        Ok(response) => {
            let status = response.status();
            if status.is_success() {
                SendOutcome::Success
            } else if status.is_server_error() {
                SendOutcome::RetryLater {
                    reason: format!("status {}", status.as_u16()),
                }
            } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                // 429 Too Many Requests — transient; gateway is
                // rate-limiting us. Retry with backoff rather than
                // DLQing a perfectly valid payload.
                SendOutcome::RetryLater {
                    reason: format!("status {}", status.as_u16()),
                }
            } else {
                // All other 4xx — permanent (bad secret, bad payload,
                // wrong URL, etc.). Retrying won't change the outcome
                // because the body and URL are server-configured.
                SendOutcome::PermanentFailure {
                    reason: format!("status {}", status.as_u16()),
                }
            }
        }
        Err(err) => {
            // Network-level failures (DNS, connect, timeout) are
            // transient by default. `reqwest::Error::is_timeout` and
            // `is_connect` are informational — we still retry.
            SendOutcome::RetryLater {
                reason: format!("transport error: {err}"),
            }
        }
    }
}

/// Compute the exponential backoff delay (seconds) for the given
/// attempt count. Matches the plan's schedule:
/// `next_retry = now + min(2^attempts, 256)s`.
///
/// `attempts` is the attempts counter *after* this failure — so the
/// first failure passes 1 and waits 2s. The cap prevents runaway
/// waits: `2^9 = 512` would already blow the cap; at `attempts >= 8`
/// the delay is 256s.
pub fn backoff_secs(attempts: u32) -> u64 {
    match attempts {
        0 => 1,
        // 1..=8 -> 2,4,8,16,32,64,128,256 (capped)
        _ if attempts >= 8 => 256,
        _ => 1u64 << attempts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify_round_trip() {
        let body = b"hello world";
        let ts = 1_700_000_000u64;
        let header = sign(b"secret", ts, body);
        assert!(header.starts_with("sha256="));
        assert!(verify(b"secret", ts, body, &header));
    }

    #[test]
    fn verify_rejects_wrong_timestamp() {
        // Timestamp mismatch must invalidate the signature even when
        // body and secret are correct (anti-replay).
        let body = b"hello world";
        let header = sign(b"secret", 1_700_000_000, body);
        assert!(!verify(b"secret", 1_700_000_001, body, &header));
    }

    #[test]
    fn verify_rejects_bad_prefix() {
        let body = b"x";
        let ts = 42u64;
        let header = sign(b"s", ts, body).replace("sha256=", "md5=");
        assert!(!verify(b"s", ts, body, &header));
    }

    #[test]
    fn verify_rejects_bad_hex() {
        // "sha256=not-hex" — length differs from expected so ct_eq returns false.
        assert!(!verify(b"s", 0, b"x", "sha256=not-hex"));
    }

    #[test]
    fn verify_rejects_wrong_secret() {
        let body = b"x";
        let ts = 99u64;
        let header = sign(b"s1", ts, body);
        assert!(!verify(b"s2", ts, body, &header));
    }

    #[test]
    fn backoff_schedule_matches_plan() {
        assert_eq!(backoff_secs(0), 1);
        assert_eq!(backoff_secs(1), 2);
        assert_eq!(backoff_secs(2), 4);
        assert_eq!(backoff_secs(3), 8);
        assert_eq!(backoff_secs(4), 16);
        assert_eq!(backoff_secs(5), 32);
        assert_eq!(backoff_secs(6), 64);
        assert_eq!(backoff_secs(7), 128);
        assert_eq!(backoff_secs(8), 256);
        assert_eq!(backoff_secs(9), 256);
        assert_eq!(backoff_secs(u32::MAX), 256);
    }
}
