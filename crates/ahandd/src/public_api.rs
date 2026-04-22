//! Public library API for embedding `ahandd` in-process.
//!
//! This module exposes the minimal surface area that external consumers
//! (e.g. the Tauri desktop shell) need to spawn and supervise the daemon
//! without depending on its CLI binary entry point. The heavy lifting
//! (WebSocket client, job execution, approvals, etc.) still lives in
//! `crate::ahand_client::run`; `spawn()` wires up the shared state, kicks
//! the client off on a background task, and returns a [`DaemonHandle`]
//! that lets the caller observe status and request a graceful shutdown.
//!
//! Only the `ahand-cloud` connection mode is supported here — the
//! `openclaw-gateway` path remains CLI-only for now.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, oneshot, watch};
use tokio::task::JoinHandle;

pub use ahand_protocol::SessionMode;

use crate::ahand_client::{self, ClientReporter, ConnectOutcome};
use crate::approval::ApprovalManager;
use crate::browser::BrowserManager;
use crate::config::{BrowserConfig, Config, HubConfig};
use crate::device_identity::DeviceIdentity;
use crate::registry::JobRegistry;
use crate::session::SessionManager;

use crate::device_identity::IDENTITY_FILE as IDENTITY_FILE_NAME;

/// Configuration for a library-embedded `ahandd` instance.
#[derive(Clone, Debug)]
pub struct DaemonConfig {
    /// Cloud hub WebSocket URL (e.g. `ws://localhost:3000/ws`).
    pub hub_url: String,
    /// Bootstrap bearer token forwarded to the hub in the Hello envelope.
    ///
    /// Named `device_jwt` to match the eventual JWT-based auth that Task 1.3
    /// will introduce; today this maps to `hub.bootstrap_token`.
    pub device_jwt: String,
    /// Directory where the device identity (`hub-device-identity.json`)
    /// and other daemon state live.
    pub identity_dir: PathBuf,
    /// Default session mode applied on startup.
    pub session_mode: SessionMode,
    /// Whether browser capabilities should be advertised to the hub.
    pub browser_enabled: bool,
    /// How often the daemon sends a `Heartbeat` envelope over its hub
    /// WebSocket. The hub forwards each heartbeat as a `device.heartbeat`
    /// event for TTL-based presence tracking. Defaults to 60 s.
    pub heartbeat_interval: Duration,
    /// Override for the device ID. If `None` (the default), the device ID
    /// is derived as `SHA256(pubkey)` hex from the loaded identity, which
    /// is the canonical derivation per the ahand spec.
    ///
    /// **Warning:** If you supply a value here it MUST equal
    /// `DeviceIdentity::device_id()` for the identity loaded from
    /// `identity_dir`. Supplying a mismatched ID causes the hub to reject
    /// the Hello handshake with a 401 at runtime.
    ///
    /// This field exists only for tests that need to fix the device ID;
    /// production callers should always leave it as `None`.
    pub device_id: Option<String>,
    /// Maximum number of concurrent jobs accepted by the executor. Defaults to 8.
    pub max_concurrent_jobs: usize,
    /// Approval timeout for strict-mode jobs. Defaults to 24h.
    pub approval_timeout: Duration,
    /// Trust-mode timeout in minutes. Defaults to 60.
    pub trust_timeout_mins: u64,
}

impl DaemonConfig {
    /// Begin building a [`DaemonConfig`] with required fields.
    pub fn builder(
        hub_url: impl Into<String>,
        device_jwt: impl Into<String>,
        identity_dir: impl Into<PathBuf>,
    ) -> DaemonConfigBuilder {
        DaemonConfigBuilder {
            hub_url: hub_url.into(),
            device_jwt: device_jwt.into(),
            identity_dir: identity_dir.into(),
            session_mode: SessionMode::AutoAccept,
            browser_enabled: false,
            heartbeat_interval: Duration::from_secs(60),
            device_id: None,
            max_concurrent_jobs: 8,
            approval_timeout: Duration::from_secs(86_400),
            trust_timeout_mins: 60,
        }
    }
}

/// Fluent builder for [`DaemonConfig`].
pub struct DaemonConfigBuilder {
    hub_url: String,
    device_jwt: String,
    identity_dir: PathBuf,
    session_mode: SessionMode,
    browser_enabled: bool,
    heartbeat_interval: Duration,
    device_id: Option<String>,
    max_concurrent_jobs: usize,
    approval_timeout: Duration,
    trust_timeout_mins: u64,
}

