use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::config::BrowserConfig;

/// Result of executing a browser command via agent-browser CLI.
pub struct BrowserCommandResult {
    pub success: bool,
    pub result_json: String,
    pub error: String,
    pub binary_data: Vec<u8>,
    pub binary_mime: String,
}

/// Raw JSON response from `agent-browser --json`.
#[derive(Deserialize)]
struct CliResponse {
    success: bool,
    data: Option<serde_json::Value>,
    error: Option<String>,
}

pub struct BrowserManager {
    config: BrowserConfig,
    active_sessions: Mutex<HashSet<String>>,
}

impl BrowserManager {
    pub fn new(config: BrowserConfig) -> Self {
        let mgr = Self {
            config,
            active_sessions: Mutex::new(HashSet::new()),
        };
        if mgr.is_enabled() {
            mgr.check_prerequisites();
        }
        mgr
    }

    /// Whether browser capabilities are enabled.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled.unwrap_or(false)
    }

    /// Resolve the downloads directory (for download/pdf output files).
    fn downloads_dir(&self, session_id: &str) -> PathBuf {
        let base = match &self.config.downloads_dir {
            Some(p) => PathBuf::from(p),
            None => dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join(".ahand")
                .join("browser")
                .join("downloads"),
        };
        base.join(session_id)
    }

    /// Generate a default output path when the caller doesn't provide one.
    fn default_output_path(&self, session_id: &str, action: &str, ext: &str) -> PathBuf {
        let dir = self.downloads_dir(session_id);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        dir.join(format!("{}_{}.{}", ts, action, ext))
    }

    /// Ensure the downloads directory exists for a session.
    async fn ensure_downloads_dir(&self, session_id: &str) -> anyhow::Result<()> {
        let dir = self.downloads_dir(session_id);
        tokio::fs::create_dir_all(&dir).await?;
        Ok(())
    }

    /// Inject a default output path into params if the caller didn't provide one.
    fn inject_default_path(&self, session_id: &str, action: &str, params_json: &str) -> String {
        let mut params: serde_json::Value =
            serde_json::from_str(params_json).unwrap_or(serde_json::Value::Object(Default::default()));

        if params.get("path").and_then(|v| v.as_str()).is_none() {
            let ext = match action {
                "pdf" => "pdf",
                _ => "bin",
            };
            let path = self.default_output_path(session_id, action, ext);
            params.as_object_mut().unwrap().insert(
                "path".to_string(),
                serde_json::Value::String(path.to_string_lossy().into_owned()),
            );
        }

        serde_json::to_string(&params).unwrap_or_else(|_| params_json.to_string())
    }

    /// Log warnings for missing prerequisites at startup.
    fn check_prerequisites(&self) {
        let bin = self.binary_path();
        if !bin.exists() {
            warn!(
                path = %bin.display(),
                "agent-browser CLI not found — run: ahandctl browser-init"
            );
        } else {
            info!(path = %bin.display(), "agent-browser CLI found");
        }

        let home = self.daemon_home();
        let daemon = home.join("dist").join("daemon.js");
        if !daemon.exists() {
            warn!(
                path = %daemon.display(),
                "daemon.js not found — run: ahandctl browser-init"
            );
        }

        if let Some(exe) = self.resolve_executable_path() {
            info!(path = %exe, "system browser detected");
        } else {
            let browsers_dir = home.join("browsers");
            if !browsers_dir.exists() || browsers_dir.read_dir().map(|mut d| d.next().is_none()).unwrap_or(true) {
                warn!("no system browser found and no Chromium installed — run: ahandctl browser-init");
            }
        }

        if self.config.headed.unwrap_or(false) {
            info!("browser headed mode enabled (visible window)");
        }
    }

    /// Resolve AGENT_BROWSER_HOME directory.
    fn daemon_home(&self) -> PathBuf {
        match &self.config.home_dir {
            Some(p) => PathBuf::from(p),
            None => dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join(".ahand")
                .join("browser"),
        }
    }

    /// Execute a browser command via agent-browser CLI.
    pub async fn execute(
        &self,
        session_id: &str,
        action: &str,
        params_json: &str,
        timeout_ms: u64,
    ) -> anyhow::Result<BrowserCommandResult> {
        // Check session limit.
        {
            let mut sessions = self.active_sessions.lock().await;
            let max = self.config.max_sessions.unwrap_or(4);
            if !sessions.contains(session_id) && sessions.len() >= max {
                return Ok(BrowserCommandResult {
                    success: false,
                    result_json: String::new(),
                    error: format!("max browser sessions ({}) reached", max),
                    binary_data: Vec::new(),
                    binary_mime: String::new(),
                });
            }
            sessions.insert(session_id.to_string());
        }

        // For download/pdf, ensure output directory and inject default path if needed.
        let params_json = if matches!(action, "download" | "pdf") {
            self.ensure_downloads_dir(session_id).await.ok();
            self.inject_default_path(session_id, action, params_json)
        } else {
            params_json.to_string()
        };

        let args = self.build_cli_args(session_id, action, &params_json);
        let envs = self.build_env_vars();

        let timeout = if timeout_ms > 0 {
            Duration::from_millis(timeout_ms)
        } else {
            Duration::from_millis(self.config.default_timeout_ms.unwrap_or(30_000))
        };

        info!(
            session_id,
            action,
            binary = %self.binary_path().display(),
            "executing browser command"
        );

        let child = tokio::process::Command::new(self.binary_path())
            .args(&args)
            .envs(envs)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();

        let child = match child {
            Ok(c) => c,
            Err(e) => {
                return Ok(BrowserCommandResult {
                    success: false,
                    result_json: String::new(),
                    error: format!("failed to spawn agent-browser: {}", e),
                    binary_data: Vec::new(),
                    binary_mime: String::new(),
                });
            }
        };

        let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => {
                return Ok(BrowserCommandResult {
                    success: false,
                    result_json: String::new(),
                    error: format!("agent-browser process error: {}", e),
                    binary_data: Vec::new(),
                    binary_mime: String::new(),
                });
            }
            Err(_) => {
                return Ok(BrowserCommandResult {
                    success: false,
                    result_json: String::new(),
                    error: "browser command timed out".to_string(),
                    binary_data: Vec::new(),
                    binary_mime: String::new(),
                });
            }
        };

        self.parse_output(&output, action).await
    }

    /// Check whether a domain is allowed for navigation actions.
    pub fn check_domain(&self, action: &str, params_json: &str) -> Result<(), String> {
        // Only check for navigation actions.
        if action != "open" && action != "navigate" {
            return Ok(());
        }

        let url = match serde_json::from_str::<serde_json::Value>(params_json) {
            Ok(v) => v
                .get("url")
                .and_then(|u| u.as_str())
                .unwrap_or("")
                .to_string(),
            Err(_) => return Ok(()),
        };

        if url.is_empty() {
            return Ok(());
        }

        // Extract domain from URL.
        let domain = extract_domain(&url);
        if domain.is_empty() {
            return Ok(());
        }

        // Check denied domains first.
        for denied in &self.config.denied_domains {
            if domain_matches(&domain, denied) {
                return Err(format!("domain '{}' is denied", domain));
            }
        }

        // If allowed_domains is non-empty, domain must be in the list.
        if !self.config.allowed_domains.is_empty() {
            let allowed = self
                .config
                .allowed_domains
                .iter()
                .any(|a| domain_matches(&domain, a));
            if !allowed {
                return Err(format!("domain '{}' is not in allowed list", domain));
            }
        }

        Ok(())
    }

    /// Remove a session from tracking (e.g. after "close" command).
    pub async fn release_session(&self, session_id: &str) {
        self.active_sessions.lock().await.remove(session_id);
    }

    fn binary_path(&self) -> PathBuf {
        match &self.config.binary_path {
            Some(p) => PathBuf::from(p),
            None => dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join(".ahand")
                .join("bin")
                .join("agent-browser"),
        }
    }

    fn build_cli_args(&self, session_id: &str, action: &str, params_json: &str) -> Vec<String> {
        let mut args = vec![
            "--json".to_string(),
            "--session".to_string(),
            session_id.to_string(),
            action.to_string(),
        ];

        // Parse params_json and convert to CLI positional/flag arguments.
        if let Ok(params) = serde_json::from_str::<serde_json::Value>(params_json) {
            if let Some(obj) = params.as_object() {
                args.extend(params_to_cli_args(action, obj));
            }
        }

        args
    }

    fn build_env_vars(&self) -> Vec<(String, String)> {
        let mut envs = Vec::new();

        if let Some(dir) = &self.config.socket_dir {
            envs.push(("AGENT_BROWSER_SOCKET_DIR".into(), dir.clone()));
        } else {
            // Default socket dir.
            if let Some(home) = dirs::home_dir() {
                let dir = home.join(".ahand").join("browser").join("sockets");
                envs.push((
                    "AGENT_BROWSER_SOCKET_DIR".into(),
                    dir.to_string_lossy().into_owned(),
                ));
            }
        }

        if let Some(home) = &self.config.home_dir {
            envs.push(("AGENT_BROWSER_HOME".into(), home.clone()));
        } else {
            if let Some(home) = dirs::home_dir() {
                let dir = home.join(".ahand").join("browser");
                envs.push((
                    "AGENT_BROWSER_HOME".into(),
                    dir.to_string_lossy().into_owned(),
                ));
            }
        }

        // System Chrome detection — set before PLAYWRIGHT_BROWSERS_PATH so we
        // can skip the latter when a system browser is found.
        let resolved_exe = self.resolve_executable_path();
        if let Some(exe) = &resolved_exe {
            envs.push(("AGENT_BROWSER_EXECUTABLE_PATH".into(), exe.clone()));
        }

        if let Some(path) = &self.config.browsers_path {
            envs.push(("PLAYWRIGHT_BROWSERS_PATH".into(), path.clone()));
        } else if resolved_exe.is_none() {
            // Only set PLAYWRIGHT_BROWSERS_PATH when no system browser was found
            // (fallback to locally installed Chromium).
            if let Some(home) = dirs::home_dir() {
                let dir = home.join(".ahand").join("browser").join("browsers");
                envs.push((
                    "PLAYWRIGHT_BROWSERS_PATH".into(),
                    dir.to_string_lossy().into_owned(),
                ));
            }
        }

        if self.config.headed.unwrap_or(false) {
            envs.push(("AGENT_BROWSER_HEADED".into(), "1".into()));
        }

        envs
    }

    /// Resolve browser executable: config > system Chrome auto-detect.
    fn resolve_executable_path(&self) -> Option<String> {
        if let Some(path) = &self.config.executable_path {
            return Some(path.clone());
        }

        #[cfg(target_os = "macos")]
        {
            for candidate in &[
                "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
                "/Applications/Google Chrome Dev.app/Contents/MacOS/Google Chrome Dev",
                "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
                "/Applications/Chromium.app/Contents/MacOS/Chromium",
            ] {
                if std::path::Path::new(candidate).exists() {
                    return Some(candidate.to_string());
                }
            }
        }

        #[cfg(target_os = "linux")]
        {
            for candidate in &["/usr/bin/google-chrome", "/usr/bin/google-chrome-stable"] {
                if std::path::Path::new(candidate).exists() {
                    return Some(candidate.to_string());
                }
            }
        }

        None
    }

    async fn parse_output(
        &self,
        output: &std::process::Output,
        action: &str,
    ) -> anyhow::Result<BrowserCommandResult> {
        let stdout = String::from_utf8_lossy(&output.stdout);

        // agent-browser --json outputs one JSON line to stdout.
        let resp: CliResponse = match serde_json::from_str(stdout.trim()) {
            Ok(r) => r,
            Err(e) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!(
                    exit_code = output.status.code(),
                    stdout = %stdout,
                    stderr = %stderr,
                    "failed to parse agent-browser output"
                );
                return Ok(BrowserCommandResult {
                    success: false,
                    result_json: String::new(),
                    error: format!("failed to parse CLI output: {}", e),
                    binary_data: Vec::new(),
                    binary_mime: String::new(),
                });
            }
        };

        let result_json = resp
            .data
            .as_ref()
            .map(|d| serde_json::to_string(d).unwrap_or_default())
            .unwrap_or_default();

        let error = resp.error.unwrap_or_default();

        // For commands that produce files, read binary data from the path in the response.
        let (binary_data, binary_mime) =
            if matches!(action, "screenshot" | "download" | "pdf") && resp.success {
                self.read_file_data(&resp.data).await
            } else {
                (Vec::new(), String::new())
            };

        Ok(BrowserCommandResult {
            success: resp.success,
            result_json,
            error,
            binary_data,
            binary_mime,
        })
    }

    /// Read a file produced by agent-browser (screenshot, download, pdf) and detect MIME type.
    async fn read_file_data(
        &self,
        data: &Option<serde_json::Value>,
    ) -> (Vec<u8>, String) {
        let path = data
            .as_ref()
            .and_then(|d| d.get("path"))
            .and_then(|p| p.as_str());

        let Some(path) = path else {
            return (Vec::new(), String::new());
        };

        match tokio::fs::read(path).await {
            Ok(bytes) => {
                let mime = mime_from_extension(path);
                info!(path, mime, bytes = bytes.len(), "read file data");
                (bytes, mime.to_string())
            }
            Err(e) => {
                warn!(path, error = %e, "failed to read file");
                (Vec::new(), String::new())
            }
        }
    }
}

