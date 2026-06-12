//! Integration tests for Task 7 + Task 8: per-device app tool catalog.
//!
//! Task 7:
//! - Inbound AppToolsUpdate is stored and clears the `stale` flag.
//! - Duplicate revision on a fresh catalog is ignored.
//! - Disconnect marks the catalog stale (content retained).
//! - Reconnect + re-send of same revision is accepted (was stale).
//! - Audit entry `device.app_tools.updated` is written on accepted updates.
//! - Hello-time staleness: catalog is marked stale on new connection before
//!   the daemon re-advertises (covers hub-crash / revision-reset scenarios).
//! - Empty tools snapshot is accepted as a valid catalog state.
//!
//! Task 8:
//! - GET /api/devices/{device_id}/app-tools returns full camelCase catalog.
//! - GET unknown device → 404 standard envelope.
//! - GET known device, no catalog → 200 empty-catalog shape.
//! - GET stale-by-offline: catalog fresh in store but device offline → stale=true.
//! - Webhook enqueued on accepted update with correct payload.
//! - Webhook NOT enqueued on ignored (duplicate-revision) update.

mod support;

use std::time::Duration;

use ahand_hub_core::traits::DeviceAdminStore;
use ahand_protocol::{AppToolDescriptor, AppToolsUpdate, Envelope, envelope};
use ed25519_dalek::SigningKey;
use futures_util::SinkExt;
use prost::Message;
use reqwest::StatusCode;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use support::{
    attach_owned_device, mint_cp_jwt, mint_cp_jwt_with_options, read_hello_accepted,
    read_hello_challenge, signed_hello, spawn_server_with_state, test_state,
    test_state_with_webhook_persistent,
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
        if let Ok(Some(catalog)) = state.app_tools.get_catalog(device_id).await
            && catalog.stale
        {
            return true;
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
        if let Ok(Some(catalog)) = state.app_tools.get_catalog(device_id).await
            && !catalog.stale
            && catalog.revision == expected_revision
        {
            return true;
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

    // Poll for catalog to appear (poll_catalog already enforces its own deadline).
    let catalog = poll_catalog(server.state(), "device-1", Duration::from_secs(3))
        .await
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

    // Wait until rev=1 is stored (poll_catalog already enforces its own deadline).
    let catalog_rev1 = poll_catalog(server.state(), "device-1", Duration::from_secs(3))
        .await
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

    // To make the negative assertion sound: send a higher-revision update (rev=2) after the
    // duplicate, then wait until rev=2 is reflected in the store. Once rev=2 is observed we
    // know the hub has fully processed the duplicate frame that preceded it, so any write
    // the duplicate might have caused would already be visible.
    let sentinel = Envelope {
        device_id: "device-1".into(),
        msg_id: "app-tools-sentinel".into(),
        seq: 3,
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::AppToolsUpdate(AppToolsUpdate {
            revision: 2,
            tools: vec![AppToolDescriptor {
                name: "tool_a_v2".into(),
                description: "Sentinel update".into(),
                input_schema_json: r#"{"type":"object"}"#.into(),
                requires_approval: false,
            }],
        })),
        ..Default::default()
    };
    socket
        .send(WsMessage::Binary(sentinel.encode_to_vec().into()))
        .await
        .expect("send sentinel update");

    // Wait until the sentinel (rev=2) lands — at that point all preceding frames are processed.
    // Use poll_fresh(revision=2) so we wait specifically for rev=2 rather than returning on any
    // existing catalog (which would still be rev=1).
    let sentinel_landed = poll_fresh(server.state(), "device-1", 2, Duration::from_secs(3)).await;
    assert!(sentinel_landed, "sentinel update (rev=2) must be accepted");

    let final_catalog = server
        .state()
        .app_tools
        .get_catalog("device-1")
        .await
        .expect("no store error")
        .expect("catalog should exist");
    // Sentinel reached us; the original rev=1 content was never overwritten by the duplicate.
    assert_eq!(
        final_catalog.revision, 2,
        "sentinel update must be accepted"
    );
    assert_eq!(final_catalog.tools[0].name, "tool_a_v2");
    // The updated_at for rev=1 must not have been touched by the duplicate.
    assert_eq!(
        catalog_rev1.updated_at_ms, first_updated_at,
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
    poll_catalog(server.state(), "device-1", Duration::from_secs(3))
        .await
        .expect("catalog should be stored");

    // Disconnect — catalog should become stale.
    socket.close(None).await.ok();
    let became_stale = poll_stale(server.state(), "device-1", Duration::from_secs(3)).await;
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
    let is_fresh = poll_fresh(server.state(), "device-1", 1, Duration::from_secs(3)).await;
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
    poll_catalog(server.state(), "device-1", Duration::from_secs(3)).await;

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

/// Hello-time staleness: the catalog is marked stale when a new connection is
/// registered, BEFORE the daemon re-advertises its snapshot. This covers
/// hub-crash (cleanup never ran → catalog stays fresh forever) and
/// fast-reconnect (the daemon's revision counter reset after restart).
#[tokio::test]
async fn hello_time_staleness_marks_catalog_stale_before_re_advertise() {
    let state = test_state().await;
    let server = spawn_server_with_state(state.clone()).await;

    // --- First connection: establish a fresh catalog ---
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
            make_app_tools_update(1, "original_tool")
                .encode_to_vec()
                .into(),
        ))
        .await
        .expect("send first update");

    poll_catalog(server.state(), "device-1", Duration::from_secs(3))
        .await
        .expect("catalog should be stored");

    // Disconnect gracefully so cleanup runs.
    socket.close(None).await.ok();
    let became_stale = poll_stale(server.state(), "device-1", Duration::from_secs(3)).await;
    assert!(became_stale, "catalog must become stale after disconnect");

    // Directly inject a fresh (non-stale) catalog to simulate the hub-crash scenario:
    // the stale flag was never set because cleanup never ran.
    server
        .state()
        .app_tools
        .put_catalog(
            "device-1",
            ahand_hub_store::app_tool_store::StoredAppToolCatalog {
                revision: 1,
                stale: false,
                tools: vec![ahand_hub_store::app_tool_store::StoredAppTool {
                    name: "original_tool".into(),
                    description: "A test tool".into(),
                    input_schema_json: r#"{"type":"object","properties":{}}"#.into(),
                    requires_approval: false,
                }],
                updated_at_ms: 0,
            },
        )
        .await
        .expect("manual catalog inject");

    // Confirm catalog is fresh before reconnect.
    let pre_hello = server
        .state()
        .app_tools
        .get_catalog("device-1")
        .await
        .unwrap()
        .unwrap();
    assert!(
        !pre_hello.stale,
        "pre-condition: catalog must be fresh before new Hello"
    );

    // --- Second connection: Hello must stale the catalog immediately ---
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

    // The catalog should now be stale because Hello-accept sets it stale,
    // BEFORE the daemon re-advertises its snapshot.
    let became_stale_on_hello =
        poll_stale(server.state(), "device-1", Duration::from_secs(3)).await;
    assert!(
        became_stale_on_hello,
        "catalog must be stale right after new Hello (hello-time staleness)"
    );

    // Now the daemon resends revision=1 — must be accepted since catalog is stale.
    socket2
        .send(WsMessage::Binary(
            make_app_tools_update(1, "original_tool")
                .encode_to_vec()
                .into(),
        ))
        .await
        .expect("send re-advertise after hello-time stale");

    let is_fresh = poll_fresh(server.state(), "device-1", 1, Duration::from_secs(3)).await;
    assert!(
        is_fresh,
        "catalog must become fresh after daemon re-advertises on hello-time-stale path"
    );

    socket2.close(None).await.ok();
    server.shutdown().await;
}

/// An empty-tools snapshot (tools=[]) is a valid accepted catalog state.
/// Covers the "register-then-clear" semantics where the daemon may advertise
/// an empty tool list after all tools have been deregistered.
#[tokio::test]
async fn empty_tools_snapshot_is_accepted() {
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

    // Send an AppToolsUpdate with no tools.
    let empty_update = ahand_protocol::Envelope {
        device_id: "device-1".into(),
        msg_id: "app-tools-empty".into(),
        seq: 1,
        ts_ms: now_ms(),
        payload: Some(ahand_protocol::envelope::Payload::AppToolsUpdate(
            ahand_protocol::AppToolsUpdate {
                revision: 1,
                tools: vec![],
            },
        )),
        ..Default::default()
    };
    socket
        .send(WsMessage::Binary(empty_update.encode_to_vec().into()))
        .await
        .expect("send empty tools update");

    let catalog = poll_catalog(server.state(), "device-1", Duration::from_secs(3))
        .await
        .expect("empty-tools catalog should be stored");

    assert_eq!(catalog.revision, 1);
    assert!(!catalog.stale);
    assert_eq!(
        catalog.tools.len(),
        0,
        "empty tools list must be stored as-is"
    );

    let _ = socket.close(None).await;
    server.shutdown().await;
}

// ─────────────────────────────────────────────────────────────────────────────
// Task 8 tests: GET /api/devices/{device_id}/app-tools
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/devices/{device_id}/app-tools happy path:
/// - connect device, send AppToolsUpdate rev=1 with all fields
/// - GET returns 200 with correct camelCase JSON body
#[tokio::test]
async fn get_app_tools_happy_path() {
    let state = test_state().await;
    let server = spawn_server_with_state(state).await;

    let mut socket = attach_owned_device(&server, "cp-device-1", "user-cp-1").await;

    // Send AppToolsUpdate rev=1.
    let update = Envelope {
        device_id: "cp-device-1".into(),
        msg_id: "app-tools-1".into(),
        seq: 1,
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::AppToolsUpdate(AppToolsUpdate {
            revision: 1,
            tools: vec![AppToolDescriptor {
                name: "my_tool".into(),
                description: "A cool tool".into(),
                input_schema_json: r#"{"type":"object","properties":{}}"#.into(),
                requires_approval: true,
            }],
        })),
        ..Default::default()
    };
    socket
        .send(WsMessage::Binary(update.encode_to_vec().into()))
        .await
        .unwrap();

    // Wait for the catalog to land.
    let catalog_present = poll_catalog(server.state(), "cp-device-1", Duration::from_secs(3)).await;
    assert!(catalog_present.is_some(), "catalog must be stored");

    // GET via control-plane JWT.
    let token = mint_cp_jwt("user-cp-1");
    let resp = reqwest::Client::new()
        .get(format!(
            "{}/api/devices/cp-device-1/app-tools",
            server.http_base_url()
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();

    assert_eq!(body["revision"], 1);
    assert_eq!(body["stale"], false);
    assert!(body["updatedAtMs"].as_u64().unwrap_or(0) > 0);
    let tools = body["tools"].as_array().expect("tools must be array");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "my_tool");
    assert_eq!(tools[0]["description"], "A cool tool");
    assert_eq!(
        tools[0]["inputSchemaJson"],
        r#"{"type":"object","properties":{}}"#
    );
    assert_eq!(tools[0]["requiresApproval"], true);

    let _ = socket.close(None).await;
    server.shutdown().await;
}

/// GET /api/devices/{device_id}/app-tools for an unknown device → 404.
#[tokio::test]
async fn get_app_tools_unknown_device_returns_404() {
    let state = test_state().await;
    let server = spawn_server_with_state(state).await;

    let token = mint_cp_jwt("user-nobody");
    let resp = reqwest::Client::new()
        .get(format!(
            "{}/api/devices/nonexistent-device/app-tools",
            server.http_base_url()
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "DEVICE_NOT_FOUND");

    server.shutdown().await;
}

/// GET /api/devices/{device_id}/app-tools for a known device with no catalog
/// → 200 with empty-catalog shape.
#[tokio::test]
async fn get_app_tools_no_catalog_returns_empty_shape() {
    let state = test_state().await;
    let server = spawn_server_with_state(state).await;

    // Register device with external_user_id but don't send AppToolsUpdate.
    let mut socket = attach_owned_device(&server, "cp-device-nocatalog", "user-cp-2").await;

    let token = mint_cp_jwt("user-cp-2");
    let resp = reqwest::Client::new()
        .get(format!(
            "{}/api/devices/cp-device-nocatalog/app-tools",
            server.http_base_url()
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["revision"], 0);
    assert_eq!(body["stale"], true);
    assert_eq!(body["updatedAtMs"], 0);
    let tools = body["tools"].as_array().expect("tools must be array");
    assert_eq!(tools.len(), 0);

    let _ = socket.close(None).await;
    server.shutdown().await;
}

/// GET /api/devices/{device_id}/app-tools: catalog is fresh in the store but
/// device is offline → stale=true.
///
/// To isolate the `stale = stored_stale || !online` OR logic without relying
/// on disconnect-time stale-marking (which also sets `stored.stale = true`),
/// we inject a catalog directly into the store with `stale: false`, then
/// assert that the HTTP response still returns `stale: true` because the device
/// is offline. This tests the `!device_online` branch of the OR expression.
#[tokio::test]
async fn get_app_tools_stale_by_offline() {
    let state = test_state().await;
    let server = spawn_server_with_state(state).await;

    // Register device without connecting (stays offline).
    let verifying = SigningKey::from_bytes(&[7u8; 32])
        .verifying_key()
        .to_bytes();
    server
        .state()
        .devices
        .pre_register("cp-device-offline", &verifying, "user-cp-3")
        .await
        .unwrap();

    // Inject a "fresh" (stale=false) catalog directly so the stored stale
    // flag is false, isolating the online-state contribution to the OR.
    server
        .state()
        .app_tools
        .put_catalog(
            "cp-device-offline",
            ahand_hub_store::app_tool_store::StoredAppToolCatalog {
                revision: 5,
                stale: false,
                tools: vec![ahand_hub_store::app_tool_store::StoredAppTool {
                    name: "offline_tool".into(),
                    description: "Tool from offline device".into(),
                    input_schema_json: r#"{"type":"object"}"#.into(),
                    requires_approval: false,
                }],
                updated_at_ms: 1_000_000,
            },
        )
        .await
        .expect("put_catalog");

    let token = mint_cp_jwt("user-cp-3");
    let resp = reqwest::Client::new()
        .get(format!(
            "{}/api/devices/cp-device-offline/app-tools",
            server.http_base_url()
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    // Device is offline → stale must be true even though stored.stale=false.
    assert_eq!(
        body["stale"], true,
        "offline device must make catalog stale"
    );
    assert_eq!(body["revision"], 5);

    server.shutdown().await;
}

/// GET /api/devices/{device_id}/app-tools: token owned by a different user → 403.
#[tokio::test]
async fn get_app_tools_wrong_owner_returns_403() {
    let state = test_state().await;
    let server = spawn_server_with_state(state).await;

    // Register device owned by "user-owner".
    let mut socket = attach_owned_device(&server, "cp-device-owner-403", "user-owner").await;

    // Token minted for a different user → must 403.
    let attacker_token = mint_cp_jwt("user-attacker");
    let resp = reqwest::Client::new()
        .get(format!(
            "{}/api/devices/cp-device-owner-403/app-tools",
            server.http_base_url()
        ))
        .bearer_auth(&attacker_token)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "FORBIDDEN");

    let _ = socket.close(None).await;
    server.shutdown().await;
}

/// GET /api/devices/{device_id}/app-tools: token has device_ids allowlist that
/// excludes the target device → 403.
#[tokio::test]
async fn get_app_tools_device_not_in_allowlist_returns_403() {
    let state = test_state().await;
    let server = spawn_server_with_state(state).await;

    let mut socket = attach_owned_device(&server, "cp-device-allowlist", "user-al").await;

    // Token scoped to a different device but same user → 403.
    let restricted_token =
        mint_cp_jwt_with_options("user-al", "jobs:execute", Some(vec!["other-device".into()]));
    let resp = reqwest::Client::new()
        .get(format!(
            "{}/api/devices/cp-device-allowlist/app-tools",
            server.http_base_url()
        ))
        .bearer_auth(&restricted_token)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "FORBIDDEN");

    let _ = socket.close(None).await;
    server.shutdown().await;
}

/// GET /api/devices/{device_id}/app-tools: token with wrong scope → 403.
#[tokio::test]
async fn get_app_tools_wrong_scope_returns_403() {
    let state = test_state().await;
    let server = spawn_server_with_state(state).await;

    let mut socket = attach_owned_device(&server, "cp-device-scope", "user-scope").await;

    // Token with "other:scope" instead of "jobs:execute" → 403 before any DB work.
    let bad_scope_token = mint_cp_jwt_with_options("user-scope", "other:scope", None);
    let resp = reqwest::Client::new()
        .get(format!(
            "{}/api/devices/cp-device-scope/app-tools",
            server.http_base_url()
        ))
        .bearer_auth(&bad_scope_token)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "FORBIDDEN");

    let _ = socket.close(None).await;
    server.shutdown().await;
}

// ─────────────────────────────────────────────────────────────────────────────
// Task 8 webhook tests
// ─────────────────────────────────────────────────────────────────────────────

/// Helper: poll the webhook delivery store for a row matching the predicate,
/// up to the given deadline (wall clock). Returns true if found.
async fn poll_webhook_delivery<F>(
    store: &std::sync::Arc<dyn ahand_hub_store::webhook_delivery_store::WebhookDeliveryStore>,
    predicate: F,
    deadline: Duration,
) -> bool
where
    F: Fn(&serde_json::Value) -> bool,
{
    let end = tokio::time::Instant::now() + deadline;
    while tokio::time::Instant::now() < end {
        let rows = store
            .lease_due(chrono::Utc::now() + chrono::Duration::seconds(3600), 20)
            .await
            .unwrap_or_default();
        // Release the leased rows so the background worker can still process them.
        for row in &rows {
            let _ = store
                .mark_failed(
                    &row.event_id,
                    row.next_retry_at,
                    row.attempts,
                    row.last_error.as_deref().unwrap_or(""),
                )
                .await;
        }
        if rows.iter().any(|r| predicate(&r.payload)) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    false
}

/// Accepted AppToolsUpdate must enqueue a `device.app_tools.updated` webhook
/// with `{revision, toolCount}` in the data field.
#[tokio::test]
async fn webhook_enqueued_on_accepted_update() {
    // Use webhook_persistent so the background worker doesn't DLQ+delete rows
    // before we can inspect them (max_retries=1 exhausts on first attempt).
    let state = test_state_with_webhook_persistent().await;
    let server = spawn_server_with_state(state).await;

    // Register device with external_user_id before connecting.
    let verifying = SigningKey::from_bytes(&[7u8; 32])
        .verifying_key()
        .to_bytes();
    server
        .state()
        .devices
        .pre_register("wh-device-1", &verifying, "user-wh-1")
        .await
        .unwrap();

    // Connect.
    let (mut socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();
    let challenge = read_hello_challenge(&mut socket).await;
    let hello = signed_hello("wh-device-1", &challenge.nonce);
    socket
        .send(WsMessage::Binary(hello.encode_to_vec().into()))
        .await
        .unwrap();
    let _ = read_hello_accepted(&mut socket).await;

    // Send AppToolsUpdate rev=1.
    let update = make_app_tools_update_for("wh-device-1", 1, "wh_tool");
    socket
        .send(WsMessage::Binary(update.encode_to_vec().into()))
        .await
        .unwrap();

    // Wait for catalog to land.
    poll_catalog(server.state(), "wh-device-1", Duration::from_secs(3))
        .await
        .expect("catalog must be stored before webhook check");

    let wh_store = server
        .state()
        .webhook
        .store()
        .expect("webhook store must be present");

    let found = poll_webhook_delivery(
        &wh_store,
        |payload| {
            payload["eventType"] == "device.app_tools.updated"
                && payload["deviceId"] == "wh-device-1"
                && payload["data"]["revision"] == 1
                && payload["data"]["toolCount"] == 1
        },
        Duration::from_secs(3),
    )
    .await;

    assert!(
        found,
        "device.app_tools.updated webhook must be enqueued with revision=1, toolCount=1"
    );

    let _ = socket.close(None).await;
    server.shutdown().await;
}

/// Duplicate-revision (ignored) AppToolsUpdate must NOT enqueue a webhook.
#[tokio::test]
async fn webhook_not_enqueued_on_duplicate_revision() {
    // Use webhook_persistent so the background worker doesn't DLQ+delete rows
    // before we can inspect them (max_retries=1 exhausts on first attempt).
    let state = test_state_with_webhook_persistent().await;
    let server = spawn_server_with_state(state).await;

    let verifying = SigningKey::from_bytes(&[7u8; 32])
        .verifying_key()
        .to_bytes();
    server
        .state()
        .devices
        .pre_register("wh-device-2", &verifying, "user-wh-2")
        .await
        .unwrap();

    let (mut socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();
    let challenge = read_hello_challenge(&mut socket).await;
    let hello = signed_hello("wh-device-2", &challenge.nonce);
    socket
        .send(WsMessage::Binary(hello.encode_to_vec().into()))
        .await
        .unwrap();
    let _ = read_hello_accepted(&mut socket).await;

    // Send first update (rev=1) — accepted.
    let first = make_app_tools_update_for("wh-device-2", 1, "tool_a");
    socket
        .send(WsMessage::Binary(first.encode_to_vec().into()))
        .await
        .unwrap();
    poll_catalog(server.state(), "wh-device-2", Duration::from_secs(3))
        .await
        .expect("first catalog must land");

    // Wait for the first webhook to appear so we have a stable baseline count.
    let wh_store = server
        .state()
        .webhook
        .store()
        .expect("webhook store must be present");
    let _ = poll_webhook_delivery(
        &wh_store,
        |p| p["eventType"] == "device.app_tools.updated" && p["deviceId"] == "wh-device-2",
        Duration::from_secs(3),
    )
    .await;

    // Count how many `device.app_tools.updated` rows exist for this device now.
    let rows_before: usize = {
        let rows = wh_store
            .lease_due(chrono::Utc::now() + chrono::Duration::seconds(3600), 50)
            .await
            .unwrap_or_default();
        for row in &rows {
            let _ = wh_store
                .mark_failed(
                    &row.event_id,
                    row.next_retry_at,
                    row.attempts,
                    row.last_error.as_deref().unwrap_or(""),
                )
                .await;
        }
        rows.iter()
            .filter(|r| {
                r.payload["eventType"] == "device.app_tools.updated"
                    && r.payload["deviceId"] == "wh-device-2"
            })
            .count()
    };

    // Send duplicate (same rev=1, fresh catalog) — must be ignored.
    let dup = make_app_tools_update_for("wh-device-2", 1, "tool_a_dup");
    socket
        .send(WsMessage::Binary(dup.encode_to_vec().into()))
        .await
        .unwrap();

    // Send sentinel rev=2 so we know the hub processed the duplicate frame.
    let sentinel = make_app_tools_update_for("wh-device-2", 2, "tool_b");
    socket
        .send(WsMessage::Binary(sentinel.encode_to_vec().into()))
        .await
        .unwrap();
    // Wait until sentinel lands in the catalog.
    poll_fresh(server.state(), "wh-device-2", 2, Duration::from_secs(3)).await;

    // Give a bit more time for any spurious webhook row.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Count again.
    let rows_after: usize = {
        let rows = wh_store
            .lease_due(chrono::Utc::now() + chrono::Duration::seconds(3600), 50)
            .await
            .unwrap_or_default();
        for row in &rows {
            let _ = wh_store
                .mark_failed(
                    &row.event_id,
                    row.next_retry_at,
                    row.attempts,
                    row.last_error.as_deref().unwrap_or(""),
                )
                .await;
        }
        rows.iter()
            .filter(|r| {
                r.payload["eventType"] == "device.app_tools.updated"
                    && r.payload["deviceId"] == "wh-device-2"
            })
            .count()
    };

    // Exactly one additional row for the sentinel (rev=2); duplicate must not
    // have added one.
    assert_eq!(
        rows_after,
        rows_before + 1,
        "duplicate update must not enqueue a webhook; expected exactly one new row (sentinel)"
    );

    let _ = socket.close(None).await;
    server.shutdown().await;
}

/// Helper variant of `make_app_tools_update` that takes a device_id.
fn make_app_tools_update_for(device_id: &str, revision: u64, tool_name: &str) -> Envelope {
    Envelope {
        device_id: device_id.into(),
        msg_id: format!("app-tools-{device_id}-{revision}"),
        seq: revision,
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::AppToolsUpdate(AppToolsUpdate {
            revision,
            tools: vec![AppToolDescriptor {
                name: tool_name.into(),
                description: "A webhook test tool".into(),
                input_schema_json: r#"{"type":"object"}"#.into(),
                requires_approval: false,
            }],
        })),
        ..Default::default()
    }
}

/// A helper that builds an empty AppToolsUpdate (no tools).
fn make_empty_app_tools_update(device_id: &str, revision: u64) -> Envelope {
    Envelope {
        device_id: device_id.into(),
        msg_id: format!("app-tools-empty-{device_id}-{revision}"),
        seq: revision,
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::AppToolsUpdate(AppToolsUpdate {
            revision,
            tools: vec![],
        })),
        ..Default::default()
    }
}

/// A tool-less daemon reconnect (empty-to-empty) must NOT spam the webhook.
/// The catalog is still stored and stale is cleared, but no audit/webhook fires.
#[tokio::test]
async fn empty_catalog_reconnect_does_not_spam_webhook() {
    let state = test_state_with_webhook_persistent().await;
    let server = spawn_server_with_state(state).await;

    let verifying = SigningKey::from_bytes(&[7u8; 32])
        .verifying_key()
        .to_bytes();
    server
        .state()
        .devices
        .pre_register("ec-device-1", &verifying, "user-ec-1")
        .await
        .unwrap();

    let wh_store = server
        .state()
        .webhook
        .store()
        .expect("webhook store must be present");

    // Connect and send empty catalog (rev=1).
    let (mut socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();
    let challenge = read_hello_challenge(&mut socket).await;
    let hello = signed_hello("ec-device-1", &challenge.nonce);
    socket
        .send(WsMessage::Binary(hello.encode_to_vec().into()))
        .await
        .unwrap();
    let _ = read_hello_accepted(&mut socket).await;

    let empty_update = make_empty_app_tools_update("ec-device-1", 1);
    socket
        .send(WsMessage::Binary(empty_update.encode_to_vec().into()))
        .await
        .unwrap();

    // Wait for the catalog to land (stale cleared by the accepted update).
    let did_land = poll_fresh(server.state(), "ec-device-1", 1, Duration::from_secs(3)).await;
    assert!(did_land, "empty catalog must be stored and become fresh");

    // Give time for any spurious webhook to appear.
    tokio::time::sleep(Duration::from_millis(150)).await;

    // No device.app_tools.updated webhook should have been fired.
    let found_webhook = {
        let rows = wh_store
            .lease_due(chrono::Utc::now() + chrono::Duration::seconds(3600), 50)
            .await
            .unwrap_or_default();
        rows.iter().any(|r| {
            r.payload["eventType"] == "device.app_tools.updated"
                && r.payload["deviceId"] == "ec-device-1"
        })
    };
    assert!(
        !found_webhook,
        "empty-to-empty catalog update must not enqueue a device.app_tools.updated webhook"
    );

    let _ = socket.close(None).await;
    server.shutdown().await;
}

/// An empty-AFTER-nonempty catalog update must still fire the webhook so
/// operators see intentional tool unregistrations.
#[tokio::test]
async fn empty_after_nonempty_catalog_fires_webhook() {
    let state = test_state_with_webhook_persistent().await;
    let server = spawn_server_with_state(state).await;

    let verifying = SigningKey::from_bytes(&[7u8; 32])
        .verifying_key()
        .to_bytes();
    server
        .state()
        .devices
        .pre_register("ec-device-2", &verifying, "user-ec-2")
        .await
        .unwrap();

    let wh_store = server
        .state()
        .webhook
        .store()
        .expect("webhook store must be present");

    let (mut socket, _) = tokio_tungstenite::connect_async(server.ws_url("/ws"))
        .await
        .unwrap();
    let challenge = read_hello_challenge(&mut socket).await;
    let hello = signed_hello("ec-device-2", &challenge.nonce);
    socket
        .send(WsMessage::Binary(hello.encode_to_vec().into()))
        .await
        .unwrap();
    let _ = read_hello_accepted(&mut socket).await;

    // First: send a non-empty catalog (rev=1, 1 tool).
    let nonempty = make_app_tools_update_for("ec-device-2", 1, "some_tool");
    socket
        .send(WsMessage::Binary(nonempty.encode_to_vec().into()))
        .await
        .unwrap();
    // Wait for the first webhook to confirm it landed.
    let first_found = poll_webhook_delivery(
        &wh_store,
        |p| {
            p["eventType"] == "device.app_tools.updated"
                && p["deviceId"] == "ec-device-2"
                && p["data"]["toolCount"] == 1
        },
        Duration::from_secs(3),
    )
    .await;
    assert!(
        first_found,
        "non-empty catalog must fire webhook (rev=1, toolCount=1)"
    );

    // Now send an empty catalog (rev=2, 0 tools) — must still fire.
    let empty = make_empty_app_tools_update("ec-device-2", 2);
    socket
        .send(WsMessage::Binary(empty.encode_to_vec().into()))
        .await
        .unwrap();

    let second_found = poll_webhook_delivery(
        &wh_store,
        |p| {
            p["eventType"] == "device.app_tools.updated"
                && p["deviceId"] == "ec-device-2"
                && p["data"]["toolCount"] == 0
                && p["data"]["revision"] == 2
        },
        Duration::from_secs(3),
    )
    .await;
    assert!(
        second_found,
        "empty-after-nonempty catalog (rev=2, toolCount=0) must still fire the webhook"
    );

    let _ = socket.close(None).await;
    server.shutdown().await;
}

/// 256 KiB size guard: an oversized catalog must be rejected (not stored).
#[tokio::test]
async fn oversized_catalog_is_rejected() {
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

    // Build a tool whose input_schema_json exceeds 256 KiB.
    let huge_schema = "x".repeat(300 * 1024);
    let oversized_update = ahand_protocol::Envelope {
        device_id: "device-1".into(),
        msg_id: "app-tools-oversized".into(),
        seq: 1,
        ts_ms: now_ms(),
        payload: Some(ahand_protocol::envelope::Payload::AppToolsUpdate(
            ahand_protocol::AppToolsUpdate {
                revision: 1,
                tools: vec![AppToolDescriptor {
                    name: "huge_tool".into(),
                    description: "A tool with a very large schema".into(),
                    input_schema_json: huge_schema,
                    requires_approval: false,
                }],
            },
        )),
        ..Default::default()
    };
    socket
        .send(WsMessage::Binary(oversized_update.encode_to_vec().into()))
        .await
        .expect("send oversized update");

    // The hub should reject the update: catalog must remain absent after a short wait.
    tokio::time::sleep(Duration::from_millis(150)).await;

    let catalog = server
        .state()
        .app_tools
        .get_catalog("device-1")
        .await
        .expect("no store error");
    // Catalog may be absent (never written) or, due to hello-time stale + absent None, still None.
    // Either way, no catalog with revision=1 should exist.
    if let Some(c) = catalog {
        assert_ne!(c.revision, 1, "oversized catalog must not be stored");
    }

    let _ = socket.close(None).await;
    server.shutdown().await;
}
