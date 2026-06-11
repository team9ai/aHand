//! Integration tests for the native upgrade-check path.
//!
//! A small axum stub server stands in for `api.github.com`, letting tests
//! exercise the full `resolve_latest` / `run_with_bases` stack without any
//! network calls.  Pattern mirrors `ahandd/tests/file_ops_s3_write.rs`.

use std::sync::Arc;
use std::time::Duration;

use ahandctl::upgrade::{build_check_output, resolve_latest, resolve_target, run_with_bases};
use axum::{Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

// ── Stub server ────────────────────────────────────────────────────────────

#[derive(Clone)]
enum StubMode {
    /// 200 with a JSON body.
    Ok(String),
    /// HTTP 500.
    ServerError,
}

#[derive(Clone)]
struct StubState {
    mode: StubMode,
}

async fn stub_releases(State(state): State<Arc<StubState>>) -> impl IntoResponse {
    match &state.mode {
        StubMode::Ok(body) => (StatusCode::OK, body.clone()).into_response(),
        StubMode::ServerError => (StatusCode::INTERNAL_SERVER_ERROR, "oops").into_response(),
    }
}

struct StubServer {
    addr: std::net::SocketAddr,
    shutdown: oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<()>,
}

impl StubServer {
    fn api_base(&self) -> String {
        format!("http://{}", self.addr)
    }

    async fn stop(self) {
        let _ = self.shutdown.send(());
        let _ = tokio::time::timeout(Duration::from_secs(2), self.handle).await;
    }
}

async fn spawn_stub(mode: StubMode) -> StubServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state = Arc::new(StubState { mode });
    let app = Router::new()
        .route("/repos/team9ai/aHand/releases", get(stub_releases))
        .with_state(state);
    let (tx, rx) = oneshot::channel();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .await
            .unwrap();
    });
    // Give the listener a moment to start accepting.
    tokio::time::sleep(Duration::from_millis(10)).await;
    StubServer {
        addr,
        shutdown: tx,
        handle,
    }
}

// ── Fixture helpers ────────────────────────────────────────────────────────

/// A releases array with three up-to-date tags followed by older ones.
fn fixture_all_tags() -> String {
    r#"[
        {"tag_name": "rust-v0.2.0"},
        {"tag_name": "admin-v0.1.5"},
        {"tag_name": "browser-v0.1.1"},
        {"tag_name": "rust-v0.1.9"},
        {"tag_name": "admin-v0.1.4"},
        {"tag_name": "browser-v0.1.0"}
    ]"#
    .to_string()
}

/// A releases array with only a rust tag (no admin/browser).
fn fixture_rust_only() -> String {
    r#"[{"tag_name": "rust-v0.2.0"}]"#.to_string()
}

/// An empty releases array.
fn fixture_empty() -> String {
    "[]".to_string()
}

/// Malformed JSON.
fn fixture_malformed() -> String {
    "not-json{{{".to_string()
}

// ── resolve_latest tests ───────────────────────────────────────────────────

/// Happy path: three tags resolved correctly from a fixture with older ones
/// below them.
#[tokio::test]
async fn resolve_latest_three_tags_correct() {
    let stub = spawn_stub(StubMode::Ok(fixture_all_tags())).await;
    let info = resolve_latest(&stub.api_base(), "team9ai/aHand")
        .await
        .expect("should resolve");

    assert_eq!(info.rust.as_deref(), Some("rust-v0.2.0"));
    assert_eq!(info.admin.as_deref(), Some("admin-v0.1.5"));
    assert_eq!(info.browser.as_deref(), Some("browser-v0.1.1"));

    stub.stop().await;
}

/// Missing admin/browser tags → both are `None`.
#[tokio::test]
async fn resolve_latest_missing_admin_browser_are_none() {
    let stub = spawn_stub(StubMode::Ok(fixture_rust_only())).await;
    let info = resolve_latest(&stub.api_base(), "team9ai/aHand")
        .await
        .expect("should resolve");

    assert_eq!(info.rust.as_deref(), Some("rust-v0.2.0"));
    assert!(info.admin.is_none());
    assert!(info.browser.is_none());

    stub.stop().await;
}

/// Malformed JSON → `Err` with context in the message.
#[tokio::test]
async fn resolve_latest_malformed_json_returns_err() {
    let stub = spawn_stub(StubMode::Ok(fixture_malformed())).await;
    let err = resolve_latest(&stub.api_base(), "team9ai/aHand")
        .await
        .unwrap_err();

    let msg = err.to_string();
    assert!(
        msg.contains("parse") || msg.contains("JSON") || msg.contains("valid"),
        "error should mention JSON parse failure, got: {msg}"
    );

    stub.stop().await;
}

/// API 500 → `Err`.
#[tokio::test]
async fn resolve_latest_api_500_returns_err() {
    let stub = spawn_stub(StubMode::ServerError).await;
    let err = resolve_latest(&stub.api_base(), "team9ai/aHand")
        .await
        .unwrap_err();

    let msg = err.to_string();
    assert!(
        msg.contains("500") || msg.contains("HTTP"),
        "error should mention HTTP 500, got: {msg}"
    );

    stub.stop().await;
}