/// Convert params_json object fields into CLI positional/flag arguments.
fn params_to_cli_args(
    action: &str,
    params: &serde_json::Map<String, serde_json::Value>,
) -> Vec<String> {
    let mut args = Vec::new();

    match action {
        "open" | "navigate" => {
            if let Some(url) = params.get("url").and_then(|v| v.as_str()) {
                args.push(url.to_string());
            }
        }
        "click" | "hover" | "focus" => {
            if let Some(sel) = params.get("selector").and_then(|v| v.as_str()) {
                args.push(sel.to_string());
            }
        }
        "fill" | "type" => {
            if let Some(sel) = params.get("selector").and_then(|v| v.as_str()) {
                args.push(sel.to_string());
            }
            if let Some(val) = params.get("value").and_then(|v| v.as_str()) {
                args.push(val.to_string());
            }
        }
        "select" => {
            if let Some(sel) = params.get("selector").and_then(|v| v.as_str()) {
                args.push(sel.to_string());
            }
            if let Some(vals) = params.get("values").and_then(|v| v.as_array()) {
                for val in vals {
                    if let Some(s) = val.as_str() {
                        args.push(s.to_string());
                    }
                }
            }
        }
        "screenshot" => {
            if let Some(path) = params.get("path").and_then(|v| v.as_str()) {
                args.push(path.to_string());
            }
            if params.get("fullPage").and_then(|v| v.as_bool()) == Some(true) {
                args.push("--full-page".to_string());
            }
        }
        "download" => {
            // download <selector> [path]
            if let Some(sel) = params.get("selector").and_then(|v| v.as_str()) {
                args.push(sel.to_string());
            }
            if let Some(path) = params.get("path").and_then(|v| v.as_str()) {
                args.push(path.to_string());
            }
        }
        "pdf" => {
            // pdf [path] [--full-page]
            if let Some(path) = params.get("path").and_then(|v| v.as_str()) {
                args.push(path.to_string());
            }
            if params.get("fullPage").and_then(|v| v.as_bool()) == Some(true) {
                args.push("--full-page".to_string());
            }
        }
        "snapshot" => {
            if params.get("compact").and_then(|v| v.as_bool()) == Some(true) {
                args.push("--compact".to_string());
            }
            if let Some(depth) = params.get("maxDepth").and_then(|v| v.as_i64()) {
                args.push("--depth".to_string());
                args.push(depth.to_string());
            }
            if let Some(sel) = params.get("selector").and_then(|v| v.as_str()) {
                args.push("--selector".to_string());
                args.push(sel.to_string());
            }
        }
        "scroll" => {
            if let Some(sel) = params.get("selector").and_then(|v| v.as_str()) {
                args.push(sel.to_string());
            }
            if let Some(dir) = params.get("direction").and_then(|v| v.as_str()) {
                args.push(dir.to_string());
            }
        }
        "press" => {
            if let Some(key) = params.get("key").and_then(|v| v.as_str()) {
                args.push(key.to_string());
            }
        }
        "wait" => {
            if let Some(text) = params.get("text").and_then(|v| v.as_str()) {
                args.push(text.to_string());
            }
            if let Some(ms) = params.get("timeout").and_then(|v| v.as_i64()) {
                args.push("--timeout".to_string());
                args.push(ms.to_string());
            }
        }
        "evaluate" => {
            if let Some(expr) = params.get("expression").and_then(|v| v.as_str()) {
                args.push(expr.to_string());
            }
        }
        _ => {
            // For unknown actions, pass all string values as positional args.
            for (_key, value) in params {
                if let Some(s) = value.as_str() {
                    args.push(s.to_string());
                }
            }
        }
    }

    args
}

