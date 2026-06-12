use ahand_platform::process;
use anyhow::{Context, Result};
use serde::Serialize;
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use warp::http::StatusCode;
use warp::{Filter, Rejection, Reply, reject};

// ──────────────────────────────────────────────────────────────────────
// Types
// ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct StatusResponse {
    version: String,
    daemon_running: bool,
    daemon_pid: Option<u32>,
    config_path: String,
    data_dir: String,
    data_dir_size: u64,
    home_dir: String,
    bin_dir: String,
}

#[derive(Debug, Serialize)]
struct LogEntry {
    ts_ms: u64,
    direction: String,
    device_id: String,
    msg_id: String,
    seq: u64,
    ack: u64,
    payload_type: String,
}

#[derive(Debug, Serialize)]
struct LogsResponse {
    total: usize,
    entries: Vec<LogEntry>,
}

#[derive(Debug, Serialize)]
struct RunEntry {
    job_id: String,
    created_at: u64,
}

#[derive(Debug, Serialize)]
struct RunsResponse {
    total: usize,
    runs: Vec<RunEntry>,
}

#[derive(Debug, Serialize)]
struct RunDetail {
    job_id: String,
    request: serde_json::Value,
    result: Option<serde_json::Value>,
    files: Vec<String>,
}

// ──────────────────────────────────────────────────────────────────────
// Entry point
// ──────────────────────────────────────────────────────────────────────

pub async fn serve(port: u16, config_path: Option<String>, no_open: bool) -> Result<()> {
    // Generate random token
    let token = generate_token();
    println!("Admin panel starting on http://127.0.0.1:{}", port);
    println!("Token: {}", token);
    println!();

    // Determine paths
    let config_file = resolve_config_path(config_path)?;
    let dist_path = resolve_dist_path()?;

    println!("Config: {}", config_file.display());
    println!("SPA:    {}", dist_path.display());
    println!();

    // Open browser
    if !no_open {
        let url = format!("http://127.0.0.1:{}?token={}", port, token);
        if let Err(e) = open::that(&url) {
            eprintln!("Failed to open browser: {}", e);
        }
    }

    // Build routes
    let token_arc = Arc::new(token.clone());
    let config_arc = Arc::new(config_file);

    let api = warp::path("api").and(
        status_route(token_arc.clone(), config_arc.clone())
            .or(host_resource_route(token_arc.clone()))
            .or(config_get_route(token_arc.clone(), config_arc.clone()))
            .or(config_put_route(token_arc.clone(), config_arc.clone()))
            .or(logs_route(token_arc.clone()))
            .or(runs_list_route(token_arc.clone()))
            .or(runs_get_route(token_arc.clone()))
            .or(runs_file_route(token_arc.clone()))
            .or(browser_init_route(token_arc.clone())),
    );

    // Static files fallback
    let static_files = warp::fs::dir(dist_path);

    let routes = api.or(static_files).recover(handle_rejection);

    // Run server with graceful shutdown
    let (addr, server) =
        warp::serve(routes).bind_with_graceful_shutdown(([127, 0, 0, 1], port), async {
            tokio::signal::ctrl_c()
                .await
                .expect("Failed to listen for Ctrl-C");
            println!("\nShutting down...");
        });

    println!("Server running at http://{}", addr);
    println!("Press Ctrl-C to stop");
    println!();

    // Graceful shutdown with timeout — force exit if connections linger.
    tokio::select! {
        _ = server => {}
        _ = async {
            tokio::signal::ctrl_c().await.ok();
            // Second Ctrl+C or timeout: force exit
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        } => {
            eprintln!("Force shutdown (timeout)");
            std::process::exit(0);
        }
    }

    println!("Server stopped");
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────
// Auth middleware
// ──────────────────────────────────────────────────────────────────────

#[derive(Debug)]
struct Unauthorized;
impl reject::Reject for Unauthorized {}

fn with_auth(token: Arc<String>) -> impl Filter<Extract = (), Error = Rejection> + Clone {
    warp::any()
        .and(warp::header::optional::<String>("authorization"))
        .and(warp::query::<std::collections::HashMap<String, String>>())
        .and_then(
            move |auth_header: Option<String>, query: std::collections::HashMap<String, String>| {
                let token = token.clone();
                async move {
                    // Check Authorization header
                    if let Some(header) = auth_header
                        && let Some(bearer) = header.strip_prefix("Bearer ")
                        && bearer == token.as_str()
                    {
                        return Ok::<_, Rejection>(());
                    }

                    // Check query parameter
                    if let Some(query_token) = query.get("token")
                        && query_token == token.as_str()
                    {
                        return Ok(());
                    }

                    Err(reject::custom(Unauthorized))
                }
            },
        )
        .untuple_one()
}

// ──────────────────────────────────────────────────────────────────────
// API Routes
// ──────────────────────────────────────────────────────────────────────

fn status_route(
    token: Arc<String>,
    config_path: Arc<PathBuf>,
) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
    warp::path!("status")
        .and(warp::get())
        .and(with_auth(token))
        .and_then(move || {
            let config_path = config_path.clone();
            async move {
                let response = get_status(&config_path).await;
                match response {
                    Ok(r) => Ok::<_, Rejection>(warp::reply::json(&r)),
                    Err(e) => {
                        eprintln!("Status error: {}", e);
                        Err(reject::reject())
                    }
                }
            }
        })
}

