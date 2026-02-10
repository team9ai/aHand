use anyhow::{Context, Result};
use serde::Serialize;
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use warp::http::StatusCode;
use warp::{reject, Filter, Rejection, Reply};

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
        });

    println!("Server running at http://{}", addr);
    println!("Press Ctrl-C to stop");
    println!();

    server.await;
    println!("\nServer stopped");
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────
// Auth middleware
// ──────────────────────────────────────────────────────────────────────

#[derive(Debug)]
struct Unauthorized;
impl reject::Reject for Unauthorized {}

fn with_auth(
    token: Arc<String>,
) -> impl Filter<Extract = (), Error = Rejection> + Clone {
    warp::any()
        .and(warp::header::optional::<String>("authorization"))
        .and(warp::query::<std::collections::HashMap<String, String>>())
        .and_then(move |auth_header: Option<String>, query: std::collections::HashMap<String, String>| {
            let token = token.clone();
            async move {
                // Check Authorization header
                if let Some(header) = auth_header {
                    if let Some(bearer) = header.strip_prefix("Bearer ") {
                        if bearer == token.as_str() {
                            return Ok::<_, Rejection>(());
                        }
                    }
                }

                // Check query parameter
                if let Some(query_token) = query.get("token") {
                    if query_token == token.as_str() {
                        return Ok(());
                    }
                }

                Err(reject::custom(Unauthorized))
            }
        })
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
        .and_then(|query: std::collections::HashMap<String, String>| async move {
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
        })
}

fn runs_list_route(token: Arc<String>) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
    warp::path!("runs")
        .and(warp::get())
        .and(with_auth(token))
        .and(warp::query::<std::collections::HashMap<String, String>>())
        .and_then(|query: std::collections::HashMap<String, String>| async move {
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
        })
}

fn runs_get_route(token: Arc<String>) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
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

fn runs_file_route(token: Arc<String>) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
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

fn browser_init_stream(
) -> impl futures_util::Stream<Item = std::result::Result<warp::sse::Event, Infallible>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<warp::sse::Event>();

    tokio::spawn(async move {
        let home = match dirs::home_dir() {
            Some(h) => h,
            None => {
                let _ = tx.send(
                    warp::sse::Event::default()
                        .data(r#"{"line":"ERROR: Failed to find home directory","status":"error","exit_code":1}"#),
                );
                return;
            }
        };

        let script_path = home.join(".ahand").join("bin").join("setup-browser.sh");
        if !script_path.exists() {
            let msg = format!(
                r#"{{"line":"ERROR: setup-browser.sh not found at {}","status":"error","exit_code":1}}"#,
                script_path.display()
            );
            let _ = tx.send(warp::sse::Event::default().data(msg));
            return;
        }

        let mut cmd = tokio::process::Command::new("bash");
        cmd.arg(&script_path);
        cmd.arg("--from-release");
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let msg = format!(
                    r#"{{"line":"ERROR: Failed to spawn setup-browser.sh: {}","status":"error","exit_code":1}}"#,
                    e
                );
                let _ = tx.send(warp::sse::Event::default().data(msg));
                return;
            }
        };

        let stdout = child.stdout.take().expect("stdout");
        let stderr = child.stderr.take().expect("stderr");

        let tx_out = tx.clone();
        let stdout_task = tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let escaped = line.replace('\\', "\\\\").replace('"', "\\\"");
                let data = format!(r#"{{"line":"{}"}}"#, escaped);
                if tx_out.send(warp::sse::Event::default().data(data)).is_err() {
                    break;
                }
            }
        });

        let tx_err = tx.clone();
        let stderr_task = tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let escaped = line.replace('\\', "\\\\").replace('"', "\\\"");
                let data = format!(r#"{{"line":"[stderr] {}"}}"#, escaped);
                if tx_err.send(warp::sse::Event::default().data(data)).is_err() {
                    break;
                }
            }
        });

        let status = child.wait().await;
        let _ = stdout_task.await;
        let _ = stderr_task.await;

        let exit_code = status.map(|s| s.code().unwrap_or(1)).unwrap_or(1);
        let status_str = if exit_code == 0 { "done" } else { "error" };
        let data = format!(r#"{{"status":"{}","exit_code":{}}}"#, status_str, exit_code);
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
        (is_process_running(pid), Some(pid))
    } else {
        (false, None)
    };

    // Calculate data dir size
    let data_dir_size = calculate_dir_size(&data_dir).await.unwrap_or(0);

    Ok(StatusResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        daemon_running,
        daemon_pid,
        config_path: config_path.display().to_string(),
        data_dir: data_dir.display().to_string(),
        data_dir_size,
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
                        payload_type: v
                            .get("payload")?
                            .as_object()?
                            .keys()
                            .next()?
                            .to_string(),
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
    runs.sort_by(|a, b| b.created_at.cmp(&a.created_at));

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

#[cfg(target_os = "linux")]
fn is_process_running(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{}", pid)).exists()
}

#[cfg(not(target_os = "linux"))]
fn is_process_running(pid: u32) -> bool {
    use std::process::Command;
    Command::new("ps")
        .args(&["-p", &pid.to_string()])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}