/// Detect MIME type from file extension.
fn mime_from_extension(path: &str) -> &'static str {
    let lower = path.to_lowercase();
    if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".svg") {
        "image/svg+xml"
    } else if lower.ends_with(".pdf") {
        "application/pdf"
    } else if lower.ends_with(".json") {
        "application/json"
    } else if lower.ends_with(".csv") {
        "text/csv"
    } else if lower.ends_with(".txt") || lower.ends_with(".log") {
        "text/plain"
    } else if lower.ends_with(".html") || lower.ends_with(".htm") {
        "text/html"
    } else if lower.ends_with(".xml") {
        "application/xml"
    } else if lower.ends_with(".zip") {
        "application/zip"
    } else if lower.ends_with(".xlsx") {
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
    } else if lower.ends_with(".docx") {
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
    } else if lower.ends_with(".xls") {
        "application/vnd.ms-excel"
    } else if lower.ends_with(".doc") {
        "application/msword"
    } else {
        "application/octet-stream"
    }
}

/// Extract domain from a URL string.
fn extract_domain(url: &str) -> String {
    // Handle URLs with or without scheme.
    let after_scheme = if let Some(idx) = url.find("://") {
        &url[idx + 3..]
    } else {
        url
    };

    // Take everything before the first '/' or ':'
    after_scheme
        .split('/')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .to_lowercase()
}

