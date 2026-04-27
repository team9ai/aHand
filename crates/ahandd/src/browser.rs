use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::config::BrowserConfig;

/// Result of executing a browser command via playwright-cli.
pub struct BrowserCommandResult {
    pub success: bool,
    pub result_json: String,
    pub error: String,
    pub binary_data: Vec<u8>,
    pub binary_mime: String,
}

impl Default for BrowserCommandResult {
    fn default() -> Self {
        Self {
            success: false,
            result_json: String::new(),
            error: String::new(),
            binary_data: Vec::new(),
            binary_mime: String::new(),
        }
    }
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

    /// Log warnings for missing prerequisites at startup.
    fn check_prerequisites(&self) {
        let bin = self.binary_path();
        if !bin.exists() {
            warn!(
                path = %bin.display(),
                "playwright-cli not found — run: ahandd browser-init"
            );
        } else {
            info!(path = %bin.display(), "playwright-cli found");
        }

        if let Some(exe) = self.resolve_executable_path() {
            info!(path = %exe, "system browser detected");
        } else {
            warn!(
                "no system browser (Chrome/Edge) detected — please install one for browser automation"
            );
        }

        if self.config.headed.unwrap_or(false) {
            info!("browser headed mode enabled (visible window)");
        }
    }

    /// Execute a browser command via playwright-cli.
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
                    error: format!("max browser sessions ({}) reached", max),
                    ..Default::default()
                });
            }
            sessions.insert(session_id.to_string());
        }

        // Determine output file path for actions that produce files.
        let output_file = if matches!(action, "screenshot" | "pdf" | "snapshot") {
            self.ensure_downloads_dir(session_id).await.ok();
            let ext = match action {
                "screenshot" => "png",
                "pdf" => "pdf",
                "snapshot" => "yaml",
                _ => "bin",
            };
            Some(self.default_output_path(session_id, action, ext))
        } else {
            None
        };

        let args = self.build_cli_args(session_id, action, params_json, output_file.as_deref());
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
                    error: format!("failed to spawn playwright-cli: {}", e),
                    ..Default::default()
                });
            }
        };

        let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => {
                return Ok(BrowserCommandResult {
                    success: false,
                    error: format!("playwright-cli process error: {}", e),
                    ..Default::default()
                });
            }
            Err(_) => {
                return Ok(BrowserCommandResult {
                    success: false,
                    error: "browser command timed out".to_string(),
                    ..Default::default()
                });
            }
        };

        self.parse_output(&output, action, output_file.as_deref())
            .await
    }

    /// Execute a single CLI command (used internally by download/wait polling).
    async fn execute_single(
        &self,
        session_id: &str,
        action: &str,
        params_json: &str,
        timeout_ms: u64,
    ) -> anyhow::Result<BrowserCommandResult> {
        let args = self.build_cli_args(session_id, action, params_json, None);
        let envs = self.build_env_vars();
        let timeout = Duration::from_millis(if timeout_ms > 0 {
            timeout_ms
        } else {
            self.config.default_timeout_ms.unwrap_or(30_000)
        });

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
                    error: format!("failed to spawn playwright-cli: {}", e),
                    ..Default::default()
                });
            }
        };

        let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => {
                return Ok(BrowserCommandResult {
                    success: false,
                    error: format!("playwright-cli process error: {}", e),
                    ..Default::default()
                });
            }
            Err(_) => {
                return Ok(BrowserCommandResult {
                    success: false,
                    error: "browser command timed out".to_string(),
                    ..Default::default()
                });
            }
        };

        self.parse_output(&output, action, None).await
    }

    /// Execute a download by clicking a ref and polling the downloads directory.
    pub async fn execute_download(
        &self,
        session_id: &str,
        ref_selector: &str,
        timeout_ms: u64,
    ) -> anyhow::Result<BrowserCommandResult> {
        self.ensure_downloads_dir(session_id).await.ok();
        let download_dir = self.downloads_dir(session_id);

        // 1. Snapshot directory before click.
        let before = list_files(&download_dir).await;

        // 2. Click the download trigger element.
        let click_params = serde_json::json!({ "ref": ref_selector });
        let click_result = self
            .execute_single(session_id, "click", &click_params.to_string(), timeout_ms)
            .await?;
        if !click_result.success {
            return Ok(click_result);
        }

        // 3. Poll for new completed file.
        let effective_timeout = if timeout_ms > 0 {
            timeout_ms
        } else {
            self.config.default_timeout_ms.unwrap_or(30_000)
        };
        let deadline = Instant::now() + Duration::from_millis(effective_timeout);

        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if Instant::now() > deadline {
                return Ok(BrowserCommandResult {
                    success: false,
                    error: format!("download timed out after {}ms", effective_timeout),
                    ..Default::default()
                });
            }

            let after = list_files(&download_dir).await;
            for file in &after {
                if !before.contains(file) && is_download_complete(file) {
                    // Found a new completed file.
                    let path_str = file.to_string_lossy().to_string();
                    let binary_data = tokio::fs::read(file).await.unwrap_or_default();
                    let binary_mime = mime_from_extension(&path_str).to_string();
                    return Ok(BrowserCommandResult {
                        success: true,
                        result_json: format!("Downloaded: {}", path_str),
                        binary_data,
                        binary_mime,
                        ..Default::default()
                    });
                }
            }
        }
    }

    /// Wait for text to appear on the page by polling with eval.
    pub async fn execute_wait_for_text(
        &self,
        session_id: &str,
        text: &str,
        timeout_ms: u64,
    ) -> anyhow::Result<BrowserCommandResult> {
        let escaped = text.replace('\\', "\\\\").replace('\'', "\\'");
        let js_expr = format!("() => document.body.innerText.includes('{}')", escaped);
        let params = serde_json::json!({ "expression": js_expr });
        let params_str = params.to_string();

        let effective_timeout = if timeout_ms > 0 {
            timeout_ms
        } else {
            self.config.default_timeout_ms.unwrap_or(30_000)
        };
        let deadline = Instant::now() + Duration::from_millis(effective_timeout);
        let poll_interval = Duration::from_millis(500);

        loop {
            let result = self
                .execute_single(session_id, "eval", &params_str, 10_000)
                .await?;

            if result.success && result.result_json.trim() == "true" {
                return Ok(BrowserCommandResult {
                    success: true,
                    result_json: format!("Text '{}' found on page", text),
                    ..Default::default()
                });
            }

            if Instant::now() + poll_interval > deadline {
                return Ok(BrowserCommandResult {
                    success: false,
                    error: format!(
                        "Timeout: text '{}' not found within {}ms",
                        text, effective_timeout
                    ),
                    ..Default::default()
                });
            }

            tokio::time::sleep(poll_interval).await;
        }
    }

    /// Check whether a domain is allowed for navigation actions.
    pub fn check_domain(&self, action: &str, params_json: &str) -> Result<(), String> {
        // Only check for navigation actions.
        if action != "goto" && action != "open" {
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
            None => {
                // Prefer the aHand-managed Node.js installation
                let ahand_path = dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("/tmp"))
                    .join(".ahand")
                    .join("node")
                    .join("bin")
                    .join("playwright-cli");
                if ahand_path.exists() {
                    ahand_path
                } else {
                    PathBuf::from("playwright-cli") // fallback to PATH
                }
            }
        }
    }

    fn build_cli_args(
        &self,
        session_id: &str,
        action: &str,
        params_json: &str,
        output_file: Option<&Path>,
    ) -> Vec<String> {
        let mut args = vec![format!("-s={}", session_id), action.to_string()];

        // Parse params_json and convert to CLI positional/flag arguments.
        if let Ok(params) = serde_json::from_str::<serde_json::Value>(params_json) {
            if let Some(obj) = params.as_object() {
                args.extend(params_to_cli_args(action, obj));
            }
        }

        // Inject --filename for actions that produce file output.
        if let Some(path) = output_file {
            args.push(format!("--filename={}", path.to_string_lossy()));
        }

        args
    }

    fn build_env_vars(&self) -> Vec<(String, String)> {
        let mut envs = Vec::new();

        // Prepend our locally-installed Node.js to PATH so playwright-cli
        // can find dependencies.
        if let Some(home) = dirs::home_dir() {
            let node_bin_dir = home.join(".ahand").join("node").join("bin");
            if node_bin_dir.is_dir() {
                let system_path = std::env::var("PATH").unwrap_or_default();
                envs.push((
                    "PATH".into(),
                    format!("{}:{}", node_bin_dir.to_string_lossy(), system_path),
                ));
            }
        }

        // System Chrome detection — set PLAYWRIGHT_MCP_EXECUTABLE_PATH
        // before PLAYWRIGHT_BROWSERS_PATH so we can skip the latter
        // when a system browser is found.
        let resolved_exe = self.resolve_executable_path();
        if let Some(exe) = &resolved_exe {
            envs.push(("PLAYWRIGHT_MCP_EXECUTABLE_PATH".into(), exe.clone()));
        }

        if let Some(path) = &self.config.browsers_path {
            envs.push(("PLAYWRIGHT_BROWSERS_PATH".into(), path.clone()));
        }

        // Headed mode: tell playwright-cli to show the browser window.
        if self.config.headed.unwrap_or(false) {
            envs.push(("PLAYWRIGHT_MCP_HEADLESS".into(), "false".into()));
        }

        envs
    }

    /// Resolve browser executable: config > system Chrome/Edge auto-detect.
    fn resolve_executable_path(&self) -> Option<String> {
        crate::browser_setup::detect_browser(self.config.executable_path.as_deref())
            .map(|b| b.path.to_string_lossy().into_owned())
    }

    async fn parse_output(
        &self,
        output: &std::process::Output,
        action: &str,
        output_file: Option<&Path>,
    ) -> anyhow::Result<BrowserCommandResult> {
        let success = output.status.success();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        // For screenshot/pdf, read binary file from the --filename path.
        let (binary_data, binary_mime) = if matches!(action, "screenshot" | "pdf") && success {
            if let Some(path) = output_file {
                self.read_file_at_path(path).await
            } else {
                (Vec::new(), String::new())
            }
        } else {
            (Vec::new(), String::new())
        };

        Ok(BrowserCommandResult {
            success,
            result_json: if success { stdout } else { String::new() },
            error: if success { String::new() } else { stderr },
            binary_data,
            binary_mime,
        })
    }

    /// Read a file produced by playwright-cli and detect MIME type.
    async fn read_file_at_path(&self, path: &Path) -> (Vec<u8>, String) {
        match tokio::fs::read(path).await {
            Ok(bytes) => {
                let path_str = path.to_string_lossy();
                let mime = mime_from_extension(&path_str);
                info!(path = %path_str, mime, bytes = bytes.len(), "read file data");
                (bytes, mime.to_string())
            }
            Err(e) => {
                warn!(path = %path.display(), error = %e, "failed to read file");
                (Vec::new(), String::new())
            }
        }
    }
}