fn host_resource_route(
    token: Arc<String>,
) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
    warp::path!("host-resource")
        .and(warp::get())
        .and(with_auth(token))
        .and_then(|| async move {
            match ahandd::plugin_runtime::get_host_resource().await {
                Ok(snapshot) => Ok::<_, Rejection>(warp::reply::json(&snapshot)),
                Err(e) => {
                    eprintln!("Host resource error: {}", e);
                    Err(reject::reject())
                }
            }
        })
}

fn config_get_route(
    token: Arc<String>,
    config_path: Arc<PathBuf>,
) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
    warp::path!("config")
        .and(warp::get())
        .and(with_auth(token))
        .and_then(move || {
            let config_path = config_path.clone();
            async move {
                match get_config(&config_path).await {
                    Ok(config_json) => Ok::<_, Rejection>(warp::reply::json(&config_json)),
                    Err(e) => {
                        eprintln!("Config read error: {}", e);
                        Err(reject::reject())
                    }
                }
            }
        })
}

fn config_put_route(
    token: Arc<String>,
    config_path: Arc<PathBuf>,
) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
    warp::path!("config")
        .and(warp::put())
        .and(with_auth(token))
        .and(warp::body::json())
        .and_then(move |body: serde_json::Value| {
            let config_path = config_path.clone();
            async move {
                match put_config(&config_path, body).await {
                    Ok(_) => Ok::<_, Rejection>(warp::reply::with_status(
                        "Config updated",
                        StatusCode::OK,
                    )),
                    Err(e) => {
                        eprintln!("Config write error: {}", e);
                        Err(reject::reject())
                    }
                }
            }
        })
}

fn logs_route(token: Arc<String>) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
    warp::path!("logs")
        .and(warp::get())
        .and(with_auth(token))
        .and(warp::query::<std::collections::HashMap<String, String>>())
        .and_then(
            |query: std::collections::HashMap<String, String>| async move {
                let limit = query
                    .get("limit")
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(50);
                let offset = query
                    .get("offset")
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(0);

                match get_logs(limit, offset).await {
                    Ok(logs) => Ok::<_, Rejection>(warp::reply::json(&logs)),
                    Err(e) => {
                        eprintln!("Logs error: {}", e);
                        Err(reject::reject())
                    }
                }
            },
        )
}

