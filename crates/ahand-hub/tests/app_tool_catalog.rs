//! Integration tests for Task 7: per-device app tool catalog with stale semantics.
//!
//! Verifies:
//! - Inbound AppToolsUpdate is stored and clears the `stale` flag.
//! - Duplicate revision on a fresh catalog is ignored.
//! - Disconnect marks the catalog stale (content retained).
//! - Reconnect + re-send of same revision is accepted (was stale).
//! - Audit entry `device.app_tools.updated` is written on accepted updates.

mod support;

use std::time::Duration;

use ahand_protocol::{AppToolDescriptor, AppToolsUpdate, Envelope, envelope};
use futures_util::SinkExt;
use prost::Message;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use support::{
    read_hello_accepted, read_hello_challenge, signed_hello, spawn_server_with_state, test_state,
};

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn make_app_tools_update(revision: u64, tool_name: &str) -> Envelope {
    Envelope {
        device_id: "device-1".into(),
        msg_id: format!("app-tools-update-{revision}"),
        seq: revision,
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::AppToolsUpdate(AppToolsUpdate {
            revision,
            tools: vec![AppToolDescriptor {
                name: tool_name.into(),
                description: "A test tool".into(),
                input_schema_json: r#"{"type":"object","properties":{}}"#.into(),
                requires_approval: false,
            }],
        })),
        ..Default::default()
    }
}

