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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::sync::{Mutex as AsyncMutex, broadcast, oneshot, watch};
use tokio::task::JoinHandle;

pub use ahand_protocol::{ApprovalRequest, SessionMode};
use ahand_protocol::{Envelope, envelope};

use crate::ahand_client::{self, ClientReporter, ConnectOutcome};
use crate::app_tool_registry::AppToolRegistry;
use crate::approval::ApprovalManager;
use crate::browser::BrowserManager;
use crate::config::{BrowserConfig, Config, FilePolicyConfig, HubConfig};
use crate::device_identity::DeviceIdentity;
use crate::registry::JobRegistry;
use crate::sandbox::{
    CommitResult, FileVersion, HostFileRef, NetworkPolicy, PermissionSnapshot,
    RegisterVersionRequest, RegisteredExecEnvironment, RuntimeExecuteRequest, RuntimeExecuteResult,
    RuntimeProviderConfig, SandboxExecRequest, SandboxExecResult, SandboxFile,
    SandboxPermissionMode, SandboxResult, SandboxSessionConfig, file_lifecycle, path_policy,
    registry::SandboxRegistry,
    runner::{self, PlatformExecuteRequest, RuntimeSandboxPolicy},
};
use crate::session::SessionManager;

pub use crate::app_tool_registry::{
    AppToolArgsHandler, AppToolDef, AppToolError, AppToolHandler, AppToolInvocation,
    args_only_handler,
};

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
            (DaemonStatus::Error { kind: ka, .. }, DaemonStatus::Error { kind: kb, .. }) => {
                ka == kb
            }
            _ => false,
        }
    }
}

/// Cursor over daemon approval requests for embedding UIs.
pub struct ApprovalSubscription {
    rx: broadcast::Receiver<Envelope>,
}

impl ApprovalSubscription {
    fn new(rx: broadcast::Receiver<Envelope>) -> Self {
        Self { rx }
    }