fn runs_list_route(
    token: Arc<String>,
) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
    warp::path!("runs")
        .and(warp::get())
        .and(with_auth(token))
        .and(warp::query::<std::collections::HashMap<String, String>>())
        .and_then(
            |query: std::collections::HashMap<String, String>| async move {
                let limit = query
                    .get("limit")
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(20);
                let offset = query
                    .get("offset")
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(0);

                match list_runs(limit, offset).await {
                    Ok(runs) => Ok::<_, Rejection>(warp::reply::json(&runs)),
                    Err(e) => {
                        eprintln!("Runs list error: {}", e);
                        Err(reject::reject())
                    }
                }
            },
        )
}

fn runs_get_route(
    token: Arc<String>,
) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
    warp::path!("runs" / String)
        .and(warp::get())
        .and(with_auth(token))
        .and_then(|job_id: String| async move {
            match get_run_detail(&job_id).await {
                Ok(detail) => Ok::<_, Rejection>(warp::reply::json(&detail)),
                Err(e) => {
                    eprintln!("Run detail error: {}", e);
                    Err(reject::reject())
                }
            }
        })
}

fn runs_file_route(
    token: Arc<String>,
) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
    warp::path!("runs" / String / String)
        .and(warp::get())
        .and(with_auth(token))
        .and_then(|job_id: String, filename: String| async move {
            match get_run_file(&job_id, &filename).await {
                Ok(content) => Ok::<_, Rejection>(warp::reply::with_header(
                    content,
                    "Content-Type",
                    "text/plain; charset=utf-8",
                )),
                Err(e) => {
                    eprintln!("Run file error: {}", e);
                    Err(reject::reject())
                }
            }
        })
}

fn browser_init_route(
    token: Arc<String>,
) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
    warp::path!("browser" / "init")
        .and(warp::get())
        .and(with_auth(token))
        .and_then(|| async move {
            let stream = browser_init_stream();
            Ok::<_, Rejection>(warp::sse::reply(warp::sse::keep_alive().stream(stream)))
        })
}

/// Convert a `ProgressEvent` to the SSE line data string in the EXACT wire
/// format the admin SPA expects:
///   - Per-line events:  `{"line":"<escaped message>"}`
///   - Final event:      `{"status":"done|error","exit_code":N}`  (not from this fn)
///
/// The human-readable line content is produced by the shared
/// `format_progress_line` helper (same rules as the CLI surfaces):
///   - `Phase::Done`   → `✓ <message>`
///   - `Phase::Failed` → `✗ <message>`
///   - `Phase::Log` with `LogStream::Stderr` → `[stderr] <message>`
///   - All other phases → `<message>` unchanged
///
/// The JSON envelope is produced by `serde_json::json!` so that any character
/// that is special in JSON (including embedded newlines from multi-line anyhow
/// error chains, control characters, additional backslashes, etc.) is correctly
/// escaped.  The hand-rolled `replace('\\', …).replace('"', …)` was only safe
/// for single-line messages; multiline failure messages would have broken the
/// SPA's `JSON.parse`.
pub fn progress_event_to_sse_line(event: &ahandd::browser_setup::ProgressEvent) -> String {
    let line = ahandd::browser_setup::format_progress_line(event);
    serde_json::json!({"line": line}).to_string()
}

fn browser_init_stream()
-> impl futures_util::Stream<Item = std::result::Result<warp::sse::Event, Infallible>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<warp::sse::Event>();

    tokio::spawn(async move {
        // Callback: convert each ProgressEvent to a `{"line":"..."}` SSE event.
        // Wire format is byte-compatible with the old bash-stream implementation
        // (same escaping: backslash → `\\`, double-quote → `\"`).
        let tx_progress = tx.clone();
        let progress = move |event: ahandd::browser_setup::ProgressEvent| {
            let data = progress_event_to_sse_line(&event);
            let _ = tx_progress.send(warp::sse::Event::default().data(data));
        };

        let result = ahandd::browser_setup::run_all(false, progress).await;

        let (exit_code, status_str) = match result {
            Ok(_) => (0i32, "done"),
            Err(_) => (1i32, "error"),
        };
        let data = serde_json::json!({"status": status_str, "exit_code": exit_code}).to_string();
        let _ = tx.send(warp::sse::Event::default().data(data));
    });

    futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|event| (Ok(event), rx))
    })
}