/// Poll for a catalog to become available in the store, up to the given timeout.
async fn poll_catalog(
    state: &ahand_hub::state::AppState,
    device_id: &str,
    timeout: Duration,
) -> Option<ahand_hub_store::app_tool_store::StoredAppToolCatalog> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(Some(catalog)) = state.app_tools.get_catalog(device_id).await {
            return Some(catalog);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Poll until the catalog is stale, up to the given timeout.
async fn poll_stale(
    state: &ahand_hub::state::AppState,
    device_id: &str,
    timeout: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(Some(catalog)) = state.app_tools.get_catalog(device_id).await {
            if catalog.stale {
                return true;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Poll until the catalog is NOT stale (fresh), up to the given timeout.
async fn poll_fresh(
    state: &ahand_hub::state::AppState,
    device_id: &str,
    expected_revision: u64,
    timeout: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(Some(catalog)) = state.app_tools.get_catalog(device_id).await {
            if !catalog.stale && catalog.revision == expected_revision {
                return true;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn app_tools_update_stored_and_cleared_stale() {
    let state = test_state().await;
    let server = spawn_server_with_state(state.clone()).await;

    let (mut socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .expect("connect");
    let challenge = read_hello_challenge(&mut socket).await;
    let hello = signed_hello("device-1", &challenge.nonce);
    socket
        .send(WsMessage::Binary(hello.encode_to_vec().into()))
        .await
        .expect("send hello");
    let _ = read_hello_accepted(&mut socket).await;

    // Send AppToolsUpdate revision=1 with one tool.
    let update = make_app_tools_update(1, "list_docs");
    socket
        .send(WsMessage::Binary(update.encode_to_vec().into()))
        .await
        .expect("send app tools update");

    // Poll for catalog to appear.
    let catalog = tokio::time::timeout(Duration::from_secs(3), async {
        poll_catalog(server.state(), "device-1", Duration::from_secs(3)).await
    })
    .await
    .expect("timeout waiting for catalog")
    .expect("catalog should be present");

    assert_eq!(catalog.revision, 1);
    assert!(!catalog.stale, "catalog should not be stale after update");
    assert_eq!(catalog.tools.len(), 1);
    assert_eq!(catalog.tools[0].name, "list_docs");

    let _ = socket.close(None).await;
    server.shutdown().await;
}

#[tokio::test]
async fn duplicate_revision_on_fresh_catalog_is_ignored() {
    let state = test_state().await;
    let server = spawn_server_with_state(state.clone()).await;

    let (mut socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .expect("connect");
    let challenge = read_hello_challenge(&mut socket).await;
    let hello = signed_hello("device-1", &challenge.nonce);
    socket
        .send(WsMessage::Binary(hello.encode_to_vec().into()))
        .await
        .expect("send hello");
    let _ = read_hello_accepted(&mut socket).await;

    // Send initial update rev=1.
    socket
        .send(WsMessage::Binary(
            make_app_tools_update(1, "tool_a").encode_to_vec().into(),
        ))
        .await
        .expect("send first update");

    // Wait until rev=1 is stored.
    let catalog_rev1 = tokio::time::timeout(Duration::from_secs(3), async {
        poll_catalog(server.state(), "device-1", Duration::from_secs(3)).await
    })
    .await
    .expect("timeout waiting for first catalog")
    .expect("first catalog should be stored");
    let first_updated_at = catalog_rev1.updated_at_ms;

    // Give a tiny gap to ensure updated_at_ms would differ if we did write.
    tokio::time::sleep(Duration::from_millis(5)).await;

    // Send same revision again — should be ignored (duplicate on fresh).
    let dup_update = Envelope {
        device_id: "device-1".into(),
        msg_id: "app-tools-dup".into(),
        seq: 2, // different seq but same revision
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::AppToolsUpdate(AppToolsUpdate {
            revision: 1,
            tools: vec![AppToolDescriptor {
                name: "tool_b".into(), // different tool name to detect any rewrite
                description: "Another tool".into(),
                input_schema_json: r#"{"type":"object"}"#.into(),
                requires_approval: false,
            }],
        })),
        ..Default::default()
    };
    socket
        .send(WsMessage::Binary(dup_update.encode_to_vec().into()))
        .await
        .expect("send duplicate update");

    // Give the hub time to process.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Catalog should still have the original content — not "tool_b".
    let catalog = server
        .state()
        .app_tools
        .get_catalog("device-1")
        .await
        .expect("no store error")
        .expect("catalog should still exist");
    assert_eq!(catalog.revision, 1);
    assert_eq!(catalog.tools.len(), 1);
    assert_eq!(catalog.tools[0].name, "tool_a", "duplicate must be ignored");
    assert_eq!(
        catalog.updated_at_ms, first_updated_at,
        "updated_at must not change for ignored update"
    );

    let _ = socket.close(None).await;
    server.shutdown().await;
}

#[tokio::test]
async fn disconnect_marks_catalog_stale_and_reconnect_accepts_same_revision() {
    let state = test_state().await;
    let server = spawn_server_with_state(state.clone()).await;

    // --- First connection ---
    let (mut socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .expect("connect");
    let challenge = read_hello_challenge(&mut socket).await;
    let hello = signed_hello("device-1", &challenge.nonce);
    socket
        .send(WsMessage::Binary(hello.encode_to_vec().into()))
        .await
        .expect("send hello");
    let _ = read_hello_accepted(&mut socket).await;

    // Send AppToolsUpdate rev=1.
    socket
        .send(WsMessage::Binary(
            make_app_tools_update(1, "my_tool").encode_to_vec().into(),
        ))
        .await
        .expect("send update");

    // Wait for catalog to land.
    tokio::time::timeout(Duration::from_secs(3), async {
        poll_catalog(server.state(), "device-1", Duration::from_secs(3)).await
    })
    .await
    .expect("timeout waiting for catalog")
    .expect("catalog should be stored");

    // Disconnect — catalog should become stale.
    socket.close(None).await.ok();
    let became_stale = tokio::time::timeout(
        Duration::from_secs(3),
        poll_stale(server.state(), "device-1", Duration::from_secs(3)),
    )
    .await
    .expect("timeout waiting for stale");
    assert!(became_stale, "catalog must be stale after disconnect");

    // Verify content is retained.
    let stale_catalog = server
        .state()
        .app_tools
        .get_catalog("device-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stale_catalog.tools.len(), 1);
    assert_eq!(stale_catalog.tools[0].name, "my_tool");

    // --- Second connection (reconnect) ---
    let (mut socket2, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .expect("reconnect");
    let challenge2 = read_hello_challenge(&mut socket2).await;
    let hello2 = signed_hello("device-1", &challenge2.nonce);
    socket2
        .send(WsMessage::Binary(hello2.encode_to_vec().into()))
        .await
        .expect("send hello on reconnect");
    let _ = read_hello_accepted(&mut socket2).await;

    // Resend same revision=1 — should be accepted because catalog is stale.
    socket2
        .send(WsMessage::Binary(
            make_app_tools_update(1, "my_tool").encode_to_vec().into(),
        ))
        .await
        .expect("send same revision after reconnect");

    // Wait for catalog to become fresh again.
    let is_fresh = tokio::time::timeout(
        Duration::from_secs(3),
        poll_fresh(server.state(), "device-1", 1, Duration::from_secs(3)),
    )
    .await
    .expect("timeout waiting for fresh catalog");
    assert!(is_fresh, "catalog must be fresh after reconnect + resend");

    let fresh_catalog = server
        .state()
        .app_tools
        .get_catalog("device-1")
        .await
        .unwrap()
        .unwrap();
    assert!(!fresh_catalog.stale);
    assert_eq!(fresh_catalog.revision, 1);
    assert_eq!(fresh_catalog.tools.len(), 1);
    assert_eq!(fresh_catalog.tools[0].name, "my_tool");

    socket2.close(None).await.ok();
    server.shutdown().await;
}

#[tokio::test]
async fn audit_entry_written_on_accepted_update() {
    use ahand_hub_core::audit::AuditFilter;

    let state = test_state().await;
    let server = spawn_server_with_state(state.clone()).await;

    let (mut socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .expect("connect");
    let challenge = read_hello_challenge(&mut socket).await;
    let hello = signed_hello("device-1", &challenge.nonce);
    socket
        .send(WsMessage::Binary(hello.encode_to_vec().into()))
        .await
        .expect("send hello");
    let _ = read_hello_accepted(&mut socket).await;

    socket
        .send(WsMessage::Binary(
            make_app_tools_update(1, "audit_tool")
                .encode_to_vec()
                .into(),
        ))
        .await
        .expect("send update");

    // Wait for catalog to land.
    tokio::time::timeout(Duration::from_secs(3), async {
        poll_catalog(server.state(), "device-1", Duration::from_secs(3)).await
    })
    .await
    .expect("timeout waiting for catalog");

    // Poll for the audit entry to appear (the BufferedAuditStore flushes on a
    // ~500ms batch cycle, so we need to wait for that flush to complete).
    let entries = {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            let entries = server
                .state()
                .audit_store
                .query(AuditFilter {
                    action: Some("device.app_tools.updated".into()),
                    resource_id: Some("device-1".into()),
                    ..Default::default()
                })
                .await
                .expect("audit query");
            if !entries.is_empty() {
                break entries;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("expected at least one device.app_tools.updated audit entry after 3s");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    let entry = &entries[0];
    assert_eq!(entry.action, "device.app_tools.updated");
    assert_eq!(entry.resource_type, "device");
    assert_eq!(entry.resource_id, "device-1");
    assert_eq!(entry.detail["revision"], 1);
    assert_eq!(entry.detail["toolCount"], 1);

    let _ = socket.close(None).await;
    server.shutdown().await;
}
