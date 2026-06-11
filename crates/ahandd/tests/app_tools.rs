//! Integration tests for `DaemonHandle::register_app_tool` /
//! `unregister_app_tool` and the resulting `AppToolsUpdate` snapshots that
//! the daemon pushes to the hub over its WebSocket connection.
//!
//! Test matrix:
//!   * Initial empty snapshot sent right after Hello.
//!   * Registering a tool causes a snapshot with the new tool.
//!   * Unregistering removes the tool and increments the revision.
//!   * Tools registered before the connection is established arrive in the
//!     initial snapshot (or at least with strictly increasing revisions —
//!     see note in `registration_racing_connect_yields_monotonic_revisions`).
//!   * Reconnect after a hub-initiated close re-sends the snapshot.

use std::sync::Arc;
use std::time::Duration;

use ahandd::{AppToolDef, AppToolHandler, DaemonConfig, DaemonStatus, spawn};
use serde_json::json;
use tempfile::TempDir;

mod mock_hub;

// ── helpers ──────────────────────────────────────────────────────────────────

fn echo_handler() -> AppToolHandler {
    Arc::new(|args| Box::pin(async move { Ok(json!({ "echo": args })) }))
}

fn demo_echo_def() -> AppToolDef {
    AppToolDef {
        name: "demo_echo".to_string(),
        description: "Echoes its arguments back".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "message": { "type": "string" }
            }
        }),
        requires_approval: false,
    }
}