// ──────────────────────────────────────────────────────────────────────
// API Handlers
// ──────────────────────────────────────────────────────────────────────

async fn get_status(config_path: &Path) -> Result<StatusResponse> {
    let data_dir = get_data_dir()?;

    // Check daemon PID
    let pid_file = data_dir.join("daemon.pid");
    let (daemon_running, daemon_pid) = if pid_file.exists() {
        let pid_str = tokio::fs::read_to_string(&pid_file).await?;
        let pid: u32 = pid_str.trim().parse().unwrap_or(0);
        (process::is_process_running(pid), Some(pid))
    } else {
        (false, None)
    };

    // Calculate data dir size
    let data_dir_size = calculate_dir_size(&data_dir).await.unwrap_or(0);

    // Resolve platform-correct home and bin directories.
    let home = dirs::home_dir().context("Failed to find home directory")?;
    let bin_dir = ahand_bin_dir(&home);

    Ok(StatusResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        daemon_running,
        daemon_pid,
        config_path: config_path.display().to_string(),
        data_dir: data_dir.display().to_string(),
        data_dir_size,
        home_dir: home.display().to_string(),
        bin_dir: bin_dir.display().to_string(),
    })
}

async fn get_config(config_path: &Path) -> Result<serde_json::Value> {
    // If config file doesn't exist, return empty object
    if !config_path.exists() {
        return Ok(serde_json::json!({}));
    }

    let toml_str = tokio::fs::read_to_string(config_path)
        .await
        .with_context(|| format!("Failed to read config: {}", config_path.display()))?;

    let toml_value: toml::Value = toml::from_str(&toml_str)?;
    let json_value = serde_json::to_value(&toml_value)?;

    Ok(json_value)
}

async fn put_config(config_path: &Path, json_value: serde_json::Value) -> Result<()> {
    // Ensure parent directory exists
    if let Some(parent) = config_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Convert JSON to TOML
    let toml_value: toml::Value = serde_json::from_value(json_value)?;
    let toml_str = toml::to_string_pretty(&toml_value)?;

    tokio::fs::write(config_path, toml_str)
        .await
        .with_context(|| format!("Failed to write config: {}", config_path.display()))?;

    Ok(())
}

async fn get_logs(limit: usize, offset: usize) -> Result<LogsResponse> {
    let data_dir = get_data_dir()?;
    let trace_file = data_dir.join("trace.jsonl");

    if !trace_file.exists() {
        return Ok(LogsResponse {
            total: 0,
            entries: vec![],
        });
    }

    let content = tokio::fs::read_to_string(&trace_file).await?;
    let mut lines: Vec<_> = content.lines().collect();
    lines.reverse(); // Most recent first

    let total = lines.len();
    let entries: Vec<LogEntry> = lines
        .into_iter()
        .skip(offset)
        .take(limit)
        .filter_map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .and_then(|v| {
                    Some(LogEntry {
                        ts_ms: v.get("ts_ms")?.as_u64()?,
                        direction: v.get("direction")?.as_str()?.to_string(),
                        device_id: v.get("device_id")?.as_str()?.to_string(),
                        msg_id: v.get("msg_id")?.as_str()?.to_string(),
                        seq: v.get("seq")?.as_u64()?,
                        ack: v.get("ack")?.as_u64()?,
                        payload_type: v.get("payload")?.as_object()?.keys().next()?.to_string(),
                    })
                })
        })
        .collect();

    Ok(LogsResponse { total, entries })
}