    pub async fn recv(&mut self) -> Option<ApprovalRequest> {
        loop {
            match self.rx.recv().await {
                Ok(envelope) => match envelope.payload {
                    Some(envelope::Payload::ApprovalRequest(req)) => return Some(req),
                    _ => continue,
                },
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

/// Handle returned by [`spawn`]. Drop-safe — `shutdown()` is the preferred
/// cleanup path, but dropping the handle also cancels the inner task via
/// the embedded `oneshot` sender going out of scope.
pub struct DaemonHandle {
    shutdown_tx: Option<oneshot::Sender<()>>,
    join: JoinHandle<anyhow::Result<()>>,
    status_rx: watch::Receiver<DaemonStatus>,
    device_id: String,
    app_tools: Arc<AppToolRegistry>,
    approval_broadcast_tx: broadcast::Sender<Envelope>,
    approval_mgr: Arc<ApprovalManager>,
    session_mgr: Arc<SessionManager>,
    sandbox_registry: Arc<AsyncMutex<SandboxRegistry>>,
}

impl std::fmt::Debug for DaemonHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonHandle")
            .field("device_id", &self.device_id)
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

static ACTIVE_DAEMONS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

#[derive(Debug)]
struct ActiveDaemonGuard {
    key: String,
}

impl Drop for ActiveDaemonGuard {
    fn drop(&mut self) {
        let mut active = active_daemons()
            .lock()
            .expect("active daemon registry poisoned");
        active.remove(&self.key);
    }
}

fn active_daemons() -> &'static Mutex<HashSet<String>> {
    ACTIVE_DAEMONS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn reserve_active_daemon(hub_url: &str, device_id: &str) -> anyhow::Result<ActiveDaemonGuard> {
    let key = format!("{hub_url}\0{device_id}");
    let mut active = active_daemons()
        .lock()
        .expect("active daemon registry poisoned");
    if !active.insert(key.clone()) {
        anyhow::bail!("ahandd daemon already running for hub URL {hub_url} and device {device_id}");
    }
    Ok(ActiveDaemonGuard { key })
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

    /// Register an app-defined tool. The daemon will advertise an updated
    /// snapshot to the hub immediately (if connected) and after every
    /// subsequent reconnect.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - `def.name` does not match `^[a-z0-9_-]{1,64}$`
    /// - `def.input_schema` is not a JSON object
    /// - a tool with the same name is already registered
    ///
    /// A failed registration does not change the advertised catalog; the hub
    /// sees no new snapshot and the revision is not incremented.
    ///
    /// # Handler contract
    ///
    /// Handlers should complete promptly or honor cancellation by finishing.
    /// A handler that never returns permanently consumes one of the 4
    /// per-device concurrency slots: a timeout response is sent to the caller
    /// when the configured `timeout_ms` elapses, but the slot is only released
    /// when the handler task actually finishes (or is dropped). Four stuck
    /// handlers will disable app tools on the device until daemon restart.
    pub async fn register_app_tool(
        &self,
        def: AppToolDef,
        handler: AppToolHandler,
    ) -> anyhow::Result<()> {
        self.app_tools.register(def, handler).await
    }

    /// Unregister an app-defined tool by name. Returns `true` if the tool
    /// existed. The daemon pushes a new snapshot (without the tool) to the
    /// hub immediately if connected.
    pub async fn unregister_app_tool(&self, name: &str) -> bool {
        self.app_tools.unregister(name).await
    }

    /// Subscribe to in-process approval requests.
    ///
    /// Job, file, and app-tool paths all broadcast here. Each subscriber
    /// receives every request independently (broadcast semantics). Lagged
    /// subscribers automatically skip missed messages rather than blocking.
    ///
    /// # When requests appear
    ///
    /// Under the builder default [`SessionMode::AutoAccept`], approval requests
    /// fire **only** for app tools registered with `requires_approval: true`
    /// (and for policy-escalated file operations). Under
    /// [`SessionMode::Strict`] every job, file, and app-tool call goes through
    /// approval. An embedder subscribing under the default configuration may
    /// therefore never receive a request unless it registers tools with
    /// `requires_approval: true` or explicitly switches to `Strict` mode.
    ///
    /// ## `job_id` namespaces
    ///
    /// The `job_id` field on each [`ApprovalRequest`] is already namespaced:
    ///
    /// 1. **Real jobs** - raw `job_id` from the cloud `JobRequest`; tool is
    ///    whatever the cloud sent (e.g. `"bash"`, `"computer"`).
    /// 2. **File requests** - `"file-req:{request_id}"`; tool is `"file"`.
    ///    The prefix prevents a `request_id` from evicting a same-named real
    ///    job entry.
    /// 3. **App-tool calls** - `"app-tool:{tool_call_id}"`; tool is
    ///    `"app:{name}"`. The prefix prevents a cloud-chosen `tool_call_id`
    ///    from colliding with a real job's pending entry.
    ///
    /// Pass the `job_id` verbatim to [`DaemonHandle::respond_approval`].
    ///
    /// ## Expiry
    ///
    /// Each request carries an `expires_ms` timestamp derived from
    /// [`DaemonConfig::approval_timeout`]. For app tools the effective window
    /// is further bounded by the call's own `timeout_ms`: approval must arrive
    /// within `min(approval_timeout, timeout_ms)` of the request being sent.
    ///
    /// ## Subscribe-before-traffic and multi-subscriber semantics
    ///
    /// This channel does **not** replay missed messages - subscribe before
    /// sending traffic (or before spawning the task that will send traffic).
    /// Every active subscriber independently receives every request.
    pub fn subscribe_approvals(&self) -> ApprovalSubscription {
        ApprovalSubscription::new(self.approval_broadcast_tx.subscribe())
    }

    /// Answer a pending approval from the embedding application.
    ///
    /// Mirrors the WS/IPC `ApprovalResponse` handling exactly:
    /// - deny with non-empty `reason` -> `resolve` + `record_refusal`
    /// - approve or deny without reason -> `resolve` only
    ///
    /// The refusal log is keyed by tool name today (`SessionManager::record_refusal`
    /// ignores the caller principal). The `"local"` principal passed internally
    /// is a forward-looking sentinel for when per-principal tracking is added.
    ///
    /// `job_id` is [`ApprovalRequest::job_id`] verbatim (already namespaced,
    /// e.g. `"app-tool:{id}"`).
    ///
    /// Returns `false` when no matching pending approval exists (already
    /// resolved or expired).
    pub async fn respond_approval(&self, job_id: &str, approved: bool, reason: &str) -> bool {
        let resp = ahand_protocol::ApprovalResponse {
            job_id: job_id.to_string(),
            approved,
            reason: reason.to_string(),
            remember: false,
        };
        crate::approval::apply_approval_response(
            &self.approval_mgr,
            &self.session_mgr,
            &resp,
            "local",
        )
        .await
    }

    pub async fn create_sandbox_session(&self, config: SandboxSessionConfig) -> SandboxResult<()> {
        self.sandbox_registry.lock().await.create_session(config)
    }

    pub async fn update_sandbox_permission_mode(
        &self,
        session_id: &str,
        mode: SandboxPermissionMode,
    ) -> SandboxResult<PermissionSnapshot> {
        self.sandbox_registry
            .lock()
            .await
            .update_permission(session_id, mode)
    }

    pub async fn register_sandbox_runtime(
        &self,
        session_id: &str,
        provider: RuntimeProviderConfig,
    ) -> SandboxResult<()> {
        let provider = canonicalize_runtime_provider(provider)?;
        let mut registry = self.sandbox_registry.lock().await;
        let session = registry.session_mut(session_id)?;
        session.runtimes.insert(provider.name.clone(), provider);
        Ok(())
    }

    pub async fn import_sandbox_file(
        &self,
        session_id: &str,
        file_ref: HostFileRef,
    ) -> SandboxResult<SandboxFile> {
        let mut registry = self.sandbox_registry.lock().await;
        file_lifecycle::import_file(&mut registry, session_id, file_ref)
    }

    pub async fn execute_sandbox_command(
        &self,
        session_id: &str,
        request: SandboxExecRequest,
    ) -> SandboxResult<SandboxExecResult> {
        execute_sandbox_command_with_registry(
            Arc::clone(&self.sandbox_registry),
            session_id,
            request,
        )
        .await
    }

    pub async fn execute_sandbox_runtime(
        &self,
        session_id: &str,
        request: RuntimeExecuteRequest,
    ) -> SandboxResult<RuntimeExecuteResult> {
        let provider = {
            let registry = self.sandbox_registry.lock().await;
            let session = registry.session(session_id)?;
            session
                .runtimes
                .get(&request.runtime)
                .cloned()
                .ok_or_else(|| {
                    crate::sandbox::SandboxError::runtime_not_registered(format!(
                        "sandbox runtime '{}' is not registered",
                        request.runtime
                    ))
                })?
        };

        let mut env = provider.env.clone();
        env.extend(request.env);
        let command = std::iter::once(provider.executable.to_string_lossy().to_string())
            .chain(request.args)
            .collect::<Vec<_>>();

        self.execute_sandbox_command(
            session_id,
            SandboxExecRequest {
                command,
                cwd: request.cwd,
                env,
                timeout: request.timeout.or(Some(provider.default_timeout)),
            },
        )
        .await
    }

    pub async fn register_sandbox_file_version(
        &self,
        session_id: &str,
        request: RegisterVersionRequest,
    ) -> SandboxResult<FileVersion> {
        let mut registry = self.sandbox_registry.lock().await;
        file_lifecycle::register_file_version(&mut registry, session_id, request)
    }

    pub async fn list_sandbox_file_versions(
        &self,
        session_id: &str,
    ) -> SandboxResult<Vec<FileVersion>> {
        let registry = self.sandbox_registry.lock().await;
        file_lifecycle::list_file_versions(&registry, session_id)
    }

    pub async fn commit_sandbox_file_version(
        &self,
        session_id: &str,
        version_id: &str,
    ) -> SandboxResult<CommitResult> {
        let mut registry = self.sandbox_registry.lock().await;
        file_lifecycle::commit_file_version(&mut registry, session_id, version_id)
    }

    pub async fn confirm_sandbox_file_version_overwrite(
        &self,
        session_id: &str,
        version_id: &str,
    ) -> SandboxResult<CommitResult> {
        let mut registry = self.sandbox_registry.lock().await;
        file_lifecycle::confirm_file_version_overwrite(&mut registry, session_id, version_id)
    }

    pub async fn save_sandbox_file_version_as(
        &self,
        session_id: &str,
        version_id: &str,
        target_path: &Path,
    ) -> SandboxResult<CommitResult> {
        let mut registry = self.sandbox_registry.lock().await;
        file_lifecycle::save_file_version_as(
            &mut registry,
            session_id,
            version_id,
            target_path.to_path_buf(),
        )
    }
}

pub(crate) async fn execute_sandbox_command_with_registry(
    sandbox_registry: Arc<AsyncMutex<SandboxRegistry>>,
    session_id: &str,
    request: SandboxExecRequest,
) -> SandboxResult<SandboxExecResult> {
    let (workspace_root, network, exec_env): (PathBuf, NetworkPolicy, RegisteredExecEnvironment) = {
        let registry = sandbox_registry.lock().await;
        let session = registry.session(session_id)?;
        (
            session.workspace_root.clone(),
            session.network,
            session.exec_environment(),
        )
    };
    let SandboxExecRequest {
        command,
        cwd,
        env: request_env,
        timeout: request_timeout,
    } = request;
    let (program, args) = command.split_first().ok_or_else(|| {
        crate::sandbox::SandboxError::invalid_command("sandbox command must not be empty")
    })?;

    std::fs::create_dir_all(&workspace_root).map_err(|e| {
        crate::sandbox::SandboxError::unavailable(format!(
            "failed to create sandbox workspace root: {e}"
        ))
    })?;
    let cwd = match cwd {
        Some(cwd) => {
            path_policy::resolve_existing_sandbox_path(&workspace_root, &cwd.to_string_lossy())?
        }
        None => workspace_root.canonicalize().map_err(|e| {
            crate::sandbox::SandboxError::invalid_sandbox_path(format!(
                "failed to resolve sandbox workspace root: {e}"
            ))
        })?,
    };
    let executable = runner::resolve_executable(program, &exec_env.path_entries)?;
    let mut env = exec_env.env;
    merge_path_entries(&mut env, &exec_env.path_entries);
    env.extend(request_env);
    let timeout = request_timeout.unwrap_or(exec_env.default_timeout);
    let policy = RuntimeSandboxPolicy {
        writable_root: workspace_root,
        readonly_roots: exec_env.readonly_roots,
        network,
    };

    runner::execute(PlatformExecuteRequest {
        executable,
        args: args.to_vec(),
        cwd,
        env,
        timeout,
        policy,
    })
    .await
}

fn canonicalize_runtime_provider(
    mut provider: RuntimeProviderConfig,
) -> SandboxResult<RuntimeProviderConfig> {
    let executable_entry = if provider.executable.is_absolute() {
        provider.executable.clone()
    } else {
        std::env::current_dir()
            .map_err(|e| {
                crate::sandbox::SandboxError::unavailable(format!(
                    "failed to resolve current directory for sandbox runtime executable: {e}"
                ))
            })?
            .join(&provider.executable)
    };
    executable_entry.canonicalize().map_err(|e| {
        crate::sandbox::SandboxError::unavailable(format!(
            "failed to resolve sandbox runtime executable '{}': {e}",
            executable_entry.display()
        ))
    })?;
    provider.executable = executable_entry;
    provider.readonly_roots = provider
        .readonly_roots
        .into_iter()
        .map(|root| {
            root.canonicalize().map_err(|e| {
                crate::sandbox::SandboxError::unavailable(format!(
                    "failed to resolve sandbox runtime readonly root '{}': {e}",
                    root.display()
                ))
            })
        })
        .collect::<SandboxResult<Vec<_>>>()?;
    provider.readonly_roots.sort();
    provider.readonly_roots.dedup();
    Ok(provider)
}

fn merge_path_entries(env: &mut HashMap<String, String>, path_entries: &[PathBuf]) {
    let separator = if cfg!(windows) { ";" } else { ":" };
    let prefix = path_entries
        .iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(separator);
    if prefix.is_empty() {
        return;
    }
    let path = match env.get("PATH") {
        Some(existing) if !existing.is_empty() => format!("{prefix}{separator}{existing}"),
        _ => prefix,
    };
    env.insert("PATH".to_string(), path);
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
    let active_guard = reserve_active_daemon(&config.hub_url, &device_id)?;

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
    // FileManager is policy-driven; library callers don't yet expose
    // file-policy config so we hand it the inner config's `file_policy`
    // (defaulted in `build_inner_config`).
    let file_policy_cfg = inner_config.file_policy.clone().unwrap_or_default();
    let file_mgr = Arc::new(crate::file_manager::FileManager::new(&file_policy_cfg));
    let app_tools = Arc::new(AppToolRegistry::new());

    let status_tx_task = status_tx.clone();
    let device_id_for_task = device_id.clone();
    let reporter_device_id = device_id.clone();
    let reporter_status_tx = status_tx.clone();
    let reporter: Arc<dyn ClientReporter> =
        Arc::new(StatusReporter::new(reporter_status_tx, reporter_device_id));

    let approval_broadcast_tx_for_handle = approval_broadcast_tx.clone();
    let approval_mgr_for_handle = Arc::clone(&approval_mgr);
    let session_mgr_for_handle = Arc::clone(&session_mgr);

    let app_tools_for_task = Arc::clone(&app_tools);
    let join = tokio::spawn(async move {
        let _active_guard = active_guard;
        let run_fut = ahand_client::run_with_reporter(
            inner_config,
            device_id_for_task,
            registry,
            None,
            session_mgr,
            approval_mgr,
            approval_broadcast_tx,
            browser_mgr,
            file_mgr,
            app_tools_for_task,
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
        app_tools,
        approval_broadcast_tx: approval_broadcast_tx_for_handle,
        approval_mgr: approval_mgr_for_handle,
        session_mgr: session_mgr_for_handle,
        sandbox_registry: Arc::new(AsyncMutex::new(SandboxRegistry::default())),
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
    if s.contains("401")
        || s.contains("unauthorized")
        || s.contains("invalid jwt")
        || s.contains("jwt expired")
        || s.contains("auth rejected")
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
        // Embedded library consumers currently don't expose a file-policy
        // surface. Keep the Mac app path functional by defaulting embedded
        // daemons to a permissive policy; callers that need tighter controls
        // should get an explicit builder option next.
        file_policy: Some(permissive_embedded_file_policy()),
    }
}

fn permissive_embedded_file_policy() -> FilePolicyConfig {
    FilePolicyConfig {
        enabled: true,
        // "/**" matches Unix absolute paths; "**" matches Windows
        // drive-letter paths (glob components carry no leading separator on
        // Windows, so "/**" alone would deny everything there).
        path_allowlist: vec!["/**".to_string(), "**".to_string()],
        ..FilePolicyConfig::default()
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

    #[tokio::test]
    async fn execute_sandbox_command_rejects_empty_command() {
        let temp = tempfile::tempdir().unwrap();
        let identity_dir = temp.path().join("identity");
        let workspace_root = temp.path().join("sandbox");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let cfg = DaemonConfig::builder("ws://127.0.0.1:9/ws", "test-token", &identity_dir)
            .heartbeat_interval(Duration::from_millis(50))
            .build();
        let handle = spawn(cfg).await.unwrap();
        handle
            .create_sandbox_session(SandboxSessionConfig {
                session_id: "session-1".to_string(),
                permission_mode: SandboxPermissionMode::Readonly,
                workspace_root,
                network: NetworkPolicy::Enabled,
            })
            .await
            .unwrap();

        let err = handle
            .execute_sandbox_command(
                "session-1",
                SandboxExecRequest {
                    command: vec![],
                    cwd: None,
                    env: HashMap::new(),
                    timeout: Some(Duration::from_secs(1)),
                },
            )
            .await
            .unwrap_err();

        assert_eq!(err.code, "INVALID_COMMAND");
        handle.shutdown().await.unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn canonicalize_runtime_provider_preserves_executable_entry_path() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let bin = temp.path().join("runtime").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let alias = bin.join("python");
        symlink("/bin/echo", &alias).unwrap();

        let provider = canonicalize_runtime_provider(RuntimeProviderConfig {
            name: "python".to_string(),
            executable: alias.clone(),
            readonly_roots: vec![bin.clone(), PathBuf::from("/bin")],
            env: HashMap::new(),
            default_timeout: Duration::from_secs(10),
        })
        .unwrap();

        assert_eq!(provider.executable, alias);
        assert!(
            provider
                .readonly_roots
                .contains(&bin.canonicalize().unwrap())
        );
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

    #[tokio::test]
    async fn permissive_embedded_policy_admits_native_paths() {
        // Regression pin for the Windows port: the permissive allowlist used
        // to be only "/**", which never matches drive-letter paths
        // (C:\...), so every embedded-library file op was PolicyDenied on
        // Windows. This must pass on all three CI platforms.
        use ahand_protocol::{FileRequest, FileStat, file_request, file_response};
        let dir = std::env::temp_dir().join(format!(
            "ahandd-public-api-permissive-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let probe = dir.join("probe.txt");
        std::fs::write(&probe, "ok").unwrap();

        let mgr = crate::file_manager::FileManager::new(&permissive_embedded_file_policy());
        let req = FileRequest {
            request_id: "permissive".into(),
            operation: Some(file_request::Operation::Stat(FileStat {
                path: probe.to_string_lossy().into_owned(),
                no_follow_symlink: false,
            })),
        };
        let resp = mgr.handle(&req).await;
        assert!(
            matches!(resp.result, Some(file_response::Result::Stat(_))),
            "permissive policy denied a native path: {:?}",
            resp.result
        );
        // Cleanup
        let _ = std::fs::remove_file(&probe);
        let _ = std::fs::remove_dir(&dir);
    }
}