impl DaemonConfigBuilder {
    pub fn session_mode(mut self, mode: SessionMode) -> Self {
        self.session_mode = mode;
        self
    }
    pub fn browser_enabled(mut self, enabled: bool) -> Self {
        self.browser_enabled = enabled;
        self
    }
    pub fn heartbeat_interval(mut self, d: Duration) -> Self {
        self.heartbeat_interval = d;
        self
    }
    pub fn device_id(mut self, id: impl Into<String>) -> Self {
        self.device_id = Some(id.into());
        self
    }
    pub fn max_concurrent_jobs(mut self, n: usize) -> Self {
        self.max_concurrent_jobs = n;
        self
    }
    pub fn approval_timeout(mut self, d: Duration) -> Self {
        self.approval_timeout = d;
        self
    }
    pub fn trust_timeout_mins(mut self, m: u64) -> Self {
        self.trust_timeout_mins = m;
        self
    }
    pub fn build(self) -> DaemonConfig {
        DaemonConfig {
            hub_url: self.hub_url,
            device_jwt: self.device_jwt,
            identity_dir: self.identity_dir,
            session_mode: self.session_mode,
            browser_enabled: self.browser_enabled,
            heartbeat_interval: self.heartbeat_interval,
            device_id: self.device_id,
            max_concurrent_jobs: self.max_concurrent_jobs,
            approval_timeout: self.approval_timeout,
            trust_timeout_mins: self.trust_timeout_mins,
        }
    }
}

/// Coarse classification for errors surfaced via [`DaemonStatus::Error`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// Hub rejected the Hello handshake (401 / policy close / invalid JWT).
    Auth,
    /// Transport-level failure: unreachable hub, DNS, TLS, timeout.
    Network,
    /// Anything that doesn't fit the above.
    Other,
}

/// Lifecycle status emitted by [`DaemonHandle::subscribe_status`].
#[derive(Clone, Debug)]
pub enum DaemonStatus {
    /// Initial state, nothing attempted yet.
    Idle,
    /// Attempting to connect to the hub / negotiate the handshake.
    Connecting,
    /// Connected and handshaked successfully.
    Online { device_id: String },
    /// Disconnected cleanly (e.g. after `shutdown()`).
    Offline,
    /// The inner task returned an error.
    Error { kind: ErrorKind, message: String },
}

impl PartialEq for DaemonStatus {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (DaemonStatus::Idle, DaemonStatus::Idle) => true,
            (DaemonStatus::Connecting, DaemonStatus::Connecting) => true,
            (DaemonStatus::Online { device_id: a }, DaemonStatus::Online { device_id: b }) => {
                a == b
            }
            (DaemonStatus::Offline, DaemonStatus::Offline) => true,
            (
                DaemonStatus::Error { kind: ka, .. },
                DaemonStatus::Error { kind: kb, .. },
            ) => ka == kb,
            _ => false,
        }
    }
}

/// Handle returned by [`spawn`]. Drop-safe — `shutdown()` is the preferred
/// cleanup path, but dropping the handle also cancels the inner task via
/// the embedded `oneshot` sender going out of scope.
#[derive(Debug)]
pub struct DaemonHandle {
    shutdown_tx: Option<oneshot::Sender<()>>,
    join: JoinHandle<anyhow::Result<()>>,
    status_rx: watch::Receiver<DaemonStatus>,
    device_id: String,
}

impl DaemonHandle {
    /// Request a graceful shutdown and wait for the inner task to finish.
    pub async fn shutdown(mut self) -> anyhow::Result<()> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        match self.join.await {
            Ok(res) => res,
            Err(join_err) if join_err.is_cancelled() => Ok(()),
            Err(join_err) => Err(anyhow::anyhow!(join_err)),
        }
    }

    /// Snapshot of the current status (does not wait for a change).
    pub fn status(&self) -> DaemonStatus {
        self.status_rx.borrow().clone()
    }

    /// Subscribe to status transitions. Each subscriber gets its own cursor.
    pub fn subscribe_status(&self) -> watch::Receiver<DaemonStatus> {
        self.status_rx.clone()
    }

    /// Device ID assigned at spawn time. Stable for the lifetime of the handle.
    pub fn device_id(&self) -> &str {
        &self.device_id
    }
}