async fn list_runs(limit: usize, offset: usize) -> Result<RunsResponse> {
    let data_dir = get_data_dir()?;
    let runs_dir = data_dir.join("runs");

    if !runs_dir.exists() {
        return Ok(RunsResponse {
            total: 0,
            runs: vec![],
        });
    }

    let mut entries = tokio::fs::read_dir(&runs_dir).await?;
    let mut runs = Vec::new();

    while let Some(entry) = entries.next_entry().await? {
        if entry.file_type().await?.is_dir() {
            let job_id = entry.file_name().to_string_lossy().to_string();
            let metadata = entry.metadata().await?;
            let created_at = metadata
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis() as u64;

            runs.push(RunEntry { job_id, created_at });
        }
    }

    // Sort by created_at descending
    runs.sort_by_key(|b| std::cmp::Reverse(b.created_at));

    let total = runs.len();
    let runs = runs.into_iter().skip(offset).take(limit).collect();

    Ok(RunsResponse { total, runs })
}

async fn get_run_detail(job_id: &str) -> Result<RunDetail> {
    let data_dir = get_data_dir()?;
    let run_dir = data_dir.join("runs").join(job_id);

    if !run_dir.exists() {
        anyhow::bail!("Run not found: {}", job_id);
    }

    // Read request.json
    let request_path = run_dir.join("request.json");
    let request: serde_json::Value = if request_path.exists() {
        let content = tokio::fs::read_to_string(&request_path).await?;
        serde_json::from_str(&content)?
    } else {
        serde_json::json!({})
    };

    // Read result.json
    let result_path = run_dir.join("result.json");
    let result: Option<serde_json::Value> = if result_path.exists() {
        let content = tokio::fs::read_to_string(&result_path).await?;
        Some(serde_json::from_str(&content)?)
    } else {
        None
    };

    // List all files
    let mut entries = tokio::fs::read_dir(&run_dir).await?;
    let mut files = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        if entry.file_type().await?.is_file() {
            files.push(entry.file_name().to_string_lossy().to_string());
        }
    }

    Ok(RunDetail {
        job_id: job_id.to_string(),
        request,
        result,
        files,
    })
}

async fn get_run_file(job_id: &str, filename: &str) -> Result<String> {
    let data_dir = get_data_dir()?;
    let file_path = data_dir.join("runs").join(job_id).join(filename);

    if !file_path.exists() {
        anyhow::bail!("File not found: {}/{}", job_id, filename);
    }

    // Security: ensure filename doesn't contain path traversal
    if filename.contains("..") || filename.contains('/') || filename.contains('\\') {
        anyhow::bail!("Invalid filename");
    }

    let content = tokio::fs::read_to_string(&file_path).await?;
    Ok(content)
}

// ──────────────────────────────────────────────────────────────────────
// Error handling
// ──────────────────────────────────────────────────────────────────────

async fn handle_rejection(err: Rejection) -> Result<impl Reply, Infallible> {
    let code;
    let message;

    if err.is_not_found() {
        code = StatusCode::NOT_FOUND;
        message = "Not Found";
    } else if err.find::<Unauthorized>().is_some() {
        code = StatusCode::UNAUTHORIZED;
        message = "Unauthorized";
    } else {
        eprintln!("Unhandled rejection: {:?}", err);
        code = StatusCode::INTERNAL_SERVER_ERROR;
        message = "Internal Server Error";
    }

    Ok(warp::reply::with_status(message, code))
}

// ──────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────

fn generate_token() -> String {
    use rand::RngCore;
    let mut rng = rand::thread_rng();
    let mut bytes = [0u8; 32];
    rng.fill_bytes(&mut bytes);
    bytes
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>()
}

fn resolve_config_path(path: Option<String>) -> Result<PathBuf> {
    if let Some(p) = path {
        Ok(PathBuf::from(p))
    } else {
        let home = dirs::home_dir().context("Failed to find home directory")?;
        Ok(home.join(".ahand").join("config.toml"))
    }
}

