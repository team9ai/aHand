//! Integration tests for the `ahandd::spawn` library API.
//!
//! Spawns a real daemon task pointed at an in-process mock WebSocket hub and
//! verifies:
//!   1. `Connecting → Online { device_id }` transitions for a well-behaved hub.
//!   2. `DaemonStatus::Error { kind: Auth, .. }` on handshake rejection.
//!   3. `shutdown()` returns cleanly and the inner task finishes.
//!   4. `load_or_create_identity` is idempotent on a temp directory.

use std::time::Duration;

use ahandd::{DaemonConfig, DaemonStatus, ErrorKind, load_or_create_identity, spawn};
use tempfile::TempDir;

mod mock_hub;

#[tokio::test]
async fn spawn_connects_and_reports_online() {
    let mock = mock_hub::start_accepting().await;
    let tmp = TempDir::new().unwrap();
    let config = DaemonConfig::builder(mock.ws_url(), mock.valid_jwt(), tmp.path())
        .heartbeat_interval(Duration::from_secs(1))
        .build();

    let handle = spawn(config).await.expect("spawn ok");
    // device_id is SHA256(pubkey) — just assert it is non-empty.
    assert!(!handle.device_id().is_empty());

    let mut status = handle.subscribe_status();
    let online = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            // Check borrow first in case we missed the initial value.
            if matches!(*status.borrow(), DaemonStatus::Online { .. }) {
                break;
            }
            status.changed().await.unwrap();
        }
    })
    .await;
    assert!(online.is_ok(), "did not reach Online within 5s");

    handle.shutdown().await.expect("shutdown clean");
}

#[tokio::test]
async fn spawn_surfaces_auth_error() {
    let mock = mock_hub::start_rejecting_401().await;
    let tmp = TempDir::new().unwrap();
    let config = DaemonConfig::builder(mock.ws_url(), "bad-jwt", tmp.path()).build();
    let handle = spawn(config)
        .await
        .expect("spawn returns handle even if auth later fails");

    let mut status = handle.subscribe_status();
    let got_auth_error = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(
                *status.borrow(),
                DaemonStatus::Error {
                    kind: ErrorKind::Auth,
                    ..
                }
            ) {
                break true;
            }
            status.changed().await.unwrap();
        }
    })
    .await;
    assert!(got_auth_error.is_ok(), "did not see Auth error within 5s");
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_finishes_inner_task() {
    // Point at a hub that never accepts — the client will sit in its reconnect
    // loop. `shutdown()` must cancel it promptly.
    let mock = mock_hub::start_rejecting_401().await;
    let tmp = TempDir::new().unwrap();
    let config = DaemonConfig::builder(mock.ws_url(), "bad-jwt", tmp.path()).build();
    let handle = spawn(config).await.unwrap();

    let before_status = handle.status();
    // Could be Connecting or already an Error; either way the handle is live.
    assert!(matches!(
        before_status,
        DaemonStatus::Connecting | DaemonStatus::Error { .. }
    ));

    tokio::time::timeout(Duration::from_secs(5), handle.shutdown())
        .await
        .expect("shutdown did not complete within 5s")
        .expect("shutdown returned Err");
}

#[tokio::test]
async fn load_or_create_identity_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let a = load_or_create_identity(tmp.path()).unwrap();
    let b = load_or_create_identity(tmp.path()).unwrap();
    assert_eq!(a.public_key_bytes(), b.public_key_bytes());
}