/// Spawn an `ahandd` instance wired against the cloud hub described by `config`.
///
/// Returns once the identity has been loaded and the background task has been
/// started. Status transitions (`Connecting → Online → Offline`/`Error`) are
/// surfaced through [`DaemonHandle::subscribe_status`].
pub async fn spawn(config: DaemonConfig) -> anyhow::Result<DaemonHandle> {
    // Load identity up-front so bad paths surface synchronously to the caller
    // rather than getting buried inside the spawned task.
    let identity_path = config.identity_dir.join(IDENTITY_FILE_NAME);
    let identity = DeviceIdentity::load_or_create(&identity_path)?;
    let device_id = config
        .device_id
        .clone()
        .unwrap_or_else(|| identity.device_id());

    // Catch misconfigured callers early in debug builds.
    debug_assert!(
        config.device_id.is_none()
            || config.device_id.as_deref() == Some(identity.device_id().as_str()),
        "DaemonConfig::device_id ({:?}) must equal SHA256(pubkey) = {:?}",
        config.device_id,
        identity.device_id()
    );

    let (status_tx, status_rx) = watch::channel(DaemonStatus::Connecting);
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let session_mgr = Arc::new(SessionManager::new(config.trust_timeout_mins));
    session_mgr.set_default_mode(config.session_mode).await;
    let approval_mgr = Arc::new(ApprovalManager::new(config.approval_timeout.as_secs()));
    let registry = Arc::new(JobRegistry::new(config.max_concurrent_jobs));
    let (approval_broadcast_tx, _) = broadcast::channel(64);
    let browser_mgr = Arc::new(BrowserManager::new(BrowserConfig {
        enabled: Some(config.browser_enabled),
        ..BrowserConfig::default()
    }));

    let inner_config = build_inner_config(&config, &identity_path);

    let status_tx_task = status_tx.clone();
    let device_id_for_task = device_id.clone();
    let reporter_device_id = device_id.clone();
    let reporter_status_tx = status_tx.clone();
    let reporter: Arc<dyn ClientReporter> =
        Arc::new(StatusReporter::new(reporter_status_tx, reporter_device_id));

    let join = tokio::spawn(async move {
        let run_fut = ahand_client::run_with_reporter(
            inner_config,
            device_id_for_task,
            registry,
            None,
            session_mgr,
            approval_mgr,
            approval_broadcast_tx,
            browser_mgr,
            reporter,
        );

        tokio::select! {
            res = run_fut => {
                match &res {
                    Ok(()) => {
                        let _ = status_tx_task.send(DaemonStatus::Offline);
                    }
                    Err(e) => {
                        let kind = classify_error(e);
                        let _ = status_tx_task.send(DaemonStatus::Error {
                            kind,
                            message: e.to_string(),
                        });
                    }
                }
                res
            }
            _ = shutdown_rx => {
                let _ = status_tx_task.send(DaemonStatus::Offline);
                Ok(())
            }
        }
    });

    Ok(DaemonHandle {
        shutdown_tx: Some(shutdown_tx),
        join,
        status_rx,
        device_id,
    })
}

/// Load (or create on first call) the Ed25519 device identity under `dir`.
///
/// Thin wrapper that joins `hub-device-identity.json` onto `dir` and delegates
/// to [`DeviceIdentity::load_or_create`]. Idempotent: subsequent calls read
/// the persisted key back.
pub fn load_or_create_identity(dir: &Path) -> anyhow::Result<DeviceIdentity> {
    DeviceIdentity::load_or_create(&dir.join(IDENTITY_FILE_NAME))
}

/// Classify an anyhow error into a broad [`ErrorKind`] for status reporting.
/// Operates on the stringified error message; this is a best-effort
/// classification. Structured error types from ahand_client will supersede
/// this in a future refactor.
fn classify_error(e: &anyhow::Error) -> ErrorKind {
    let s = e.to_string().to_lowercase();
    // NOTE: This matches on error message substrings, which is brittle.
    // Prefer structured errors from ahand_client when those become available.
    // Auth errors: hub explicitly mentions 401/unauthorized/jwt rejection.
    if s.contains("401") || s.contains("unauthorized") || s.contains("invalid jwt")
        || s.contains("jwt expired") || s.contains("auth rejected")
    {
        return ErrorKind::Auth;
    }
    // Network errors: transport failures from tungstenite/tokio-tungstenite.
    // Use specific patterns to avoid false-positives from application messages.
    if s.contains("connection refused")
        || s.contains("connection reset")
        || s.contains("dns error")
        || s.contains("timed out")
        || s.contains("broken pipe")
        || s.contains("host not found")
        || s.contains("no route to host")
        // TLS negotiation failures from rustls / native-tls:
        || s.contains("tls handshake")
        || s.contains("tls error")
        || s.contains("certificate")
    {
        return ErrorKind::Network;
    }
    ErrorKind::Other
}