/// Convert params_json object fields into playwright-cli positional/flag arguments.
fn params_to_cli_args(
    action: &str,
    params: &serde_json::Map<String, serde_json::Value>,
) -> Vec<String> {
    let mut args = Vec::new();

    match action {
        "goto" => {
            if let Some(url) = params.get("url").and_then(|v| v.as_str()) {
                args.push(url.to_string());
            }
        }
        "open" => {
            if let Some(url) = params.get("url").and_then(|v| v.as_str()) {
                args.push(url.to_string());
            }
        }
        "click" | "hover" => {
            if let Some(sel) = params
                .get("ref")
                .or(params.get("selector"))
                .and_then(|v| v.as_str())
            {
                args.push(sel.to_string());
            }
        }
        "fill" => {
            if let Some(sel) = params
                .get("ref")
                .or(params.get("selector"))
                .and_then(|v| v.as_str())
            {
                args.push(sel.to_string());
            }
            if let Some(val) = params
                .get("text")
                .or(params.get("value"))
                .and_then(|v| v.as_str())
            {
                args.push(val.to_string());
            }
        }
        "type" => {
            // playwright-cli `type` has no ref param — types into focused element.
            if let Some(val) = params
                .get("text")
                .or(params.get("value"))
                .and_then(|v| v.as_str())
            {
                args.push(val.to_string());
            }
        }
        "select" => {
            if let Some(sel) = params
                .get("ref")
                .or(params.get("selector"))
                .and_then(|v| v.as_str())
            {
                args.push(sel.to_string());
            }
            if let Some(val) = params.get("value").and_then(|v| v.as_str()) {
                args.push(val.to_string());
            } else if let Some(vals) = params.get("values").and_then(|v| v.as_array()) {
                for val in vals {
                    if let Some(s) = val.as_str() {
                        args.push(s.to_string());
                    }
                }
            }
        }
        "screenshot" => {
            // screenshot [ref] [--filename=<path>]
            if let Some(sel) = params.get("ref").and_then(|v| v.as_str()) {
                args.push(sel.to_string());
            }
        }
        "pdf" => {
            // pdf [--filename=<path>] — filename injected by build_cli_args
        }
        "snapshot" => {
            // snapshot [--filename=<path>] — filename injected by build_cli_args
        }
        "press" => {
            if let Some(key) = params.get("key").and_then(|v| v.as_str()) {
                args.push(key.to_string());
            }
        }
        "eval" => {
            if let Some(expr) = params.get("expression").and_then(|v| v.as_str()) {
                args.push(expr.to_string());
            }
            if let Some(sel) = params.get("ref").and_then(|v| v.as_str()) {
                args.push(sel.to_string());
            }
        }
        "drag" => {
            if let Some(start) = params.get("startRef").and_then(|v| v.as_str()) {
                args.push(start.to_string());
            }
            if let Some(end) = params.get("endRef").and_then(|v| v.as_str()) {
                args.push(end.to_string());
            }
        }
        "resize" => {
            if let Some(w) = params.get("width").and_then(|v| v.as_i64()) {
                args.push(w.to_string());
            }
            if let Some(h) = params.get("height").and_then(|v| v.as_i64()) {
                args.push(h.to_string());
            }
        }
        "upload" => {
            if let Some(file) = params.get("file").and_then(|v| v.as_str()) {
                args.push(file.to_string());
            } else if let Some(paths) = params.get("paths").and_then(|v| v.as_array()) {
                for p in paths {
                    if let Some(s) = p.as_str() {
                        args.push(s.to_string());
                    }
                }
            }
        }
        "dialog-accept" => {
            if let Some(text) = params.get("promptText").and_then(|v| v.as_str()) {
                args.push(text.to_string());
            }
        }
        "dialog-dismiss" => {
            // No additional args needed.
        }
        "tab-new" => {
            if let Some(url) = params.get("url").and_then(|v| v.as_str()) {
                args.push(url.to_string());
            }
        }
        "tab-close" => {
            if let Some(index) = params.get("index").and_then(|v| v.as_i64()) {
                args.push(index.to_string());
            }
        }
        "tab-select" => {
            if let Some(index) = params.get("index").and_then(|v| v.as_i64()) {
                args.push(index.to_string());
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

/// List files in a directory (non-recursive).
async fn list_files(dir: &Path) -> HashSet<PathBuf> {
    let mut files = HashSet::new();
    if let Ok(mut entries) = tokio::fs::read_dir(dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.is_file() {
                files.insert(path);
            }
        }
    }
    files
}

/// Check if a downloaded file is complete (not a temp/partial file).
fn is_download_complete(path: &Path) -> bool {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    !name.ends_with(".crdownload")
        && !name.ends_with(".part")
        && !name.ends_with(".tmp")
        && !name.ends_with(".download")
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
    fn test_params_to_cli_args_goto() {
        let params: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"url":"https://example.com"}"#).unwrap();
        let args = params_to_cli_args("goto", &params);
        assert_eq!(args, vec!["https://example.com"]);
    }

    #[test]
    fn test_params_to_cli_args_fill() {
        let params: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"ref":"@e3","text":"hello world"}"#).unwrap();
        let args = params_to_cli_args("fill", &params);
        assert_eq!(args, vec!["@e3", "hello world"]);
    }

    #[test]
    fn test_params_to_cli_args_click() {
        let params: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"ref":"@e5"}"#).unwrap();
        let args = params_to_cli_args("click", &params);
        assert_eq!(args, vec!["@e5"]);
    }

    #[test]
    fn test_mime_from_extension() {
        assert_eq!(mime_from_extension("/tmp/shot.png"), "image/png");
        assert_eq!(mime_from_extension("/tmp/doc.PDF"), "application/pdf");
        assert_eq!(mime_from_extension("/tmp/data.csv"), "text/csv");
        assert_eq!(
            mime_from_extension("/tmp/report.xlsx"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        );
        assert_eq!(
            mime_from_extension("/tmp/unknown.xyz"),
            "application/octet-stream"
        );
    }

    #[test]
    fn test_is_download_complete() {
        assert!(is_download_complete(Path::new("/tmp/file.pdf")));
        assert!(!is_download_complete(Path::new("/tmp/file.crdownload")));
        assert!(!is_download_complete(Path::new("/tmp/file.part")));
        assert!(!is_download_complete(Path::new("/tmp/file.tmp")));
    }
}