fn resolve_dist_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Failed to find home directory")?;
    let dist = home.join(".ahand").join("admin").join("dist");

    if !dist.exists() {
        eprintln!("Warning: SPA dist not found at {}", dist.display());
        eprintln!("Run: bash scripts/deploy-admin.sh");
    }

    Ok(dist)
}

fn get_data_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Failed to find home directory")?;
    Ok(home.join(".ahand").join("data"))
}

/// Resolve the `.ahand/bin` directory under the given home path.
/// Pure (no I/O) so it can be unit-tested across platforms.
fn ahand_bin_dir(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".ahand").join("bin")
}

fn calculate_dir_size(
    path: &Path,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<u64>> + Send + '_>> {
    Box::pin(async move {
        if !path.exists() {
            return Ok(0);
        }

        let mut total = 0u64;
        let mut entries = tokio::fs::read_dir(path).await?;

        while let Some(entry) = entries.next_entry().await? {
            let metadata = entry.metadata().await?;
            if metadata.is_file() {
                total += metadata.len();
            } else if metadata.is_dir() {
                total += calculate_dir_size(&entry.path()).await.unwrap_or(0);
            }
        }

        Ok(total)
    })
}

// ──────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod sse_adapter_tests {
    use super::*;
    use ahandd::browser_setup::{LogStream, Phase, ProgressEvent};

    fn make_event(phase: Phase, message: &str) -> ProgressEvent {
        ProgressEvent {
            step: "node",
            phase,
            message: message.to_string(),
            percent: None,
            stream: None,
        }
    }

    fn make_log_event(message: &str, stream: Option<LogStream>) -> ProgressEvent {
        ProgressEvent {
            step: "playwright",
            phase: Phase::Log,
            message: message.to_string(),
            percent: None,
            stream,
        }
    }

    /// Plain message with no special characters should be wrapped unchanged.
    #[test]
    fn plain_message_wrapped_in_line_object() {
        let event = make_event(Phase::Starting, "Installing Node.js");
        let result = progress_event_to_sse_line(&event);
        assert_eq!(result, r#"{"line":"Installing Node.js"}"#);
    }

    /// Backslashes must be escaped as `\\` — parity with old bash stream.
    #[test]
    fn backslash_is_escaped() {
        let event = make_event(Phase::Installing, r"path\to\node");
        let result = progress_event_to_sse_line(&event);
        // raw string: path\\to\\node inside the JSON string
        assert_eq!(result, r#"{"line":"path\\to\\node"}"#);
    }

    /// Double-quotes must be escaped as `\"` — parity with old bash stream.
    #[test]
    fn double_quote_is_escaped() {
        let event = make_log_event(r#"npm warn "deprecated""#, Some(LogStream::Stdout));
        let result = progress_event_to_sse_line(&event);
        assert_eq!(result, r#"{"line":"npm warn \"deprecated\""}"#);
    }

    /// Both backslash and double-quote in the same message.
    #[test]
    fn backslash_and_quote_both_escaped() {
        let event = make_log_event(r#"C:\Users\"name""#, Some(LogStream::Stdout));
        let result = progress_event_to_sse_line(&event);
        assert_eq!(result, r#"{"line":"C:\\Users\\\"name\""}"#);
    }

    /// Unicode content passes through without modification.
    #[test]
    fn unicode_content_passes_through() {
        let event = make_event(Phase::Verifying, "Verification \u{2713} complete");
        let result = progress_event_to_sse_line(&event);
        assert_eq!(
            result,
            "{\u{22}line\u{22}:\u{22}Verification \u{2713} complete\u{22}}"
        );
    }

    /// Phase::Done prepends a check-mark to the message.
    #[test]
    fn done_phase_prepends_check_mark() {
        let event = make_event(Phase::Done, "Node.js installed");
        let result = progress_event_to_sse_line(&event);
        assert_eq!(
            result,
            "{\u{22}line\u{22}:\u{22}\u{2713} Node.js installed\u{22}}"
        );
    }

    /// Phase::Failed prepends a cross mark — NOT a check mark.
    #[test]
    fn failed_phase_prepends_cross_mark() {
        let event = make_event(Phase::Failed, "EACCES: permission denied");
        let result = progress_event_to_sse_line(&event);
        // ✗ is U+2717
        assert_eq!(
            result,
            "{\u{22}line\u{22}:\u{22}\u{2717} EACCES: permission denied\u{22}}"
        );
    }

    /// Phase::Log with LogStream::Stderr gets a [stderr] prefix.
    #[test]
    fn log_stderr_gets_stderr_prefix() {
        let event = make_log_event("npm warn deprecated foo@1.0.0", Some(LogStream::Stderr));
        let result = progress_event_to_sse_line(&event);
        assert_eq!(
            result,
            r#"{"line":"[stderr] npm warn deprecated foo@1.0.0"}"#
        );
    }

    /// Phase::Log with LogStream::Stdout has no prefix.
    #[test]
    fn log_stdout_has_no_prefix() {
        let event = make_log_event("npm notice flushed", Some(LogStream::Stdout));
        let result = progress_event_to_sse_line(&event);
        assert_eq!(result, r#"{"line":"npm notice flushed"}"#);
    }

    /// Phase::Log with no stream has no prefix.
    #[test]
    fn log_no_stream_has_no_prefix() {
        let event = make_log_event("some output", None);
        let result = progress_event_to_sse_line(&event);
        assert_eq!(result, r#"{"line":"some output"}"#);
    }

    /// [stderr] prefix + backslash escaping both apply correctly.
    #[test]
    fn log_stderr_with_backslash_escapes_correctly() {
        let event = make_log_event(r"npm ERR! path\to\module", Some(LogStream::Stderr));
        let result = progress_event_to_sse_line(&event);
        assert_eq!(result, r#"{"line":"[stderr] npm ERR! path\\to\\module"}"#);
    }

    /// Result is valid JSON that the SPA can JSON.parse successfully.
    #[test]
    fn result_is_valid_json_parseable_by_serde() {
        let cases = [
            make_event(Phase::Starting, "Starting"),
            make_log_event(r#"warn "pkg" deprecated"#, Some(LogStream::Stdout)),
            make_event(Phase::Done, r"C:\path\done"),
            make_event(Phase::Failed, "install error"),
            make_log_event("npm error msg", Some(LogStream::Stderr)),
        ];
        for event in &cases {
            let s = progress_event_to_sse_line(event);
            let parsed: serde_json::Value =
                serde_json::from_str(&s).expect("result must be valid JSON");
            assert!(
                parsed.get("line").is_some(),
                "parsed JSON must have 'line' key: {s}"
            );
        }
    }

    // -------------------------------------------------------------------------
    // Multiline + control-char tests (serde_json envelope, item 2)
    // These confirm that embedded newlines (from anyhow error chains) and
    // control characters are correctly escaped by serde_json so the SPA's
    // JSON.parse cannot break.
    // -------------------------------------------------------------------------

    /// A failure message with embedded newlines (anyhow chain) must be
    /// serialised with `\n` escape sequences, NOT raw newlines, and the
    /// result must parse as valid JSON.
    #[test]
    fn multiline_message_is_valid_json_with_escaped_newlines() {
        let message = "npm ERR! code EACCES\nnpm ERR! syscall access\nnpm ERR! path /usr/lib";
        let event = make_event(Phase::Failed, message);
        let result = progress_event_to_sse_line(&event);

        // Must parse as valid JSON.
        let parsed: serde_json::Value =
            serde_json::from_str(&result).expect("multiline message must produce valid JSON");

        // The "line" value must exist and contain the original text
        // (serde_json will have decoded the \\n escape back to \n).
        let line_val = parsed
            .get("line")
            .and_then(|v| v.as_str())
            .expect("'line' key must be a string");
        // The prefix char (✗) is added by format_progress_line for Phase::Failed.
        assert!(
            line_val.contains("EACCES"),
            "line value should contain original error text: {line_val}"
        );

        // The raw serialised string must NOT contain a bare newline between
        // the opening { and closing } — it must use \\n escapes.
        assert!(
            !result.contains('\n'),
            "serialised SSE line must not contain a bare newline: {result:?}"
        );
    }

    /// A message with control characters (tab, carriage-return) must also be
    /// escaped, not emitted as raw control characters, so JSON.parse is safe.
    #[test]
    fn control_chars_are_escaped_in_sse_json() {
        let message = "step\t(tab)\rstep2";
        let event = make_event(Phase::Log, message);
        let result = progress_event_to_sse_line(&event);

        // Must parse as valid JSON regardless of any control chars in message.
        let _parsed: serde_json::Value =
            serde_json::from_str(&result).expect("control chars must produce valid JSON");

        // Raw CR and TAB must not appear inside the JSON string value.
        assert!(
            !result.contains('\r'),
            "serialised SSE line must not contain raw CR: {result:?}"
        );
        assert!(
            !result.contains('\t'),
            "serialised SSE line must not contain raw TAB: {result:?}"
        );
    }

    // -------------------------------------------------------------------------
    // Stream-level error-path test (audit TOP-1, item 2)
    // Drive the part of browser_init_stream that maps Ok/Err → final event
    // and assert the final event is {"status":"error","exit_code":1}.
    // -------------------------------------------------------------------------

    /// Helper that simulates the status-event construction in browser_init_stream:
    /// given a `Result`, build the final SSE data string exactly as the route does.
    fn make_final_status_event(result: anyhow::Result<()>) -> String {
        let (exit_code, status_str) = match result {
            Ok(_) => (0i32, "done"),
            Err(_) => (1i32, "error"),
        };
        serde_json::json!({"status": status_str, "exit_code": exit_code}).to_string()
    }

    /// When run_all returns Err, the final event must be
    /// `{"status":"error","exit_code":1}`.
    #[test]
    fn stream_error_result_emits_error_status_event() {
        let data = make_final_status_event(Err(anyhow::anyhow!("simulated failure")));

        let parsed: serde_json::Value =
            serde_json::from_str(&data).expect("final event must be valid JSON");

        assert_eq!(
            parsed.get("status").and_then(|v| v.as_str()),
            Some("error"),
            "status must be 'error' on Err: {data}"
        );
        assert_eq!(
            parsed.get("exit_code").and_then(|v| v.as_i64()),
            Some(1),
            "exit_code must be 1 on Err: {data}"
        );
    }

    /// When run_all returns Ok, the final event must be
    /// `{"status":"done","exit_code":0}`.
    #[test]
    fn stream_ok_result_emits_done_status_event() {
        let data = make_final_status_event(Ok(()));

        let parsed: serde_json::Value =
            serde_json::from_str(&data).expect("final event must be valid JSON");

        assert_eq!(
            parsed.get("status").and_then(|v| v.as_str()),
            Some("done"),
            "status must be 'done' on Ok: {data}"
        );
        assert_eq!(
            parsed.get("exit_code").and_then(|v| v.as_i64()),
            Some(0),
            "exit_code must be 0 on Ok: {data}"
        );
    }

    // -------------------------------------------------------------------------
    // Path-helper test: ahand_bin_dir is the pure builder behind StatusResponse
    // `bin_dir`. Build the expected path with `.join` so it stays correct on
    // both POSIX (`/`) and Windows (`\`) separators.
    // -------------------------------------------------------------------------

    /// `ahand_bin_dir` appends `.ahand/bin` to the given home directory.
    #[test]
    fn ahand_bin_dir_appends_dot_ahand_bin() {
        use std::path::Path;
        let home = Path::new("/home/alice");
        let expected = home.join(".ahand").join("bin");
        assert_eq!(ahand_bin_dir(home), expected);
    }
}