/// Build an inner `config::Config` suitable for `ahand_client::run`.
fn build_inner_config(cfg: &DaemonConfig, identity_path: &Path) -> Config {
    Config {
        mode: Some("ahand-cloud".to_string()),
        server_url: cfg.hub_url.clone(),
        device_id: cfg.device_id.clone(),
        max_concurrent_jobs: Some(cfg.max_concurrent_jobs),
        data_dir: None,
        debug_ipc: Some(false),
        ipc_socket_path: None,
        ipc_socket_mode: None,
        trust_timeout_mins: Some(cfg.trust_timeout_mins),
        default_session_mode: Some(session_mode_str(cfg.session_mode).to_string()),
        policy: Default::default(),
        openclaw: None,
        browser: Some(BrowserConfig {
            enabled: Some(cfg.browser_enabled),
            ..BrowserConfig::default()
        }),
        hub: Some(HubConfig {
            bootstrap_token: if cfg.device_jwt.is_empty() {
                None
            } else {
                Some(cfg.device_jwt.clone())
            },
            private_key_path: Some(identity_path.to_string_lossy().into_owned()),
            // Expose the interval at both granularities so TOML-driven
            // setups (_secs) and programmatic sub-second tests (_ms) both
            // work. `heartbeat_interval_ms` takes precedence in
            // `run_with_reporter`.
            heartbeat_interval_secs: Some(cfg.heartbeat_interval.as_secs().max(1)),
            heartbeat_interval_ms: Some(
                u64::try_from(cfg.heartbeat_interval.as_millis())
                    .unwrap_or(u64::MAX)
                    .max(1),
            ),
        }),
    }
}

fn session_mode_str(mode: SessionMode) -> &'static str {
    match mode {
        SessionMode::AutoAccept => "auto_accept",
        SessionMode::Trust => "trust",
        SessionMode::Strict => "strict",
        SessionMode::Inactive => "inactive",
    }
}

/// Derive a stable fallback device ID from the identity directory path so
/// repeat launches with the same dir reuse the same ID. Callers that need a
/// specific ID should set `DaemonConfig::device_id` directly.
///
/// Note: kept for tests only. Production code uses [`DeviceIdentity::device_id`]
/// (SHA256 of pubkey) via `spawn()`.
#[cfg(test)]
fn default_device_id(identity_dir: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"ahandd-device-id:");
    hasher.update(identity_dir.as_os_str().as_encoded_bytes());
    let digest = hasher.finalize();
    format!("dev-{}", hex::encode(&digest[..8]))
}

/// Translates [`ConnectOutcome`] events into [`DaemonStatus`] updates.
///
/// The reporter lives for the lifetime of the spawned task; `run_with_reporter`
/// calls `report` synchronously from inside the tokio task so blocking here
/// would stall the client loop — `watch::Sender::send` is cheap and non-blocking.
struct StatusReporter {
    status_tx: watch::Sender<DaemonStatus>,
    device_id: String,
}

impl StatusReporter {
    fn new(status_tx: watch::Sender<DaemonStatus>, device_id: String) -> Self {
        Self {
            status_tx,
            device_id,
        }
    }
}

