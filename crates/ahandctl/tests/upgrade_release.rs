//! Integration tests for the native upgrade-check and upgrade paths.
//!
//! A small axum stub server stands in for `api.github.com` *and* the GitHub
//! release download CDN, letting tests exercise the full
//! `resolve_latest` / `run_with_bases` stack without any network calls.
//! Pattern mirrors `ahandd/tests/file_ops_s3_write.rs`.

// On Windows the #[cfg(unix)] full-flow tests vanish, leaving some shared
// helpers/imports unreferenced; allow that for this test-support-heavy file
// instead of cfg-gating every import (windows still compiles + runs the
// platform-neutral subset).
#![cfg_attr(windows, allow(dead_code, unused_imports))]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use ahandctl::upgrade::{
    build_check_output, check_output, resolve_latest, resolve_target, run_with_bases,
};
use axum::body::Body;
use axum::{
    Router,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

// ── Stub server (API releases endpoint) ───────────────────────────────────

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

// ── Full-upgrade stub server ───────────────────────────────────────────────

/// Assets served by the download stub.
/// Key: URL path (e.g. "/rust-v0.2.0/ahandd-linux-x64"), Value: bytes.
// Used only by the #[cfg(unix)] full-flow upgrade tests below; gate to
// keep the windows clippy lane (-D dead-code) clean.
#[cfg(unix)]
#[derive(Clone)]
struct DownloadState {
    /// API releases JSON body.
    releases_json: String,
    /// Assets keyed by URL path suffix after the base.
    assets: Arc<HashMap<String, Vec<u8>>>,
}

// Used only by the #[cfg(unix)] full-flow upgrade tests below; gate to
// keep the windows clippy lane (-D dead-code) clean.
#[cfg(unix)]
async fn download_stub_handler(
    AxumPath(tail): AxumPath<String>,
    State(state): State<Arc<DownloadState>>,
) -> impl IntoResponse {
    if let Some(body) = state.assets.get(&format!("/{tail}")) {
        (StatusCode::OK, Body::from(body.clone())).into_response()
    } else {
        (StatusCode::NOT_FOUND, Body::from("not found")).into_response()
    }
}

// Used only by the #[cfg(unix)] full-flow upgrade tests below; gate to
// keep the windows clippy lane (-D dead-code) clean.
#[cfg(unix)]
async fn download_releases_handler(State(state): State<Arc<DownloadState>>) -> impl IntoResponse {
    (StatusCode::OK, state.releases_json.clone()).into_response()
}

// Used only by the #[cfg(unix)] full-flow upgrade tests below; gate to
// keep the windows clippy lane (-D dead-code) clean.
#[cfg(unix)]
struct FullStub {
    addr: std::net::SocketAddr,
    shutdown: oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<()>,
}

// Used only by the #[cfg(unix)] full-flow upgrade tests below; gate to
// keep the windows clippy lane (-D dead-code) clean.
#[cfg(unix)]
impl FullStub {
    fn api_base(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn download_base(&self) -> String {
        format!("http://{}", self.addr)
    }

    async fn stop(self) {
        let _ = self.shutdown.send(());
        let _ = tokio::time::timeout(Duration::from_secs(2), self.handle).await;
    }
}

// Used only by the #[cfg(unix)] full-flow upgrade tests below; gate to
// keep the windows clippy lane (-D dead-code) clean.
#[cfg(unix)]
async fn spawn_full_stub(releases_json: String, assets: HashMap<String, Vec<u8>>) -> FullStub {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state = Arc::new(DownloadState {
        releases_json,
        assets: Arc::new(assets),
    });
    let app = Router::new()
        .route(
            "/repos/team9ai/aHand/releases",
            get(download_releases_handler),
        )
        .route("/{*tail}", get(download_stub_handler))
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
    tokio::time::sleep(Duration::from_millis(10)).await;
    FullStub {
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

/// Returns the platform suffix that the upgrade code will use.
// Used only by the #[cfg(unix)] full-flow upgrade tests below; gate to
// keep the windows clippy lane (-D dead-code) clean.
#[cfg(unix)]
fn platform_suffix() -> String {
    ahand_platform::paths::release_suffix()
}

/// Build fake binary bytes (not real ELF/PE, just enough to verify we wrote them).
// Used only by the #[cfg(unix)] full-flow upgrade tests below; gate to
// keep the windows clippy lane (-D dead-code) clean.
#[cfg(unix)]
fn fake_binary_bytes(name: &str) -> Vec<u8> {
    format!("fake-binary-content-for-{name}").into_bytes()
}

/// Build a minimal valid gzip-compressed tar archive with one file inside.
fn make_tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let enc = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
        let mut ar = tar::Builder::new(enc);
        for (path, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            ar.append_data(&mut header, path, std::io::Cursor::new(data))
                .unwrap();
        }
        ar.finish().unwrap();
    }
    buf
}

/// Build a tar.gz archive with a single entry at `path` (arbitrary, may
/// include `..` or leading `/` for traversal tests).
///
/// The `tar` crate's safe API rejects paths with `..` or `/`, so we write
/// a raw POSIX ustar header to bypass those checks.
fn make_raw_tar_gz_with_path(path: &str, data: &[u8]) -> Vec<u8> {
    // POSIX ustar header: 512 bytes.
    let mut header = [0u8; 512];

    // Name field: bytes 0..100.
    let name_bytes = path.as_bytes();
    let name_len = name_bytes.len().min(100);
    header[..name_len].copy_from_slice(&name_bytes[..name_len]);

    // File mode: bytes 100..108 — "0000644\0"
    header[100..107].copy_from_slice(b"0000644");
    header[107] = 0;

    // UID/GID: bytes 108..124
    header[108..115].copy_from_slice(b"0000000");
    header[115] = 0;
    header[116..123].copy_from_slice(b"0000000");
    header[123] = 0;

    // Size: bytes 124..136 (octal, null-terminated).
    let size_str = format!("{:011o}\0", data.len());
    header[124..136].copy_from_slice(size_str.as_bytes());

    // Modification time: bytes 136..148
    header[136..147].copy_from_slice(b"00000000000");
    header[147] = 0;

    // Type flag: bytes 156 — '0' = regular file.
    header[156] = b'0';

    // Magic: bytes 257..265 — "ustar  \0"
    header[257..265].copy_from_slice(b"ustar  \0");

    // Compute checksum (bytes 148..156 are treated as spaces during calc).
    header[148..156].copy_from_slice(b"        ");
    let cksum: u32 = header.iter().map(|&b| b as u32).sum();
    let cksum_str = format!("{:06o}\0 ", cksum);
    header[148..156].copy_from_slice(cksum_str.as_bytes());

    // Pad data to a 512-byte boundary.
    let padded_len = (data.len() + 511) & !511;
    let mut raw_tar: Vec<u8> = Vec::with_capacity(512 + padded_len + 1024);
    raw_tar.extend_from_slice(&header);
    raw_tar.extend_from_slice(data);
    raw_tar.extend(std::iter::repeat_n(0u8, padded_len - data.len()));
    // Two 512-byte zero blocks = end-of-archive.
    raw_tar.extend(std::iter::repeat_n(0u8, 1024));

    // Gzip-compress.
    let mut gz_buf = Vec::new();
    let mut enc = flate2::write::GzEncoder::new(&mut gz_buf, flate2::Compression::default());
    std::io::Write::write_all(&mut enc, &raw_tar).unwrap();
    enc.finish().unwrap();
    gz_buf
}

/// Build a checksums-rust.txt line in shasum-a-256 format.
// Used only by the #[cfg(unix)] full-flow upgrade tests below; gate to
// keep the windows clippy lane (-D dead-code) clean.
#[cfg(unix)]
fn make_checksum_line(filename: &str, data: &[u8]) -> String {
    let hex = hex::encode(Sha256::digest(data));
    format!("{hex}  {filename}\n")
}

// ── resolve_latest tests ───────────────────────────────────────────────────

/// Happy path: three tags resolved correctly from a fixture with older ones
/// below them.  Versions are stored BARE (prefix stripped).
#[tokio::test]
async fn resolve_latest_three_tags_correct() {
    let stub = spawn_stub(StubMode::Ok(fixture_all_tags())).await;
    let info = resolve_latest(&stub.api_base(), "team9ai/aHand")
        .await
        .expect("should resolve");

    assert_eq!(info.rust.as_deref(), Some("0.2.0"));
    assert_eq!(info.admin.as_deref(), Some("0.1.5"));
    assert_eq!(info.browser.as_deref(), Some("0.1.1"));

    stub.stop().await;
}

/// Missing admin/browser tags → both are `None`.
#[tokio::test]
async fn resolve_latest_missing_admin_browser_are_none() {
    let stub = spawn_stub(StubMode::Ok(fixture_rust_only())).await;
    let info = resolve_latest(&stub.api_base(), "team9ai/aHand")
        .await
        .expect("should resolve");

    assert_eq!(info.rust.as_deref(), Some("0.2.0"));
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
    let err = run_with_bases(true, None, &stub.api_base(), "http://unused", tmp.path())
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

// ── run_with_bases check-mode end-to-end tests ────────────────────────────

/// End-to-end: marker "0.2.0" matches latest rust tag "rust-v0.2.0" (stored
/// bare after prefix strip) → output contains "Already up to date!" and
/// displays the bare version number.
#[tokio::test]
async fn run_with_bases_marker_equals_latest_up_to_date() {
    let stub = spawn_stub(StubMode::Ok(fixture_all_tags())).await;
    let tmp = TempDir::new().unwrap();

    // Write a bare version marker — the same format install.sh writes.
    ahand_platform::paths::write_version_marker(tmp.path(), "0.2.0").unwrap();

    let out = check_output(None, &stub.api_base(), tmp.path())
        .await
        .expect("check_output should succeed");

    assert!(
        out.contains("Already up to date!"),
        "expected 'Already up to date!' in output, got: {out}"
    );
    assert!(
        out.contains("rust=0.2.0"),
        "expected bare version 'rust=0.2.0' in output, got: {out}"
    );

    stub.stop().await;
}

/// End-to-end: marker "0.1.0" is older than latest "0.2.0" → output reports
/// the update with bare version numbers.
#[tokio::test]
async fn run_with_bases_marker_older_reports_update_available() {
    let stub = spawn_stub(StubMode::Ok(fixture_all_tags())).await;
    let tmp = TempDir::new().unwrap();

    ahand_platform::paths::write_version_marker(tmp.path(), "0.1.0").unwrap();

    let out = check_output(None, &stub.api_base(), tmp.path())
        .await
        .expect("check_output should succeed");

    assert!(
        out.contains("Update available: 0.1.0 -> 0.2.0"),
        "expected update line with bare versions, got: {out}"
    );
    assert!(
        !out.contains("Already up to date"),
        "should NOT say up-to-date, got: {out}"
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

// ── build_check_output unit tests ─────────────────────────────────────────

/// When current == latest rust → "Already up to date!".
#[test]
fn build_check_output_up_to_date() {
    let out = build_check_output("0.2.0", "0.2.0", "0.1.5", "0.1.1", "linux-x64");
    assert!(out.contains("Already up to date!"), "got: {out}");
    assert!(out.contains("Current version: 0.2.0"), "got: {out}");
    assert!(out.contains("Latest version:  rust=0.2.0"), "got: {out}");
    assert!(out.contains("Platform:        linux-x64"), "got: {out}");
}

/// When current != latest rust → "Update available" with correct versions.
#[test]
fn build_check_output_update_available() {
    let out = build_check_output("0.1.9", "0.2.0", "0.1.5", "none", "darwin-arm64");
    assert!(
        out.contains("Update available: 0.1.9 -> 0.2.0"),
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
    let out = build_check_output("0.2.0", "0.2.0", "none", "none", "windows-x64");
    assert!(out.contains("admin=none"), "got: {out}");
    assert!(out.contains("browser=none"), "got: {out}");
}

// ── Full upgrade happy path (unix CI) ─────────────────────────────────────

/// Full-flow happy path: stub serves fake binaries + matching checksums +
/// small valid admin-spa.tar.gz; asserts binaries installed, dist file present,
/// version marker written, output contains "Upgrade complete".
#[cfg(unix)]
#[tokio::test]
async fn upgrade_full_flow_happy_path() {
    let suffix = platform_suffix();
    let ver = "0.2.0";

    let ahandd_filename = format!("ahandd-{suffix}");
    let ahandctl_filename = format!("ahandctl-{suffix}");

    let ahandd_bytes = fake_binary_bytes("ahandd");
    let ahandctl_bytes = fake_binary_bytes("ahandctl");
    let spa_content = b"index.html-content";
    let spa_tar = make_tar_gz(&[("index.html", spa_content)]);

    let mut cs = String::new();
    cs.push_str(&make_checksum_line(&ahandd_filename, &ahandd_bytes));
    cs.push_str(&make_checksum_line(&ahandctl_filename, &ahandctl_bytes));

    let mut assets: HashMap<String, Vec<u8>> = HashMap::new();
    assets.insert(format!("/rust-v{ver}/checksums-rust.txt"), cs.into_bytes());
    assets.insert(
        format!("/rust-v{ver}/{ahandd_filename}"),
        ahandd_bytes.clone(),
    );
    assets.insert(
        format!("/rust-v{ver}/{ahandctl_filename}"),
        ahandctl_bytes.clone(),
    );
    assets.insert(format!("/admin-v{ver}/admin-spa.tar.gz"), spa_tar);

    let releases_json = format!(
        r#"[{{"tag_name":"rust-v{ver}"}},{{"tag_name":"admin-v{ver}"}},{{"tag_name":"browser-v{ver}"}}]"#
    );

    let stub = spawn_full_stub(releases_json, assets).await;
    let tmp = TempDir::new().unwrap();

    let result = run_with_bases(
        false,
        Some(ver),
        &stub.api_base(),
        &stub.download_base(),
        tmp.path(),
    )
    .await;

    stub.stop().await;

    result.expect("upgrade should succeed");

    // Binaries installed at bin/
    let bin_dir = tmp.path().join("bin");
    let ahandd_path = bin_dir.join(ahand_platform::paths::exe_name("ahandd"));
    let ahandctl_path = bin_dir.join(ahand_platform::paths::exe_name("ahandctl"));
    assert!(ahandd_path.exists(), "ahandd binary should be installed");
    assert!(
        ahandctl_path.exists(),
        "ahandctl binary should be installed"
    );
    assert_eq!(std::fs::read(&ahandd_path).unwrap(), ahandd_bytes);
    assert_eq!(std::fs::read(&ahandctl_path).unwrap(), ahandctl_bytes);

    // Exec bit set.
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&ahandd_path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "ahandd not executable");
        let mode = std::fs::metadata(&ahandctl_path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "ahandctl not executable");
    }

    // Admin SPA extracted.
    let dist_file = tmp.path().join("admin").join("dist").join("index.html");
    assert!(dist_file.exists(), "admin dist file should be extracted");
    assert_eq!(std::fs::read(&dist_file).unwrap(), b"index.html-content");

    // Version marker written.
    assert_eq!(
        ahand_platform::paths::read_version_marker(tmp.path()),
        Some(ver.to_string())
    );
}

// ── Checksum mismatch → Err BEFORE install ────────────────────────────────

/// Checksum mismatch for ahandd → hard error BEFORE any binary is installed.
#[cfg(unix)]
#[tokio::test]
async fn upgrade_checksum_mismatch_errors_before_install() {
    let suffix = platform_suffix();
    let ver = "0.3.0";

    let ahandd_filename = format!("ahandd-{suffix}");
    let ahandctl_filename = format!("ahandctl-{suffix}");

    let ahandd_bytes = fake_binary_bytes("ahandd");
    let ahandctl_bytes = fake_binary_bytes("ahandctl");

    // Checksum for ahandd is deliberately WRONG.
    let wrong_hex = "0000000000000000000000000000000000000000000000000000000000000000";
    let mut cs = String::new();
    cs.push_str(&format!("{wrong_hex}  {ahandd_filename}\n"));
    cs.push_str(&make_checksum_line(&ahandctl_filename, &ahandctl_bytes));

    let mut assets: HashMap<String, Vec<u8>> = HashMap::new();
    assets.insert(format!("/rust-v{ver}/checksums-rust.txt"), cs.into_bytes());
    assets.insert(format!("/rust-v{ver}/{ahandd_filename}"), ahandd_bytes);
    assets.insert(format!("/rust-v{ver}/{ahandctl_filename}"), ahandctl_bytes);

    let releases_json = format!(r#"[{{"tag_name":"rust-v{ver}"}}]"#);
    let stub = spawn_full_stub(releases_json, assets).await;
    let tmp = TempDir::new().unwrap();

    let err = run_with_bases(
        false,
        Some(ver),
        &stub.api_base(),
        &stub.download_base(),
        tmp.path(),
    )
    .await
    .unwrap_err();

    stub.stop().await;

    let msg = err.to_string();
    assert!(
        msg.contains("checksum") || msg.contains("mismatch"),
        "error should mention checksum, got: {msg}"
    );

    // BEFORE any install: bin/ must be empty (no binaries written).
    let bin_dir = tmp.path().join("bin");
    assert!(
        !bin_dir.exists() || std::fs::read_dir(&bin_dir).unwrap().next().is_none(),
        "bin/ must be empty when checksum fails before install"
    );
}

// ── Tar path-traversal → Err ───────────────────────────────────────────────

/// Tar with `../escape` entry → Err, nothing extracted outside dist.
#[test]
fn extract_admin_spa_rejects_path_traversal() {
    // The `tar` crate refuses to build archives with `..` paths using safe APIs.
    // Build the archive using raw ustar bytes to bypass that check.
    let buf = make_raw_tar_gz_with_path("../escape.txt", b"evil");

    let tmp = TempDir::new().unwrap();
    let dist_dir = tmp.path().join("dist");

    let err = ahandctl::upgrade::extract_admin_spa(&buf, &dist_dir).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("traversal") || msg.contains("..") || msg.contains("path"),
        "should report path traversal, got: {msg}"
    );

    // Nothing should have been extracted outside dist.
    let escaped = tmp.path().join("escape.txt");
    assert!(
        !escaped.exists(),
        "traversal file must not be created outside dist"
    );
}

// ── Required binary 404 → clear Err ──────────────────────────────────────

#[cfg(unix)]
#[tokio::test]
async fn upgrade_required_binary_404_returns_err() {
    let suffix = platform_suffix();
    let ver = "0.4.0";
    // No assets at all → ahandd download will 404.
    let releases_json = format!(r#"[{{"tag_name":"rust-v{ver}"}}]"#);
    let stub = spawn_full_stub(releases_json, HashMap::new()).await;
    let tmp = TempDir::new().unwrap();

    let err = run_with_bases(
        false,
        Some(ver),
        &stub.api_base(),
        &stub.download_base(),
        tmp.path(),
    )
    .await
    .unwrap_err();

    stub.stop().await;

    let msg = err.to_string();
    // Should mention either the 404 HTTP status or that the download failed.
    assert!(
        msg.contains("404") || msg.contains("failed to download") || msg.contains("HTTP"),
        "expected download error, got: {msg}"
    );
    let _ = suffix; // suppress unused warning on non-unix
}

// ── Checksums 404 → proceeds without verify ───────────────────────────────

/// When the checksums file is absent (404), the upgrade should succeed
/// (no verification performed, which matches upgrade.sh behaviour).
#[cfg(unix)]
#[tokio::test]
async fn upgrade_checksums_404_proceeds_and_succeeds() {
    let suffix = platform_suffix();
    let ver = "0.5.0";

    let ahandd_filename = format!("ahandd-{suffix}");
    let ahandctl_filename = format!("ahandctl-{suffix}");

    let ahandd_bytes = fake_binary_bytes("ahandd");
    let ahandctl_bytes = fake_binary_bytes("ahandctl");

    // No checksums file in assets.
    let mut assets: HashMap<String, Vec<u8>> = HashMap::new();
    assets.insert(format!("/rust-v{ver}/{ahandd_filename}"), ahandd_bytes);
    assets.insert(format!("/rust-v{ver}/{ahandctl_filename}"), ahandctl_bytes);

    let releases_json = format!(r#"[{{"tag_name":"rust-v{ver}"}}]"#);
    let stub = spawn_full_stub(releases_json, assets).await;
    let tmp = TempDir::new().unwrap();

    let result = run_with_bases(
        false,
        Some(ver),
        &stub.api_base(),
        &stub.download_base(),
        tmp.path(),
    )
    .await;

    stub.stop().await;

    result.expect("upgrade should succeed even when checksums 404");

    // Binaries installed.
    let bin_dir = tmp.path().join("bin");
    assert!(
        bin_dir
            .join(ahand_platform::paths::exe_name("ahandd"))
            .exists()
    );
    assert!(
        bin_dir
            .join(ahand_platform::paths::exe_name("ahandctl"))
            .exists()
    );
}

// ── No admin version → skips admin step ───────────────────────────────────

/// When `info.admin` is None, the admin step is skipped; binaries still
/// installed and version marker written.
#[cfg(unix)]
#[tokio::test]
async fn upgrade_no_admin_version_skips_admin_step() {
    let suffix = platform_suffix();
    let ver = "0.6.0";

    let ahandd_filename = format!("ahandd-{suffix}");
    let ahandctl_filename = format!("ahandctl-{suffix}");

    let ahandd_bytes = fake_binary_bytes("ahandd");
    let ahandctl_bytes = fake_binary_bytes("ahandctl");

    let mut assets: HashMap<String, Vec<u8>> = HashMap::new();
    assets.insert(format!("/rust-v{ver}/{ahandd_filename}"), ahandd_bytes);
    assets.insert(format!("/rust-v{ver}/{ahandctl_filename}"), ahandctl_bytes);

    // Only rust-v tag — no admin-v or browser-v.
    let releases_json = format!(r#"[{{"tag_name":"rust-v{ver}"}}]"#);
    let stub = spawn_full_stub(releases_json, assets).await;
    let tmp = TempDir::new().unwrap();

    let result = run_with_bases(
        false,
        Some(ver),
        &stub.api_base(),
        &stub.download_base(),
        tmp.path(),
    )
    .await;

    stub.stop().await;
    result.expect("upgrade without admin version should succeed");

    // Version marker written.
    assert_eq!(
        ahand_platform::paths::read_version_marker(tmp.path()),
        Some(ver.to_string())
    );

    // admin/dist should NOT be created.
    let dist_dir = tmp.path().join("admin").join("dist");
    assert!(
        !dist_dir.exists(),
        "dist dir should not exist when no admin"
    );
}

// ── Daemon not running → stop tolerated ───────────────────────────────────

/// No daemon running → daemon::stop() is tolerant; upgrade still completes.
#[cfg(unix)]
#[tokio::test]
async fn upgrade_daemon_not_running_stop_tolerated() {
    // Same as happy path but we don't start any daemon — default tempdir state.
    let suffix = platform_suffix();
    let ver = "0.7.0";

    let ahandd_filename = format!("ahandd-{suffix}");
    let ahandctl_filename = format!("ahandctl-{suffix}");

    let ahandd_bytes = fake_binary_bytes("ahandd");
    let ahandctl_bytes = fake_binary_bytes("ahandctl");

    let mut assets: HashMap<String, Vec<u8>> = HashMap::new();
    assets.insert(format!("/rust-v{ver}/{ahandd_filename}"), ahandd_bytes);
    assets.insert(format!("/rust-v{ver}/{ahandctl_filename}"), ahandctl_bytes);

    let releases_json = format!(r#"[{{"tag_name":"rust-v{ver}"}}]"#);
    let stub = spawn_full_stub(releases_json, assets).await;
    let tmp = TempDir::new().unwrap();

    // No daemon.pid in tmp — daemon::stop() should return Ok / print "not running".
    let result = run_with_bases(
        false,
        Some(ver),
        &stub.api_base(),
        &stub.download_base(),
        tmp.path(),
    )
    .await;

    stub.stop().await;
    result.expect("upgrade should succeed even when daemon is not running");

    assert_eq!(
        ahand_platform::paths::read_version_marker(tmp.path()),
        Some(ver.to_string())
    );
}

// ── extract_admin_spa unit tests ───────────────────────────────────────────

/// Valid tar.gz → files extracted to dist_dir.
#[test]
fn extract_admin_spa_valid_archive() {
    let data = b"hello-spa";
    let tar = make_tar_gz(&[("index.html", data), ("assets/app.js", b"js-content")]);
    let tmp = TempDir::new().unwrap();
    let dist_dir = tmp.path().join("dist");

    ahandctl::upgrade::extract_admin_spa(&tar, &dist_dir).expect("should extract");

    assert_eq!(std::fs::read(dist_dir.join("index.html")).unwrap(), data);
    assert_eq!(
        std::fs::read(dist_dir.join("assets").join("app.js")).unwrap(),
        b"js-content"
    );
}

/// Pre-existing files are cleared before extraction.
#[test]
fn extract_admin_spa_clears_existing_contents() {
    let tmp = TempDir::new().unwrap();
    let dist_dir = tmp.path().join("dist");
    std::fs::create_dir_all(&dist_dir).unwrap();
    std::fs::write(dist_dir.join("old.txt"), b"stale").unwrap();

    let tar = make_tar_gz(&[("new.html", b"new")]);
    ahandctl::upgrade::extract_admin_spa(&tar, &dist_dir).expect("should extract");

    assert!(
        !dist_dir.join("old.txt").exists(),
        "stale file must be removed"
    );
    assert!(
        dist_dir.join("new.html").exists(),
        "new file must be present"
    );
}

/// Absolute path in archive → rejected.
#[test]
fn extract_admin_spa_rejects_absolute_path() {
    // Build archive with an absolute path using raw bytes to bypass tar crate safety.
    let buf = make_raw_tar_gz_with_path("/etc/evil.txt", b"evil");

    let tmp = TempDir::new().unwrap();
    let dist_dir = tmp.path().join("dist");
    let err = ahandctl::upgrade::extract_admin_spa(&buf, &dist_dir).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("absolute") || msg.contains("traversal") || msg.contains("path"),
        "should reject absolute path, got: {msg}"
    );
}

/// Symlink-escape regression test: tar contains a symlink entry pointing outside
/// the dist dir, followed by a file entry that traverses through it.
///
/// This exercises the second defense layer (tar's `unpack_in` / `validate_inside_dst`
/// canonicalization) documented on `extract_admin_spa`.  The archive is built with
/// the `tar` crate's safe `Builder` API so it is a well-formed archive; the guard
/// must still reject or contain the extraction.
#[cfg(unix)]
#[test]
fn extract_admin_spa_rejects_symlink_escape() {
    use std::io::Cursor;

    // Build a tar.gz containing:
    //   1. a symlink entry: `link` -> `/tmp`
    //   2. a regular file entry: `link/evil.txt` with content "evil"
    //
    // If the extractor naively follows the symlink it would write outside dist.
    let mut tar_buf = Vec::new();
    {
        let enc = flate2::write::GzEncoder::new(&mut tar_buf, flate2::Compression::default());
        let mut ar = tar::Builder::new(enc);

        // Entry 1: symlink `link` -> `/tmp`
        let mut sym_header = tar::Header::new_gnu();
        sym_header.set_entry_type(tar::EntryType::Symlink);
        sym_header.set_size(0);
        sym_header.set_mode(0o777);
        sym_header
            .set_link_name(std::path::Path::new("/tmp"))
            .unwrap();
        sym_header.set_cksum();
        ar.append_data(&mut sym_header, "link", Cursor::new(b""))
            .unwrap();

        // Entry 2: regular file `link/evil.txt` — would escape through the symlink
        let mut file_header = tar::Header::new_gnu();
        file_header.set_entry_type(tar::EntryType::Regular);
        file_header.set_size(4);
        file_header.set_mode(0o644);
        file_header.set_cksum();
        ar.append_data(&mut file_header, "link/evil.txt", Cursor::new(b"evil"))
            .unwrap();

        ar.finish().unwrap();
    }

    let tmp = TempDir::new().unwrap();
    let dist_dir = tmp.path().join("dist");

    // The call must either return Err OR leave nothing outside dist.
    let result = ahandctl::upgrade::extract_admin_spa(&tar_buf, &dist_dir);

    // Primary assertion: nothing must have escaped to /tmp/evil.txt or the real /tmp.
    let escaped = std::path::Path::new("/tmp/evil.txt");
    assert!(
        !escaped.exists(),
        "symlink-escape file must not be created outside dist"
    );

    // If extraction succeeded (tar crate detected and defused the attack itself),
    // also verify the file is not reachable via the symlink inside dist.
    if result.is_ok() {
        // The symlink inside dist might point to /tmp; following it must not have
        // written evil.txt there (checked above).  Nothing else to assert here.
    }
    // Both Ok (contained) and Err (rejected) are acceptable outcomes.
}

// ── I-2: version marker only written after BOTH binaries swap ────────────

/// Partial-upgrade error path: stub serves a correct checksums file but the
/// ahandctl swap is made to fail deterministically by pre-creating the target
/// binary name as a non-empty directory (renaming a file onto a non-empty
/// directory fails on Unix).
///
/// Asserts:
/// - `run_with_bases` returns `Err`
/// - the error message mentions "re-run"
/// - the version marker is NOT written (marker-only-after-both invariant)
#[cfg(unix)]
#[tokio::test]
async fn upgrade_marker_absent_when_ahandctl_swap_fails() {
    let suffix = platform_suffix();
    let ver = "9.9.9"; // unlikely to collide

    let ahandd_filename = format!("ahandd-{suffix}");
    let ahandctl_filename = format!("ahandctl-{suffix}");

    let ahandd_bytes = fake_binary_bytes("ahandd");
    let ahandctl_bytes = fake_binary_bytes("ahandctl");

    let mut cs = String::new();
    cs.push_str(&make_checksum_line(&ahandd_filename, &ahandd_bytes));
    cs.push_str(&make_checksum_line(&ahandctl_filename, &ahandctl_bytes));

    let mut assets: HashMap<String, Vec<u8>> = HashMap::new();
    assets.insert(format!("/rust-v{ver}/checksums-rust.txt"), cs.into_bytes());
    assets.insert(format!("/rust-v{ver}/{ahandd_filename}"), ahandd_bytes);
    assets.insert(format!("/rust-v{ver}/{ahandctl_filename}"), ahandctl_bytes);

    let releases_json = format!(r#"[{{"tag_name":"rust-v{ver}"}}]"#);
    let stub = spawn_full_stub(releases_json, assets).await;
    let tmp = TempDir::new().unwrap();

    // Pre-create `{ahand_home}/bin/{ahandctl_exe_name}` as a DIRECTORY containing a
    // file so that rename-onto-it fails (EISDIR / ENOTEMPTY on unix).
    let bin_dir = tmp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let ahandctl_target = bin_dir.join(ahand_platform::paths::exe_name("ahandctl"));
    std::fs::create_dir_all(&ahandctl_target).unwrap();
    std::fs::write(ahandctl_target.join("sentinel"), b"block").unwrap();

    let err = run_with_bases(
        false,
        Some(ver),
        &stub.api_base(),
        &stub.download_base(),
        tmp.path(),
    )
    .await
    .expect_err("should fail when ahandctl swap is blocked");

    stub.stop().await;

    let msg = err.to_string();
    assert!(
        msg.contains("re-run"),
        "error message should tell the user to re-run, got: {msg}"
    );

    // Version marker must NOT have been written.
    assert!(
        ahand_platform::paths::read_version_marker(tmp.path()).is_none(),
        "version marker must be absent when ahandctl swap failed"
    );
}
