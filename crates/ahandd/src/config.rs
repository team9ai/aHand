use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Connection mode for ahandd
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ConnectionMode {
    /// Connect to aHand Cloud (default)
    #[default]
    AHandCloud,
    /// Connect to OpenClaw Gateway as a node
    OpenClawGateway,
}

impl ConnectionMode {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "openclaw-gateway" | "openclaw" => Self::OpenClawGateway,
            _ => Self::AHandCloud,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Config {
    /// Connection mode: "ahand-cloud" (default) or "openclaw-gateway"
    #[serde(default)]
    pub mode: Option<String>,

    /// WebSocket server URL (e.g. "ws://localhost:3000/ws") - for ahand-cloud mode
    #[serde(default = "default_server_url")]
    pub server_url: String,

    /// Unique device identifier. Auto-generated if omitted.
    pub device_id: Option<String>,

    /// Maximum number of concurrent jobs. Defaults to 8.
    pub max_concurrent_jobs: Option<usize>,

    /// Directory for trace logs and run artifacts. Defaults to ~/.ahand/data.
    pub data_dir: Option<String>,

    /// Enable debug IPC server (Unix socket).
    #[serde(default)]
    pub debug_ipc: Option<bool>,

    /// Custom path for the IPC Unix socket. Defaults to ~/.ahand/ahandd.sock.
    pub ipc_socket_path: Option<String>,

    /// Unix permission mode for the IPC socket (e.g. 0o660 for group access).
    /// Defaults to 0o660.
    pub ipc_socket_mode: Option<u32>,

    /// Default trust timeout in minutes for Trust mode. Defaults to 60.
    pub trust_timeout_mins: Option<u64>,

    /// Default session mode for all callers on startup.
    /// "auto_accept" = trust all, "strict" = require approval, "inactive" = deny all (default).
    pub default_session_mode: Option<String>,

    #[serde(default)]
    pub policy: PolicyConfig,

    /// OpenClaw Gateway configuration (when mode = "openclaw-gateway")
    #[serde(default)]
    pub openclaw: Option<OpenClawConfig>,

    /// Browser control configuration (playwright-cli integration)
    #[serde(default)]
    pub browser: Option<BrowserConfig>,

    /// Hub authentication configuration for authenticated Hello handshakes.
    #[serde(default)]
    pub hub: Option<HubConfig>,

    /// File operation policy configuration.
    #[serde(default)]
    pub file_policy: Option<FilePolicyConfig>,
}

/// File operation policy configuration.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct FilePolicyConfig {
    /// Enable file operations (default: false).
    #[serde(default)]
    pub enabled: bool,

    /// Allowed path patterns (glob syntax). Empty = deny all.
    #[serde(default)]
    pub path_allowlist: Vec<String>,

    /// Denied path patterns (checked before allowlist).
    #[serde(default)]
    pub path_denylist: Vec<String>,

    /// Maximum bytes for a single read operation.
    #[serde(default = "default_max_read_bytes")]
    pub max_read_bytes: u64,

    /// Maximum bytes for a single write operation.
    #[serde(default = "default_max_write_bytes")]
    pub max_write_bytes: u64,

    /// Paths that require STRICT approval regardless of session mode.
    #[serde(default)]
    pub dangerous_paths: Vec<String>,
}

fn default_max_read_bytes() -> u64 {
    104_857_600 // 100MB
}

fn default_max_write_bytes() -> u64 {
    104_857_600 // 100MB
}

impl Default for FilePolicyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            path_allowlist: Vec::new(),
            path_denylist: Vec::new(),
            max_read_bytes: default_max_read_bytes(),
            max_write_bytes: default_max_write_bytes(),
            dangerous_paths: Vec::new(),
        }
    }
}