impl ClientReporter for StatusReporter {
    fn report(&self, outcome: ConnectOutcome) {
        let status = match outcome {
            ConnectOutcome::HandshakeAccepted => DaemonStatus::Online {
                device_id: self.device_id.clone(),
            },
            ConnectOutcome::HandshakeRejected(msg) => DaemonStatus::Error {
                kind: ErrorKind::Auth,
                message: msg,
            },
            ConnectOutcome::Session(msg) => DaemonStatus::Error {
                kind: classify_error(&anyhow::anyhow!(msg.clone())),
                message: msg,
            },
            ConnectOutcome::Disconnected => DaemonStatus::Connecting,
        };
        let _ = self.status_tx.send(status);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_error_detects_auth_markers() {
        assert_eq!(
            classify_error(&anyhow::anyhow!("HTTP error: 401 unauthorized")),
            ErrorKind::Auth
        );
        assert_eq!(
            classify_error(&anyhow::anyhow!("invalid jwt")),
            ErrorKind::Auth
        );
        assert_eq!(
            classify_error(&anyhow::anyhow!("hello auth rejected")),
            ErrorKind::Auth
        );
    }

    #[test]
    fn classify_error_detects_network_markers() {
        assert_eq!(
            classify_error(&anyhow::anyhow!("connection refused")),
            ErrorKind::Network
        );
        assert_eq!(
            classify_error(&anyhow::anyhow!("dns error: failed to resolve")),
            ErrorKind::Network
        );
        assert_eq!(
            classify_error(&anyhow::anyhow!("read timed out")),
            ErrorKind::Network
        );
    }

    #[test]
    fn classify_error_falls_through_to_other() {
        assert_eq!(
            classify_error(&anyhow::anyhow!("some unrelated failure")),
            ErrorKind::Other
        );
    }

    #[test]
    fn classify_error_auth_patterns() {
        let e = anyhow::anyhow!("hub returned 401 unauthorized");
        assert_eq!(classify_error(&e), ErrorKind::Auth);
        let e2 = anyhow::anyhow!("invalid jwt in header");
        assert_eq!(classify_error(&e2), ErrorKind::Auth);
    }

    #[test]
    fn classify_error_network_patterns() {
        let e = anyhow::anyhow!("connection refused (os error 111)");
        assert_eq!(classify_error(&e), ErrorKind::Network);
        let e2 = anyhow::anyhow!("timed out waiting for server");
        assert_eq!(classify_error(&e2), ErrorKind::Network);
    }

    #[test]
    fn classify_error_no_false_positives() {
        // "connect" in an application message must not match Network
        let e = anyhow::anyhow!("Postgres connection pool exhausted");
        assert_eq!(classify_error(&e), ErrorKind::Other);
        // "timeout" in an approval message must not match Network
        let e2 = anyhow::anyhow!("approval timeout expired");
        assert_eq!(classify_error(&e2), ErrorKind::Other);
        // "network" alone must not match
        let e3 = anyhow::anyhow!("network policy denied the request");
        assert_eq!(classify_error(&e3), ErrorKind::Other);
    }

    #[test]
    fn classify_error_tls_patterns() {
        // TLS negotiation failures should be Network, not Other.
        let e = anyhow::anyhow!("tls handshake failed: unexpected eof");
        assert_eq!(classify_error(&e), ErrorKind::Network);
        let e2 = anyhow::anyhow!("tls error: invalid server certificate");
        assert_eq!(classify_error(&e2), ErrorKind::Network);
        let e3 = anyhow::anyhow!("certificate verify failed");
        assert_eq!(classify_error(&e3), ErrorKind::Network);
    }

    #[test]
    fn session_mode_str_round_trips_known_values() {
        assert_eq!(session_mode_str(SessionMode::AutoAccept), "auto_accept");
        assert_eq!(session_mode_str(SessionMode::Trust), "trust");
        assert_eq!(session_mode_str(SessionMode::Strict), "strict");
        assert_eq!(session_mode_str(SessionMode::Inactive), "inactive");
    }

    #[test]
    fn default_device_id_is_stable_for_same_dir() {
        let a = default_device_id(Path::new("/tmp/ahand-a"));
        let b = default_device_id(Path::new("/tmp/ahand-a"));
        let c = default_device_id(Path::new("/tmp/ahand-b"));
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.starts_with("dev-"));
    }

    #[test]
    fn builder_applies_overrides() {
        let cfg = DaemonConfig::builder("ws://x/y", "tok", "/tmp/ahand-cfg")
            .session_mode(SessionMode::Strict)
            .browser_enabled(true)
            .heartbeat_interval(Duration::from_secs(5))
            .device_id("dev-xyz")
            .max_concurrent_jobs(2)
            .approval_timeout(Duration::from_secs(10))
            .trust_timeout_mins(30)
            .build();
        assert_eq!(cfg.hub_url, "ws://x/y");
        assert_eq!(cfg.device_jwt, "tok");
        assert_eq!(cfg.session_mode, SessionMode::Strict);
        assert!(cfg.browser_enabled);
        assert_eq!(cfg.heartbeat_interval, Duration::from_secs(5));
        assert_eq!(cfg.device_id.as_deref(), Some("dev-xyz"));
        assert_eq!(cfg.max_concurrent_jobs, 2);
        assert_eq!(cfg.approval_timeout, Duration::from_secs(10));
        assert_eq!(cfg.trust_timeout_mins, 30);
    }