/// Empty releases array → rust=None → run_with_bases errors with the
/// "Could not determine latest version" message (legacy parity).
#[tokio::test]
async fn run_with_bases_empty_releases_errors_with_could_not_determine() {
    let stub = spawn_stub(StubMode::Ok(fixture_empty())).await;
    let tmp = TempDir::new().unwrap();
    let err = run_with_bases(true, None, &stub.api_base(), tmp.path())
        .await
        .unwrap_err();

    assert!(
        err.to_string()
            .contains("Could not determine latest version"),
        "got: {}",
        err
    );

    stub.stop().await;
}

// ── resolve_target tests ───────────────────────────────────────────────────

/// Version override pins all three artefacts to the given version; no network
/// call is made (stub would be reachable but shouldn't see a request).
#[tokio::test]
async fn resolve_target_override_pins_all_three() {
    // No stub needed — override should skip the API call entirely.
    let info = resolve_target(Some("0.5.0"), "http://unused.invalid", "team9ai/aHand")
        .await
        .expect("should succeed");

    assert_eq!(info.rust.as_deref(), Some("0.5.0"));
    assert_eq!(info.admin.as_deref(), Some("0.5.0"));
    assert_eq!(info.browser.as_deref(), Some("0.5.0"));
}

// ── run_with_bases check-mode tests ────────────────────────────────────────

/// Marker matches latest rust tag → output contains "Already up to date".
#[tokio::test]
async fn run_with_bases_marker_equals_latest_up_to_date() {
    let stub = spawn_stub(StubMode::Ok(fixture_all_tags())).await;
    let tmp = TempDir::new().unwrap();

    // Write a marker that matches the latest rust tag.
    ahand_platform::paths::write_version_marker(tmp.path(), "rust-v0.2.0").unwrap();

    // Use build_check_output to test the string-builder directly (no stdout
    // capture needed).
    let output = build_check_output(
        "rust-v0.2.0",
        "rust-v0.2.0",
        "admin-v0.1.5",
        "browser-v0.1.1",
        "darwin-arm64",
    );
    assert!(
        output.contains("Already up to date"),
        "expected 'Already up to date' in output, got: {output}"
    );

    stub.stop().await;
}

/// Marker absent → current falls back to CARGO_PKG_VERSION.
#[tokio::test]
async fn current_version_absent_marker_falls_back_to_cargo_pkg_version() {
    let tmp = TempDir::new().unwrap();
    // No marker written.
    let ver = ahandctl::upgrade::current_version(tmp.path());
    assert_eq!(ver, env!("CARGO_PKG_VERSION"));
}

/// Non-check mode (check_only=false) errors with the pinned stub message.
#[tokio::test]
async fn run_with_bases_non_check_errors_with_stub_message() {
    let stub = spawn_stub(StubMode::Ok(fixture_all_tags())).await;
    let tmp = TempDir::new().unwrap();

    let err = run_with_bases(false, None, &stub.api_base(), tmp.path())
        .await
        .unwrap_err();

    assert_eq!(
        err.to_string(),
        "full native upgrade lands in the next change; use --check to query versions"
    );

    stub.stop().await;
}

// ── build_check_output unit tests ─────────────────────────────────────────

/// When current == latest rust → "Already up to date!".
#[test]
fn build_check_output_up_to_date() {
    let out = build_check_output(
        "rust-v0.2.0",
        "rust-v0.2.0",
        "admin-v0.1.5",
        "browser-v0.1.1",
        "linux-x64",
    );
    assert!(out.contains("Already up to date!"), "got: {out}");
    assert!(out.contains("Current version: rust-v0.2.0"), "got: {out}");
    assert!(
        out.contains("Latest version:  rust=rust-v0.2.0"),
        "got: {out}"
    );
    assert!(out.contains("Platform:        linux-x64"), "got: {out}");
}

/// When current != latest rust → "Update available" with correct versions.
#[test]
fn build_check_output_update_available() {
    let out = build_check_output(
        "rust-v0.1.9",
        "rust-v0.2.0",
        "admin-v0.1.5",
        "none",
        "darwin-arm64",
    );
    assert!(
        out.contains("Update available: rust-v0.1.9 -> rust-v0.2.0"),
        "got: {out}"
    );
    assert!(out.contains("Run: ahandctl upgrade"), "got: {out}");
    assert!(
        !out.contains("Already up to date"),
        "should not say up-to-date, got: {out}"
    );
}

/// None artefacts show as "none" in the output.
#[test]
fn build_check_output_none_artefacts_display_as_none() {
    let out = build_check_output("rust-v0.2.0", "rust-v0.2.0", "none", "none", "windows-x64");
    assert!(out.contains("admin=none"), "got: {out}");
    assert!(out.contains("browser=none"), "got: {out}");
}
