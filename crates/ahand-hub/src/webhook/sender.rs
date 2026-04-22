//! HMAC signing + HTTP POST for a single webhook delivery.
//!
//! The worker in [`super::worker`] leases rows and invokes
//! [`send_once`] on each. This module has no retry logic of its own —
//! that belongs to the worker, which decides based on the
//! [`SendOutcome`] what to do next (retry, DLQ, delete).

use std::time::{SystemTime, UNIX_EPOCH};

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

/// Sign `raw_body` with HMAC-SHA256 using `secret`. Returns the
/// header value (`sha256=<hex>`) directly so the caller can set it
/// without formatting.
pub fn sign(secret: &[u8], raw_body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(raw_body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

/// Verify a `sha256=<hex>` signature against `raw_body` using
/// constant-time HMAC comparison. Public for the test gateway and
/// for any future in-process verifier.
pub fn verify(secret: &[u8], raw_body: &[u8], signature_header: &str) -> bool {
    let Some(hex) = signature_header.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(sig_bytes) = hex::decode(hex) else {
        return false;
    };
    let Ok(mut mac) = HmacSha256::new_from_slice(secret) else {
        return false;
    };
    mac.update(raw_body);
    mac.verify_slice(&sig_bytes).is_ok()
}

/// POST the payload once. No retries, no store mutation — pure I/O.
pub async fn send_once(
    http: &reqwest::Client,
    url: &str,
    secret: &[u8],
    payload: &WebhookPayload,
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
    let signature = sign(secret, &body);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);

    let request = http
        .post(url)
        .header("Content-Type", "application/json")
        .header("X-AHand-Event-Id", payload.event_id.as_str())
        .header("X-AHand-Timestamp", timestamp.to_string())
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
            } else {
                // 4xx — any client error (401, 400, 403, 410, ...)
                // is permanent because the signature, event shape,
                // or URL is wrong and retrying won't change any of
                // those.
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
        let header = sign(b"secret", body);
        assert!(header.starts_with("sha256="));
        assert!(verify(b"secret", body, &header));
    }

    #[test]
    fn verify_rejects_bad_prefix() {
        let body = b"x";
        let header = sign(b"s", body).replace("sha256=", "md5=");
        assert!(!verify(b"s", body, &header));
    }

    #[test]
    fn verify_rejects_bad_hex() {
        assert!(!verify(b"s", b"x", "sha256=not-hex"));
    }

    #[test]
    fn verify_rejects_wrong_secret() {
        let body = b"x";
        let header = sign(b"s1", body);
        assert!(!verify(b"s2", body, &header));
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