/// Wait until the daemon reaches `Online`, then return the status receiver.
async fn wait_online(handle: &ahandd::DaemonHandle) {
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
    .expect("daemon did not reach Online within 5s");
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// After the Hello handshake the daemon should push an initial `AppToolsUpdate`
/// with revision=0 and an empty tool list.  After `register_app_tool` is
/// called, a second snapshot with revision=1 and the new tool should arrive.
#[tokio::test]
async fn snapshot_sent_after_hello_and_on_register() {
    let mock = mock_hub::start_accepting().await;
    let tmp = TempDir::new().unwrap();
    let config = DaemonConfig::builder(mock.ws_url(), mock.valid_jwt(), tmp.path())
        .heartbeat_interval(Duration::from_secs(60)) // keep heartbeats out of the way
        .build();

    let handle = spawn(config).await.expect("spawn ok");
    wait_online(&handle).await;

    // The initial snapshot (revision 0, no tools) should arrive quickly.
    let updates = mock
        .wait_for_app_tools_updates(1, Duration::from_secs(5))
        .await
        .expect("initial AppToolsUpdate not received within 5s");

    let initial = &updates[0];
    assert_eq!(initial.revision, 0, "initial revision must be 0");
    assert!(
        initial.tools.is_empty(),
        "initial snapshot must have no tools"
    );

    // Register the demo_echo tool.
    handle
        .register_app_tool(demo_echo_def(), echo_handler())
        .await
        .expect("register_app_tool should succeed");

    // A second snapshot with revision=1 and the new tool should arrive.
    let updates = mock
        .wait_for_app_tools_updates(2, Duration::from_secs(5))
        .await
        .expect("second AppToolsUpdate (after register) not received within 5s");

    let after_register = &updates[1];
    assert_eq!(
        after_register.revision, 1,
        "revision after register must be 1"
    );
    assert_eq!(
        after_register.tools.len(),
        1,
        "snapshot must contain 1 tool"
    );

    let tool = &after_register.tools[0];
    assert_eq!(tool.name, "demo_echo");
    assert_eq!(tool.description, "Echoes its arguments back");
    assert!(
        !tool.input_schema_json.is_empty(),
        "input_schema_json must be populated"
    );
    assert!(!tool.requires_approval);

    handle.shutdown().await.expect("shutdown clean");
}

/// register then unregister: final snapshot must have revision=2 and be empty.
#[tokio::test]
async fn unregister_pushes_snapshot_without_tool() {
    let mock = mock_hub::start_accepting().await;
    let tmp = TempDir::new().unwrap();
    let config = DaemonConfig::builder(mock.ws_url(), mock.valid_jwt(), tmp.path())
        .heartbeat_interval(Duration::from_secs(60))
        .build();

    let handle = spawn(config).await.expect("spawn ok");
    wait_online(&handle).await;

    // Wait for initial snapshot.
    mock.wait_for_app_tools_updates(1, Duration::from_secs(5))
        .await
        .expect("initial snapshot not received within 5s");

    // Register.
    handle
        .register_app_tool(demo_echo_def(), echo_handler())
        .await
        .expect("register ok");

    // Wait for revision=1 snapshot.
    mock.wait_for_app_tools_updates(2, Duration::from_secs(5))
        .await
        .expect("snapshot after register not received within 5s");

    // Unregister.
    let existed = handle.unregister_app_tool("demo_echo").await;
    assert!(
        existed,
        "unregister_app_tool should return true for an existing tool"
    );

    // Wait for revision=2 snapshot (empty tools).
    let updates = mock
        .wait_for_app_tools_updates(3, Duration::from_secs(5))
        .await
        .expect("snapshot after unregister not received within 5s");

    let final_snap = &updates[2];
    assert_eq!(
        final_snap.revision, 2,
        "revision after unregister must be 2"
    );
    assert!(
        final_snap.tools.is_empty(),
        "snapshot after unregister must be empty"
    );

    handle.shutdown().await.expect("shutdown clean");
}

/// Registering immediately after `spawn` may race the first connection attempt.
/// Regardless of timing, all received revisions must be strictly increasing and
/// the last snapshot must contain `demo_echo`.
///
/// Note: Because `DaemonHandle` is only available after `spawn()` returns,
/// strictly-before-connect registration is not constructible via the public
/// API. Instead, this test registers immediately after spawn (which may
/// race with the first connection attempt) and verifies that:
///   - Each received revision is strictly increasing (no duplicate revisions).
///   - Every snapshot sent carries a consistent tools list.
///
/// If the tool was registered before the connection completed, the initial
/// snapshot will have revision=1 (not 0). If registered after, revisions will
/// be 0, then 1. Either way, revisions must be strictly increasing.
#[tokio::test]
async fn registration_racing_connect_yields_monotonic_revisions() {
    let mock = mock_hub::start_accepting().await;
    let tmp = TempDir::new().unwrap();
    let config = DaemonConfig::builder(mock.ws_url(), mock.valid_jwt(), tmp.path())
        .heartbeat_interval(Duration::from_secs(60))
        .build();

    let handle = spawn(config).await.expect("spawn ok");

    // Register immediately — may race connection, intentionally.
    handle
        .register_app_tool(demo_echo_def(), echo_handler())
        .await
        .expect("register ok");

    wait_online(&handle).await;

    // Poll until a snapshot containing demo_echo appears.
    let found_snap = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let updates = mock.captured_app_tools_updates();
            if let Some(snap) = updates
                .iter()
                .find(|u| u.tools.iter().any(|t| t.name == "demo_echo"))
            {
                return snap.clone();
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("snapshot containing demo_echo not received within 5s");

    assert_eq!(
        found_snap.tools.len(),
        1,
        "snapshot must contain the registered tool"
    );
    assert_eq!(found_snap.tools[0].name, "demo_echo");

    // Revisions across all received snapshots must be strictly increasing.
    let updates = mock.captured_app_tools_updates();
    for window in updates.windows(2) {
        assert!(
            window[1].revision > window[0].revision,
            "revisions must be strictly increasing: got {} then {}",
            window[0].revision,
            window[1].revision
        );
    }

    handle.shutdown().await.expect("shutdown clean");
}

/// After the mock hub drops the first connection, the daemon reconnects.
/// The new Hello handshake must be followed by a fresh `AppToolsUpdate`
/// snapshot containing the same tool (same content, same revision).
#[tokio::test]
async fn snapshot_resent_after_reconnect() {
    // Drop after the first AppToolsUpdate (the initial empty snapshot), which
    // will force a reconnect. After registering a tool, the second connection's
    // initial snapshot must contain the tool.
    let mock = mock_hub::start_accepting_drop_after_n_snapshots(1).await;
    let tmp = TempDir::new().unwrap();
    let config = DaemonConfig::builder(mock.ws_url(), mock.valid_jwt(), tmp.path())
        .heartbeat_interval(Duration::from_secs(60))
        .build();

    let handle = spawn(config).await.expect("spawn ok");
    wait_online(&handle).await;

    // Wait for the initial snapshot to be received AND for the mock to drop
    // the connection (the mock drops after 1 snapshot). We can detect the
    // drop by waiting for the daemon to transition back to Connecting.
    let updates_after_first_conn = mock
        .wait_for_app_tools_updates(1, Duration::from_secs(5))
        .await
        .expect("initial snapshot not received within 5s");
    assert_eq!(
        updates_after_first_conn[0].revision, 0,
        "first snapshot should be rev=0"
    );

    // Wait for the daemon to reconnect (status goes to Connecting).
    let mut status = handle.subscribe_status();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(*status.borrow(), DaemonStatus::Connecting) {
                break;
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .expect("daemon did not reconnect within 5s");

    // Register a tool while the daemon is reconnecting (during the 1s backoff).
    handle
        .register_app_tool(demo_echo_def(), echo_handler())
        .await
        .expect("register ok");

    // Wait for the daemon to go Online again (second connection established).
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(*status.borrow(), DaemonStatus::Online { .. }) {
                break;
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .expect("daemon did not come back Online within 5s");

    // After the second Hello handshake, the daemon must re-send its current
    // snapshot which now includes demo_echo.
    let found_tool_snapshot = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let updates = mock.captured_app_tools_updates();
            // Look for a snapshot that contains demo_echo.
            for update in &updates {
                if update.tools.iter().any(|t| t.name == "demo_echo") {
                    return update.clone();
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("snapshot containing demo_echo not received within 5s after reconnect");

    assert_eq!(found_tool_snapshot.tools.len(), 1);
    assert_eq!(found_tool_snapshot.tools[0].name, "demo_echo");

    handle.shutdown().await.expect("shutdown clean");
}
