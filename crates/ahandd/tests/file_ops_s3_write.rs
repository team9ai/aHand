//! Daemon-side coverage for the S3 large-file write path.
//!
//! The flow exercised here: hub-injected presigned GET URL on
//! `FullWrite.s3_download_url` → `handle_full_write` does a plain HTTP
//! GET against that URL → bytes get written to disk. The test stands a
//! `Content-Length` ceiling and an HTTP-status sanity check ahead of
//! the body read.
//!
//! Stand-in HTTP server: a tiny `axum` router on a random port. We
//! don't need a real S3 — `handle_full_write` only treats the URL as
//! "an HTTP resource it must GET", which is exactly what the presigner
//! produces.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ahand_protocol::{
    file_request, file_response, file_write, full_write, FileErrorCode, FileRequest, FileWrite,
    FullWrite, WriteAction,
};
use ahandd::config::FilePolicyConfig;
use ahandd::file_manager::FileManager;
use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::stream;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// A handle on a single-route axum server returning canned bytes for
/// `/object`. `addr` is the bound socket; `shutdown` lets the test
/// politely tear it down.
struct StubObject {
    addr: std::net::SocketAddr,
    shutdown: oneshot::Sender<()>,
    server_handle: tokio::task::JoinHandle<()>,
}

impl StubObject {
    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    async fn stop(self) {
        let _ = self.shutdown.send(());
        let _ = tokio::time::timeout(Duration::from_secs(2), self.server_handle).await;
    }
}

/// Behavior modes for the stub object server.
#[derive(Clone)]
enum StubMode {
    /// 200 with the given bytes; sets a real Content-Length.
    Ok(Vec<u8>),
    /// 200 with the given bytes; no Content-Length advertised
    /// (chunked). Used to verify the daemon still works when S3
    /// returns chunked transfers without a length header.
    OkNoContentLength(Vec<u8>),
    /// 404.
    NotFound,
}

#[derive(Clone)]
struct StubState {
    mode: StubMode,
}

async fn stub_get(State(state): State<Arc<StubState>>) -> Response {
    match &state.mode {
        StubMode::Ok(bytes) => {
            // axum derives Content-Length from the Vec body
            // automatically — no need to set it explicitly.
            (StatusCode::OK, bytes.clone()).into_response()
        }
        StubMode::OkNoContentLength(bytes) => {
            // Stream the body via `Body::from_stream` so axum/hyper
            // doesn't compute a Content-Length up front. This mirrors
            // the chunked-transfer case some S3-compatible gateways
            // emit when range-streaming.
            use axum::body::Body;
            let body = bytes.clone();
            let s = stream::once(async move { Ok::<_, std::io::Error>(body) });
            (StatusCode::OK, Body::from_stream(s)).into_response()
        }
        StubMode::NotFound => (StatusCode::NOT_FOUND, "missing").into_response(),
    }
}

async fn spawn_stub(mode: StubMode) -> StubObject {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state = Arc::new(StubState { mode });
    let app = Router::new()
        .route("/object", get(stub_get))
        .with_state(state);
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let server_handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
    });
    // Tiny settle so the listener is accept()-ing.
    tokio::time::sleep(Duration::from_millis(10)).await;
    StubObject {
        addr,
        shutdown: shutdown_tx,
        server_handle,
    }
}

fn test_manager(tmp: &TempDir) -> (FileManager, PathBuf) {
    let root = tmp.path().canonicalize().unwrap();
    let pattern = format!("{}/**", root.to_string_lossy());
    let self_pattern = root.to_string_lossy().into_owned();
    let mgr = FileManager::new(&FilePolicyConfig {
        enabled: true,
        path_allowlist: vec![pattern, self_pattern],
        path_denylist: vec![],
        max_read_bytes: 100_000_000,
        // Set a bounded max so the size-cap tests trip predictably.
        max_write_bytes: 8 * 1024,
        dangerous_paths: vec![],
    });
    (mgr, root)
}