    #[test]
    fn daemon_status_equality_ignores_error_message() {
        let a = DaemonStatus::Error {
            kind: ErrorKind::Auth,
            message: "one".into(),
        };
        let b = DaemonStatus::Error {
            kind: ErrorKind::Auth,
            message: "two".into(),
        };
        let c = DaemonStatus::Error {
            kind: ErrorKind::Network,
            message: "one".into(),
        };
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(DaemonStatus::Idle, DaemonStatus::Idle);
        assert_eq!(
            DaemonStatus::Online {
                device_id: "x".into()
            },
            DaemonStatus::Online {
                device_id: "x".into()
            }
        );
        assert_ne!(
            DaemonStatus::Online {
                device_id: "x".into()
            },
            DaemonStatus::Online {
                device_id: "y".into()
            }
        );
        assert_ne!(DaemonStatus::Idle, DaemonStatus::Connecting);
    }

    #[test]
    fn build_inner_config_maps_fields() {
        let cfg = DaemonConfig::builder("ws://h/w", "tok", "/tmp/ahand-inner")
            .browser_enabled(true)
            .max_concurrent_jobs(3)
            .trust_timeout_mins(45)
            .build();
        let inner = build_inner_config(&cfg, Path::new("/tmp/ahand-inner/id.json"));
        assert_eq!(inner.server_url, "ws://h/w");
        assert_eq!(inner.max_concurrent_jobs, Some(3));
        assert_eq!(inner.trust_timeout_mins, Some(45));
        let hub = inner.hub.unwrap();
        assert_eq!(hub.bootstrap_token.as_deref(), Some("tok"));
        assert_eq!(
            hub.private_key_path.as_deref(),
            Some("/tmp/ahand-inner/id.json")
        );
        assert_eq!(inner.browser.unwrap().enabled, Some(true));
    }

    #[test]
    fn build_inner_config_drops_empty_token() {
        let cfg = DaemonConfig::builder("ws://h/w", "", "/tmp/ahand-empty").build();
        let inner = build_inner_config(&cfg, Path::new("/tmp/ahand-empty/id.json"));
        assert!(inner.hub.unwrap().bootstrap_token.is_none());
    }

    #[test]
    fn status_reporter_maps_outcomes_to_statuses() {
        let (tx, rx) = watch::channel(DaemonStatus::Idle);
        let reporter = StatusReporter::new(tx, "dev-42".into());

        reporter.report(ConnectOutcome::HandshakeAccepted);
        assert_eq!(
            *rx.borrow(),
            DaemonStatus::Online {
                device_id: "dev-42".into()
            }
        );

        reporter.report(ConnectOutcome::HandshakeRejected("401".into()));
        assert!(matches!(
            *rx.borrow(),
            DaemonStatus::Error {
                kind: ErrorKind::Auth,
                ..
            }
        ));

        reporter.report(ConnectOutcome::Session("connection refused".into()));
        assert!(matches!(
            *rx.borrow(),
            DaemonStatus::Error {
                kind: ErrorKind::Network,
                ..
            }
        ));

        reporter.report(ConnectOutcome::Disconnected);
        assert_eq!(*rx.borrow(), DaemonStatus::Connecting);
    }

    #[tokio::test]
    async fn spawn_requires_valid_identity_dir() {
        // identity_dir points at a path where we cannot create the file —
        // on Unix, under /dev/null we expect load_or_create to fail with
        // a filesystem error, which spawn() should surface synchronously.
        let bogus = Path::new("/dev/null/cannot-create-here");
        let cfg = DaemonConfig::builder("ws://127.0.0.1:1/ws", "tok", bogus).build();
        let err = spawn(cfg).await.unwrap_err();
        // Just assert an error was produced — the exact message varies by OS.
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn load_or_create_identity_is_idempotent() {
        let dir = std::env::temp_dir().join(format!(
            "ahandd-public-api-identity-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let a = load_or_create_identity(&dir).unwrap();
        let b = load_or_create_identity(&dir).unwrap();
        assert_eq!(a.public_key_bytes(), b.public_key_bytes());
        // Cleanup
        let _ = std::fs::remove_file(dir.join(IDENTITY_FILE_NAME));
        let _ = std::fs::remove_dir(&dir);
    }
}