/// With a 500ms heartbeat interval, the daemon must push at least two
/// Heartbeat envelopes within ~1.5s (i.e. after the first-tick skip the
/// sender should fire at ~0.5s, ~1.0s, ~1.5s). Each heartbeat carries a
/// non-empty `daemon_version` so downstream consumers can correlate
/// reported daemon versions with the webhook stream.
#[tokio::test]
async fn daemon_sends_heartbeat_on_interval() {
    let mock = mock_hub::start_accepting().await;
    let tmp = TempDir::new().unwrap();
    let config = DaemonConfig::builder(mock.ws_url(), mock.valid_jwt(), tmp.path())
        .heartbeat_interval(Duration::from_millis(500))
        .build();

    let handle = spawn(config).await.expect("spawn ok");

    // Wait until we see the daemon Online so we know handshake completed,
    // so the 1.5s observation window starts counting from post-Hello.
    let mut status = handle.subscribe_status();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(*status.borrow(), DaemonStatus::Online { .. }) {
                break;
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .expect("daemon did not come Online within 5s");

    tokio::time::sleep(Duration::from_millis(1_600)).await;

    let beats = mock.captured_heartbeats();
    assert!(
        beats.len() >= 2,
        "expected >=2 heartbeats in ~1.5s, got {}",
        beats.len()
    );
    assert!(
        beats.iter().all(|hb| !hb.daemon_version.is_empty()),
        "every heartbeat must carry a non-empty daemon_version",
    );

    handle.shutdown().await.expect("shutdown clean");
}

/// Watchdog regression: when the WS connection becomes a TCP zombie (server
/// stops reading from the socket — accepts Pings but never sends Pongs back),
/// the daemon must detect "no inbound activity for 2× heartbeat_interval"
/// and tear down the connection so the outer reconnect loop can dial a
/// fresh socket.
///
/// Without the watchdog (pre-1adb5d4) the daemon stayed `Online` indefinitely
/// — the OS only surfaces the dead socket after hours, and the hub-side
/// `mark_offline` already fired minutes earlier, so the local UI showed
/// "Online" while ahand-integration on the cloud side saw zero registered
/// backends. This test pins the recovery path: daemon goes Online, then
/// drops back to Connecting once the watchdog fires.
#[tokio::test]
async fn watchdog_detects_silent_zombie_and_reconnects() {
    let mock = mock_hub::start_silent_after_handshake().await;
    let tmp = TempDir::new().unwrap();
    // 200ms heartbeat → 400ms watchdog. Test must allow enough wall-clock
    // budget for handshake + first ping cycle + watchdog timeout +
    // reconnect attempt; pick generous bounds to avoid flakiness on slow
    // CI runners.
    let config = DaemonConfig::builder(mock.ws_url(), mock.valid_jwt(), tmp.path())
        .heartbeat_interval(Duration::from_millis(200))
        .build();

    let handle = spawn(config).await.expect("spawn ok");
    let mut status = handle.subscribe_status();

    // Step 1: daemon must reach Online (handshake completed against the
    // silent mock).
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(*status.borrow(), DaemonStatus::Online { .. }) {
                break;
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .expect("daemon never reached Online");

    // Step 2: with no Pong ever arriving, the read-loop watchdog must
    // expire and the outer reconnect loop must report Disconnected,
    // surfaced by the StatusReporter as DaemonStatus::Connecting.
    // Budget = handshake skip (200ms) + Ping interval (200ms) + watchdog
    // (400ms) + reconnect attempt slack. 5 seconds is plenty.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(*status.borrow(), DaemonStatus::Connecting) {
                break;
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .expect(
        "watchdog never fired — daemon stayed Online despite the silent hub. \
         The connection-liveness check in ahand_client::connect_with_auth \
         is not triggering; check that the WS Ping task and the read-loop \
         tokio::time::timeout wrapper are still both wired in.",
    );

    handle.shutdown().await.expect("shutdown clean");
}

/// Manual TCP-keepalive smoke test. **Not a normal CI test**: marked
/// `#[ignore]` and gated behind the `disable-ws-ping` feature so it
/// only compiles when an operator opts in.
///
/// Why this needs manual orchestration: with the in-process silent mock
/// the kernel on the server side keeps auto-ACKing TCP keepalive probes
/// (the socket is open at the OS level), so OS keepalive never actually
/// fires no matter how long we wait. To genuinely exercise the
/// kernel-level fallback, run this test while externally dropping
/// traffic to the loopback peer — for example, on macOS:
///
/// ```bash
/// echo "block drop quick on lo0 proto tcp from any to 127.0.0.1 port = $PORT" \
///   | sudo pfctl -ef -
/// cargo test -p ahandd --features disable-ws-ping --test lib_spawn -- \
///   --ignored watchdog_disabled_still_recovers_via_tcp_keepalive
/// ```
///
/// Expected behavior with traffic dropped: 30s idle + 3 × 10s probes ≈
/// 60s after the last received byte, the kernel marks the socket dead,
/// the daemon's read loop surfaces an error, and the outer reconnect
/// loop drops back to `Connecting` — well within the 90s budget.
///
/// The point of the test: prove that even if the WS Ping watchdog were
/// to silently regress (typo in the ping task, wrong cfg gate, etc.),
/// OS-level TCP keepalive still recovers a zombie connection on its
/// own. That is what "defense-in-depth" means here.
#[cfg(feature = "disable-ws-ping")]
#[tokio::test]
#[ignore = "manual smoke test - requires pfctl/iptables to drop traffic to the peer"]
async fn watchdog_disabled_still_recovers_via_tcp_keepalive() {
    let mock = mock_hub::start_silent_after_handshake().await;
    let tmp = TempDir::new().unwrap();
    let config = DaemonConfig::builder(mock.ws_url(), mock.valid_jwt(), tmp.path())
        .heartbeat_interval(Duration::from_secs(1))
        .build();

    let handle = spawn(config).await.expect("spawn ok");
    let mut status = handle.subscribe_status();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(*status.borrow(), DaemonStatus::Online { .. }) {
                break;
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .expect("daemon never reached Online");

    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            if matches!(*status.borrow(), DaemonStatus::Connecting) {
                break;
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .expect(
        "TCP keepalive never tripped. With WS Ping disabled and traffic \
         dropped externally, the kernel must detect the dead peer within \
         ~60s and the daemon must fall back to Connecting. If you ran \
         this without dropping traffic, the kernel will keep auto-ACKing \
         probes forever and the test will (correctly) hang — that is \
         intentional, see the doc comment above.",
    );

    handle.shutdown().await.expect("shutdown clean");
}