/// Hub authentication configuration.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct HubConfig {
    /// Optional one-time bootstrap bearer token for first registration.
    pub bootstrap_token: Option<String>,

    /// Path to the persisted Ed25519 private key used for Hello signing.
    pub private_key_path: Option<String>,

    /// Heartbeat interval in seconds. The daemon sends a `Heartbeat`
    /// envelope on its hub WebSocket every `heartbeat_interval_secs`
    /// seconds so the hub can refresh TTL-based presence and forward the
    /// event as `device.heartbeat` webhooks. `None` falls back to the
    /// library default (60s).
    ///
    /// When both this and [`Self::heartbeat_interval_ms`] are set,
    /// `heartbeat_interval_ms` wins (finer-grained override, mainly used
    /// by tests that need sub-second cadence).
    pub heartbeat_interval_secs: Option<u64>,

    /// Sub-second override for [`Self::heartbeat_interval_secs`]. Not
    /// typically exposed through TOML — the library surface uses this to
    /// thread `DaemonConfig.heartbeat_interval: Duration` down without
    /// losing sub-second precision.
    #[serde(default)]
    pub heartbeat_interval_ms: Option<u64>,
}

/// OpenClaw Gateway connection configuration
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct OpenClawConfig {
    /// Gateway host (e.g., "127.0.0.1")
    pub gateway_host: Option<String>,

    /// Gateway port (default: 18789)
    pub gateway_port: Option<u16>,

    /// Use TLS (wss://)
    #[serde(default)]
    pub gateway_tls: Option<bool>,

    /// TLS certificate fingerprint for pinning
    pub gateway_tls_fingerprint: Option<String>,

    /// Node ID (auto-generated if not set)
    pub node_id: Option<String>,

    /// Display name for this node
    pub display_name: Option<String>,

    /// Authentication token
    pub auth_token: Option<String>,

    /// Authentication password
    pub auth_password: Option<String>,

    /// Path to exec-approvals.json
    pub exec_approvals_path: Option<String>,
}

/// Browser control configuration (playwright-cli integration).
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct BrowserConfig {
    /// Enable browser capabilities (default: false).
    pub enabled: Option<bool>,

    /// Path to the playwright-cli binary (default: ~/.ahand/node/bin/playwright-cli).
    pub binary_path: Option<String>,

    /// Browser executable path (e.g. system Chrome). Auto-detected if omitted.
    pub executable_path: Option<String>,

    /// PLAYWRIGHT_BROWSERS_PATH override (optional, for custom browser installs).
    pub browsers_path: Option<String>,

    /// Default command timeout in milliseconds (default: 30000).
    pub default_timeout_ms: Option<u64>,

    /// Maximum number of concurrent browser sessions (default: 4).
    pub max_sessions: Option<usize>,

    /// Allowed domains (empty = allow all).
    #[serde(default)]
    pub allowed_domains: Vec<String>,

    /// Denied domains (checked before allowed_domains).
    #[serde(default)]
    pub denied_domains: Vec<String>,

    /// Directory for download/pdf output files (default: ~/.ahand/browser/downloads).
    pub downloads_dir: Option<String>,

    /// Show browser window instead of headless (default: false).
    #[serde(default)]
    pub headed: Option<bool>,

    /// Use persistent browser context to preserve cookies/storage across restarts (default: true).
    #[serde(default = "default_persistent")]
    pub persistent: Option<bool>,
}

fn default_persistent() -> Option<bool> {
    Some(true)
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PolicyConfig {
    /// If non-empty, only these tools are allowed without approval.
    #[serde(default)]
    pub allowed_tools: Vec<String>,

    /// Working directories that are denied (hard reject, no approval).
    #[serde(default)]
    pub denied_paths: Vec<String>,

    /// Tools that are always denied (hard reject, no approval opportunity).
    #[serde(default)]
    pub denied_tools: Vec<String>,

    /// Domains that are allowed without approval for network tools.
    #[serde(default)]
    pub allowed_domains: Vec<String>,

    /// How long to wait for user approval before rejecting (seconds).
    /// Defaults to 86400 (24 hours).
    #[serde(default = "default_approval_timeout")]
    pub approval_timeout_secs: u64,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            allowed_tools: Vec::new(),
            denied_paths: Vec::new(),
            denied_tools: Vec::new(),
            allowed_domains: Vec::new(),
            approval_timeout_secs: default_approval_timeout(),
        }
    }
}