fn full_write_request(target: &Path, object_key: &str, download_url: Option<String>) -> FileRequest {
    FileRequest {
        request_id: "test-s3-write".into(),
        operation: Some(file_request::Operation::Write(FileWrite {
            path: target.to_string_lossy().into_owned(),
            create_parents: false,
            method: Some(file_write::Method::FullWrite(FullWrite {
                source: Some(full_write::Source::S3ObjectKey(object_key.into())),
                s3_download_url: download_url,
                s3_download_url_expires_ms: None,
            })),
            encoding: None,
            no_follow_symlink: false,
        })),
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

/// Happy path: hub injected a valid URL → daemon downloads → file
/// written with exactly the bytes from the stub server.
#[tokio::test]
async fn full_write_with_s3_url_writes_bytes_to_disk() {
    let body = b"hello from S3".to_vec();
    let stub = spawn_stub(StubMode::Ok(body.clone())).await;
    let tmp = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&tmp);
    let target = root.join("from-s3.txt");

    let req = full_write_request(&target, "obj-1", Some(stub.url("/object")));
    let resp = mgr.handle(&req).await;

    let result = match resp.result {
        Some(file_response::Result::Write(r)) => r,
        other => panic!("expected Write result, got {other:?}"),
    };
    assert_eq!(result.action, WriteAction::Created as i32);
    assert_eq!(result.bytes_written, body.len() as u64);
    let on_disk = std::fs::read(&target).unwrap();
    assert_eq!(on_disk, body);

    stub.stop().await;
}

/// 404 from the stub maps to `FILE_ERROR_CODE_IO` and includes the
/// status in the message so operators can diagnose.
#[tokio::test]
async fn full_write_with_s3_url_404_returns_io_error() {
    let stub = spawn_stub(StubMode::NotFound).await;
    let tmp = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&tmp);
    let target = root.join("nope.txt");

    let req = full_write_request(&target, "missing", Some(stub.url("/object")));
    let resp = mgr.handle(&req).await;

    let err = match resp.result {
        Some(file_response::Result::Error(e)) => e,
        other => panic!("expected Error, got {other:?}"),
    };
    assert_eq!(err.code, FileErrorCode::Io as i32);
    assert!(
        err.message.contains("404"),
        "error should mention status code, got: {}",
        err.message
    );
    assert!(
        !target.exists(),
        "file must not be created when the download fails"
    );

    stub.stop().await;
}

/// Content-Length ceiling: when the response advertises (and contains)
/// a body bigger than `max_write_bytes`, the daemon refuses with
/// `TOO_LARGE` based on the header — before slurping the body into
/// memory. The body here is just over the 8 KiB cap so axum sets a
/// real Content-Length and the daemon rejects on the header check.
#[tokio::test]
async fn full_write_with_s3_url_rejects_oversized_content_length() {
    let body = vec![b'x'; 9_000]; // 8 KiB cap + slack
    let stub = spawn_stub(StubMode::Ok(body)).await;
    let tmp = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&tmp);
    let target = root.join("toobig.bin");

    let req = full_write_request(&target, "big", Some(stub.url("/object")));
    let resp = mgr.handle(&req).await;

    let err = match resp.result {
        Some(file_response::Result::Error(e)) => e,
        other => panic!("expected Error, got {other:?}"),
    };
    assert_eq!(err.code, FileErrorCode::TooLarge as i32);
    assert!(!target.exists());

    stub.stop().await;
}

/// Missing Content-Length: the daemon still permits the request (some
/// gateways stream chunked) but enforces the size cap on the actually
/// received bytes. Body fits → success.
#[tokio::test]
async fn full_write_with_s3_url_no_content_length_succeeds_under_cap() {
    let body = b"fits".to_vec();
    let stub = spawn_stub(StubMode::OkNoContentLength(body.clone())).await;
    let tmp = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&tmp);
    let target = root.join("chunked.bin");

    let req = full_write_request(&target, "chunked", Some(stub.url("/object")));
    let resp = mgr.handle(&req).await;
    let result = match resp.result {
        Some(file_response::Result::Write(r)) => r,
        other => panic!("expected Write result, got {other:?}"),
    };
    assert_eq!(result.bytes_written, body.len() as u64);
    assert_eq!(std::fs::read(&target).unwrap(), body);

    stub.stop().await;
}

/// Hub forgot to inject a download URL → daemon must NOT silently
/// succeed (e.g. write empty bytes). This catches a hub bug or an
/// old-hub/new-daemon mismatch loudly.
#[tokio::test]
async fn full_write_with_s3_object_key_but_no_download_url_returns_error() {
    let tmp = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&tmp);
    let target = root.join("oops.bin");

    let req = full_write_request(&target, "obj", None);
    let resp = mgr.handle(&req).await;
    let err = match resp.result {
        Some(file_response::Result::Error(e)) => e,
        other => panic!("expected Error, got {other:?}"),
    };
    assert_eq!(err.code, FileErrorCode::Unspecified as i32);
    assert!(
        err.message.contains("s3_download_url"),
        "message should explain what is missing, got: {}",
        err.message
    );
    assert!(!target.exists());
}