/// Check if a domain matches a pattern (supports wildcard prefix like "*.example.com").
fn domain_matches(domain: &str, pattern: &str) -> bool {
    if pattern.starts_with("*.") {
        let suffix = &pattern[2..];
        domain == suffix || domain.ends_with(&format!(".{}", suffix))
    } else {
        domain == pattern
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_domain() {
        assert_eq!(extract_domain("https://example.com/path"), "example.com");
        assert_eq!(extract_domain("http://foo.bar:8080/x"), "foo.bar");
        assert_eq!(extract_domain("example.com"), "example.com");
    }

    #[test]
    fn test_domain_matches() {
        assert!(domain_matches("example.com", "example.com"));
        assert!(domain_matches("sub.example.com", "*.example.com"));
        assert!(domain_matches("example.com", "*.example.com"));
        assert!(!domain_matches("notexample.com", "*.example.com"));
        assert!(!domain_matches("example.com", "other.com"));
    }

    #[test]
    fn test_params_to_cli_args_open() {
        let params: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"url":"https://example.com"}"#).unwrap();
        let args = params_to_cli_args("open", &params);
        assert_eq!(args, vec!["https://example.com"]);
    }

    #[test]
    fn test_params_to_cli_args_fill() {
        let params: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"selector":"@e3","value":"hello world"}"#).unwrap();
        let args = params_to_cli_args("fill", &params);
        assert_eq!(args, vec!["@e3", "hello world"]);
    }

    #[test]
    fn test_params_to_cli_args_snapshot_compact() {
        let params: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"compact":true,"maxDepth":3}"#).unwrap();
        let args = params_to_cli_args("snapshot", &params);
        assert!(args.contains(&"--compact".to_string()));
        assert!(args.contains(&"--depth".to_string()));
        assert!(args.contains(&"3".to_string()));
    }

    #[test]
    fn test_params_to_cli_args_download() {
        let params: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"selector":"a.download-btn","path":"/tmp/file.zip"}"#).unwrap();
        let args = params_to_cli_args("download", &params);
        assert_eq!(args, vec!["a.download-btn", "/tmp/file.zip"]);
    }

    #[test]
    fn test_params_to_cli_args_pdf() {
        let params: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"path":"/tmp/page.pdf","fullPage":true}"#).unwrap();
        let args = params_to_cli_args("pdf", &params);
        assert_eq!(args, vec!["/tmp/page.pdf", "--full-page"]);
    }

    #[test]
    fn test_mime_from_extension() {
        assert_eq!(mime_from_extension("/tmp/shot.png"), "image/png");
        assert_eq!(mime_from_extension("/tmp/doc.PDF"), "application/pdf");
        assert_eq!(mime_from_extension("/tmp/data.csv"), "text/csv");
        assert_eq!(mime_from_extension("/tmp/report.xlsx"), "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet");
        assert_eq!(mime_from_extension("/tmp/unknown.xyz"), "application/octet-stream");
    }
}