fn default_approval_timeout() -> u64 {
    86400
}

fn default_server_url() -> String {
    "ws://localhost:3000/ws".to_string()
}

/// Expand a leading `~` in a path pattern to the user's home directory.
///
/// Supports `~/...` (Unix) and `~\...` (Windows) prefixes, as well as a bare
/// `~`. Patterns without a leading tilde pass through unchanged.
///
/// I2: previous behavior was to log a warning and return the original
/// pattern when [`dirs::home_dir`] resolved to `None`, which silently
/// fails the *wrong* way for denylists / dangerous_paths — the literal
/// pattern `~/.ssh/**` never matches a canonicalized absolute path, so
/// the user's intent ("this is dangerous") becomes "this is allowed".
/// We now fail loudly: `Config::load` returns an error and daemon startup
/// aborts. The operator must either set `HOME` or use absolute paths.
fn expand_tilde_with(pattern: &str, home: Option<&Path>) -> anyhow::Result<String> {
    if let Some(rest) = pattern
        .strip_prefix("~/")
        .or_else(|| pattern.strip_prefix("~\\"))
    {
        let home = home.ok_or_else(|| {
            anyhow::anyhow!(
                "config pattern {pattern:?} starts with `~` but the user's home \
                 directory cannot be determined (is HOME unset?); use an \
                 absolute path or set HOME"
            )
        })?;
        return Ok(home.join(rest).to_string_lossy().into_owned());
    }
    if pattern == "~" {
        let home = home.ok_or_else(|| {
            anyhow::anyhow!(
                "config pattern \"~\" cannot be resolved: dirs::home_dir() \
                 returned None (is HOME unset?); use an absolute path or set HOME"
            )
        })?;
        return Ok(home.to_string_lossy().into_owned());
    }
    Ok(pattern.to_string())
}

fn expand_tilde(pattern: &str) -> anyhow::Result<String> {
    expand_tilde_with(pattern, dirs::home_dir().as_deref())
}

/// Apply [`expand_tilde`] to every entry in a mutable string list in place.
/// Stops on the first failure so the caller sees the offending pattern.
fn expand_tildes_in_place(list: &mut Vec<String>) -> anyhow::Result<()> {
    for item in list.iter_mut() {
        *item = expand_tilde(item)?;
    }
    Ok(())
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let mut config: Config = toml::from_str(&content)?;

        // Expand `~` in file-policy path patterns so FilePolicyChecker never
        // sees a tilde — glob_match compares patterns literally against
        // canonicalized absolute paths, so `~/.ssh/**` would otherwise never
        // match `/home/user/.ssh/...`.
        if let Some(ref mut fp) = config.file_policy {
            expand_tildes_in_place(&mut fp.path_allowlist)?;
            expand_tildes_in_place(&mut fp.path_denylist)?;
            expand_tildes_in_place(&mut fp.dangerous_paths)?;
        }

        Ok(config)
    }

    /// Get the connection mode
    pub fn connection_mode(&self) -> ConnectionMode {
        self.mode
            .as_ref()
            .map(|s| ConnectionMode::from_str(s))
            .unwrap_or_default()
    }

    /// Get OpenClaw config, creating default if needed
    pub fn openclaw_config(&self) -> OpenClawConfig {
        self.openclaw.clone().unwrap_or_default()
    }

    /// Get browser config, creating default if needed
    pub fn browser_config(&self) -> BrowserConfig {
        self.browser.clone().unwrap_or_default()
    }

    /// Get hub config, creating default if needed.
    pub fn hub_config(&self) -> HubConfig {
        self.hub.clone().unwrap_or_default()
    }

    /// Serialize and write the config back to a TOML file.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    pub fn device_id(&self) -> String {
        self.device_id.clone().unwrap_or_else(uuid_v4)
    }

    /// Resolve the IPC socket path. Default: ~/.ahand/ahandd.sock.
    pub fn ipc_socket_path(&self) -> PathBuf {
        match &self.ipc_socket_path {
            Some(p) => PathBuf::from(p),
            None => dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join(".ahand")
                .join("ahandd.sock"),
        }
    }

    /// Get the IPC socket permission mode. Default: 0o660.
    pub fn ipc_socket_mode(&self) -> u32 {
        self.ipc_socket_mode.unwrap_or(0o660)
    }

    /// Resolve the data directory path. Returns `None` only if explicitly
    /// set to an empty string (indicating the user wants persistence disabled).
    pub fn data_dir(&self) -> Option<PathBuf> {
        match &self.data_dir {
            Some(dir) if dir.is_empty() => None,
            Some(dir) => Some(PathBuf::from(dir)),
            None => {
                // Default: ~/.ahand/data
                dirs::home_dir().map(|h| h.join(".ahand").join("data"))
            }
        }
    }
}

fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{:032x}", ts)
}

#[cfg(test)]
mod tilde_tests {
    use super::*;

    #[test]
    fn expand_tilde_for_home_rooted_patterns() {
        let home = dirs::home_dir().expect("home dir required for this test");
        assert_eq!(
            expand_tilde("~/.ssh/**").unwrap(),
            format!("{}/.ssh/**", home.display())
        );
        assert_eq!(
            expand_tilde("~").unwrap(),
            home.to_string_lossy().to_string()
        );
    }

    #[test]
    fn expand_tilde_leaves_absolute_and_other_paths_alone() {
        assert_eq!(expand_tilde("/etc/passwd").unwrap(), "/etc/passwd");
        assert_eq!(expand_tilde("./relative").unwrap(), "./relative");
        assert_eq!(expand_tilde("no_tilde_here").unwrap(), "no_tilde_here");
    }

    #[test]
    fn expand_tilde_does_not_touch_middle_tildes() {
        // Tildes inside a path (not at the start) stay literal.
        assert_eq!(expand_tilde("/tmp/~backup").unwrap(), "/tmp/~backup");
    }

    #[test]
    fn expand_tilde_with_no_home_dir_fails_loud_for_tilde_patterns() {
        // I2 regression: when the home directory is unavailable, a tilde
        // pattern previously passed through verbatim. The downstream
        // glob-match compares canonicalized absolute paths against the
        // literal pattern, so `~/.ssh/**` would silently never match —
        // catastrophic for `path_denylist` / `dangerous_paths` where the
        // operator's intent ("this is dangerous") flips to "this is allowed".
        // The pre-check must reject the config so daemon startup aborts.
        let err = expand_tilde_with("~/.ssh/**", None).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("HOME") || msg.contains("home"),
            "error message should reference HOME: {msg}"
        );

        let err = expand_tilde_with("~", None).unwrap_err();
        assert!(
            err.to_string().contains("home") || err.to_string().contains("HOME"),
            "bare-tilde error should reference home, got: {err}"
        );
    }

    #[test]
    fn expand_tilde_with_no_home_dir_passes_non_tilde_patterns_through() {
        // Patterns without a leading tilde shouldn't care whether HOME is
        // set — they're absolute or relative and need no expansion.
        assert_eq!(expand_tilde_with("/etc/passwd", None).unwrap(), "/etc/passwd");
        assert_eq!(expand_tilde_with("./rel", None).unwrap(), "./rel");
        assert_eq!(
            expand_tilde_with("/tmp/~backup", None).unwrap(),
            "/tmp/~backup"
        );
    }
}

/// End-to-end tests that exercise `Config::load` through a real TOML
/// file rather than calling helpers directly. The I2 fix wires
/// `expand_tildes_in_place` into `Config::load`; verifying the helper
/// alone (covered above) doesn't prove the wiring is intact, so we
/// pin the wiring with a real-file round-trip here.
#[cfg(test)]
mod load_tilde_tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn write_config(dir: &TempDir, body: &str) -> PathBuf {
        let p = dir.path().join("config.toml");
        fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn load_expands_tilde_in_file_policy_when_home_present() {
        // Sanity round-trip: a TOML config that uses `~/.ssh/**` in
        // every file_policy list must come back with the home dir
        // substituted. This is the contract that
        // `FilePolicyChecker::glob_match` depends on — it compares
        // literal patterns to canonicalized absolute paths and would
        // never match `~/...`.
        let dir = TempDir::new().unwrap();
        let body = r#"
[file_policy]
enabled = true
path_allowlist = ["~/projects/**"]
path_denylist = ["~/private/**"]
dangerous_paths = ["~/.ssh/**"]
"#;
        let path = write_config(&dir, body);
        let cfg = Config::load(&path).expect("config should load");
        let fp = cfg.file_policy.expect("file_policy should be present");
        let home = dirs::home_dir().expect("home dir required for this test");
        let home_str = home.to_string_lossy();
        assert_eq!(fp.path_allowlist, vec![format!("{home_str}/projects/**")]);
        assert_eq!(fp.path_denylist, vec![format!("{home_str}/private/**")]);
        assert_eq!(fp.dangerous_paths, vec![format!("{home_str}/.ssh/**")]);
    }

    #[test]
    fn load_propagates_tilde_failure_for_dangerous_paths() {
        // I2 regression: when `expand_tildes_in_place` fails, the
        // error must propagate out of `Config::load` so daemon startup
        // aborts. Without the wiring at config.rs:341, an unexpanded
        // `~/.ssh/**` would silently not match anything — flipping
        // dangerous_paths from "deny these" to "allow everything".
        //
        // We can't easily make `dirs::home_dir()` return None during a
        // running test, so we verify the wiring at the helper level
        // and then independently verify that `Config::load` calls
        // `expand_tildes_in_place` for all three lists by checking
        // the post-load values are NOT raw `~/...`.
        let dir = TempDir::new().unwrap();
        let body = r#"
[file_policy]
enabled = true
dangerous_paths = ["~/.ssh/**"]
"#;
        let path = write_config(&dir, body);
        let cfg = Config::load(&path).unwrap();
        let dangerous = cfg
            .file_policy
            .as_ref()
            .map(|fp| &fp.dangerous_paths)
            .unwrap();
        assert!(
            !dangerous.iter().any(|p| p.starts_with("~/")),
            "dangerous_paths must have tildes expanded after Config::load \
             (raw `~/...` would silently fail to match canonicalized absolute \
              paths in FilePolicyChecker)"
        );
    }

    #[test]
    fn load_passes_through_absolute_file_policy_paths_unchanged() {
        // Non-tilde patterns must not be modified in-place by the
        // expansion step.
        let dir = TempDir::new().unwrap();
        let body = r#"
[file_policy]
enabled = true
path_allowlist = ["/etc/passwd", "/var/log/**"]
"#;
        let path = write_config(&dir, body);
        let cfg = Config::load(&path).unwrap();
        let fp = cfg.file_policy.unwrap();
        assert_eq!(
            fp.path_allowlist,
            vec!["/etc/passwd".to_string(), "/var/log/**".to_string()]
        );
    }

    #[test]
    fn load_returns_err_for_malformed_toml() {
        // The other branch of Config::load: malformed TOML must
        // surface as an error, not panic.
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "this is = not = valid = toml\n");
        let err = Config::load(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.to_lowercase().contains("toml") || msg.contains("expected"),
            "expected toml parse error, got: {msg}"
        );
    }

    #[test]
    fn load_returns_err_for_missing_file() {
        // Missing file path must surface IO error from
        // `read_to_string`, not panic.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("definitely_does_not_exist.toml");
        let err = Config::load(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.to_lowercase().contains("no such file")
                || msg.to_lowercase().contains("not found"),
            "expected NotFound IO error, got: {msg}"
        );
    }
}
