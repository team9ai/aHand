use ahand_protocol::{
    envelope, ApprovalResponse, CancelJob, Envelope, Hello, JobRequest, PolicyQuery, PolicyUpdate,
    SessionQuery, SetSessionMode,
};
use clap::{Parser, Subcommand};
use futures_util::{SinkExt, StreamExt};
use prost::Message;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite;
use tracing::info;

mod admin;
mod browser_init;
mod daemon;
mod upgrade;

#[derive(Parser)]
#[command(name = "ahandctl", about = "AHand CLI debug tool")]
struct Args {
    /// Cloud WebSocket URL
    #[arg(long, default_value = "ws://localhost:3000/ws")]
    url: String,

    /// Connect via IPC Unix socket instead of WebSocket
    #[arg(long)]
    ipc: Option<String>,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Send a job and stream its output
    Exec {
        /// Tool to execute
        tool: String,
        /// Arguments to the tool
        args: Vec<String>,
    },
    /// Cancel a running job
    Cancel {
        /// Job ID to cancel
        job_id: String,
    },
    /// Ping the server (connect, send Hello, disconnect)
    Ping,
    /// Listen for approval requests and respond interactively
    Approve,
    /// Query or update the daemon's policy
    Policy {
        #[command(subcommand)]
        action: PolicyAction,
    },
    /// Query or set session mode
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },
    /// Start local admin panel HTTP server
    Configure {
        /// HTTP server port
        #[arg(long, default_value = "9800")]
        port: u16,
        /// Config file path (defaults to ~/.ahand/config.toml)
        #[arg(long)]
        config: Option<String>,
        /// Don't automatically open browser
        #[arg(long)]
        no_open: bool,
    },
    /// Initialize browser automation dependencies
    BrowserInit {
        /// Force reinstall (clean existing installation first)
        #[arg(long)]
        force: bool,
    },
    /// Check for updates or upgrade to the latest version
    Upgrade {
        /// Only check for updates, don't install
        #[arg(long)]
        check: bool,
        /// Upgrade to a specific version
        #[arg(long)]
        version: Option<String>,
    },
    /// Start the ahandd daemon in the background
    Start {
        /// Path to config file (defaults to ~/.ahand/config.toml)
        #[arg(long)]
        config: Option<String>,
    },
    /// Stop the running ahandd daemon
    Stop,
    /// Restart the ahandd daemon (stop + start)
    Restart {
        /// Path to config file (defaults to ~/.ahand/config.toml)
        #[arg(long)]
        config: Option<String>,
    },
    /// Show daemon status
    Status,
}

#[derive(Subcommand)]
enum PolicyAction {
    /// Show current policy
    Show,
    /// Add tools to the allowlist
    AllowTool {
        /// Tool names to allow
        tools: Vec<String>,
    },
    /// Remove tools from the allowlist
    DisallowTool {
        /// Tool names to remove from allowlist
        tools: Vec<String>,
    },
    /// Add tools to the denylist
    DenyTool {
        /// Tool names to deny
        tools: Vec<String>,
    },
    /// Remove tools from the denylist
    UndenyTool {
        /// Tool names to remove from denylist
        tools: Vec<String>,
    },
    /// Add domains to the allowlist
    AllowDomain {
        /// Domain names to allow
        domains: Vec<String>,
    },
    /// Remove domains from the allowlist
    DisallowDomain {
        /// Domain names to remove from allowlist
        domains: Vec<String>,
    },
    /// Set approval timeout in seconds
    SetTimeout {
        /// Timeout in seconds (0 = no change)
        seconds: u64,
    },
}

#[derive(Subcommand)]
enum SessionAction {
    /// Show current session state for all callers
    Show {
        /// Filter by caller UID (empty = all)
        #[arg(long, default_value = "")]
        caller: String,
    },
    /// Set session mode for a caller
    Set {
        /// Session mode: inactive, strict, trust, auto_accept
        mode: String,
        /// Caller UID
        #[arg(long, default_value = "cloud")]
        caller: String,
        /// Trust timeout in minutes (only for trust mode)
        #[arg(long, default_value = "0")]
        timeout: u64,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    // Commands that don't use IPC/WS, handle early
    match &args.command {
        Cmd::Configure { .. } => {
            if let Cmd::Configure { port, config, no_open } = args.command {
                return admin::serve(port, config, no_open).await;
            }
        }
        Cmd::BrowserInit { force } => {
            return browser_init::run(*force).await;
        }
        Cmd::Upgrade { check, version } => {
            return upgrade::run(*check, version.clone()).await;
        }
        Cmd::Start { config } => {
            return daemon::start(config.clone()).await;
        }
        Cmd::Stop => {
            return daemon::stop().await;
        }
        Cmd::Restart { config } => {
            return daemon::restart(config.clone()).await;
        }
        Cmd::Status => {
            return daemon::status().await;
        }
        _ => {}
    }

    if let Some(ipc_path) = &args.ipc {
        // IPC mode — connect via Unix socket.
        match args.command {
            Cmd::Exec { tool, args: tool_args } => {
                ipc_exec(ipc_path, &tool, &tool_args).await?;
            }
            Cmd::Cancel { job_id } => {
                ipc_cancel(ipc_path, &job_id).await?;
            }
            Cmd::Ping => {
                eprintln!("Ping is not supported in IPC mode");
                std::process::exit(1);
            }
            Cmd::Approve => {
                ipc_approve(ipc_path).await?;
            }
            Cmd::Policy { action } => {
                ipc_policy(ipc_path, action).await?;
            }
            Cmd::Session { action } => {
                ipc_session(ipc_path, action).await?;
            }
            Cmd::Configure { .. } | Cmd::BrowserInit { .. } | Cmd::Upgrade { .. }
            | Cmd::Start { .. } | Cmd::Stop | Cmd::Restart { .. } | Cmd::Status => {
                unreachable!("Handled early, should not reach here");
            }
        }
    } else {
        // WS mode.
        match args.command {
            Cmd::Exec { tool, args: tool_args } => {
                ws_exec(&args.url, &tool, &tool_args).await?;
            }
            Cmd::Cancel { job_id } => {
                ws_cancel(&args.url, &job_id).await?;
            }
            Cmd::Ping => {
                ws_ping(&args.url).await?;
            }
            Cmd::Approve => {
                eprintln!("Approve is only supported in IPC mode (use --ipc <socket>)");
                std::process::exit(1);
            }
            Cmd::Policy { action } => {
                ws_policy(&args.url, action).await?;
            }
            Cmd::Session { action } => {
                ws_session(&args.url, action).await?;
            }
            Cmd::Configure { .. } | Cmd::BrowserInit { .. } | Cmd::Upgrade { .. }
            | Cmd::Start { .. } | Cmd::Stop | Cmd::Restart { .. } | Cmd::Status => {
                unreachable!("Handled early, should not reach here");
            }
        }
    }

    Ok(())
}

// ── IPC frame helpers ────────────────────────────────────────────────

async fn read_frame<R: AsyncReadExt + Unpin>(reader: &mut R) -> std::io::Result<Vec<u8>> {
    let len = reader.read_u32().await? as usize;
    if len > 16 * 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    Ok(buf)
}

async fn write_frame<W: AsyncWriteExt + Unpin>(writer: &mut W, data: &[u8]) -> std::io::Result<()> {
    writer.write_u32(data.len() as u32).await?;
    writer.write_all(data).await?;
    writer.flush().await?;
    Ok(())
}

// ── IPC exec ─────────────────────────────────────────────────────────

async fn ipc_exec(socket_path: &str, tool: &str, args: &[String]) -> anyhow::Result<()> {
    let stream = tokio::net::UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(&mut reader);

    let device_id = format!("ctl-{}", std::process::id());
    let job_id = format!("ctl-job-{}", std::process::id());

    // Send JobRequest.
    let req = Envelope {
        device_id: device_id.clone(),
        msg_id: "req-0".to_string(),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::JobRequest(JobRequest {
            job_id: job_id.clone(),
            tool: tool.to_string(),
            args: args.to_vec(),
            ..Default::default()
        })),
        ..Default::default()
    };
    write_frame(&mut writer, &req.encode_to_vec()).await?;

    info!(job_id = %job_id, "IPC: job submitted, waiting for output...");

    // Read responses.
    loop {
        let data = match read_frame(&mut reader).await {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        };

        let envelope = Envelope::decode(data.as_slice())?;

        match envelope.payload {
            Some(envelope::Payload::JobEvent(ev)) => {
                if ev.job_id != job_id {
                    continue;
                }
                match ev.event {
                    Some(ahand_protocol::job_event::Event::StdoutChunk(data)) => {
                        let text = String::from_utf8_lossy(&data);
                        print!("{text}");
                    }
                    Some(ahand_protocol::job_event::Event::StderrChunk(data)) => {
                        let text = String::from_utf8_lossy(&data);
                        eprint!("{text}");
                    }
                    Some(ahand_protocol::job_event::Event::Progress(p)) => {
                        eprintln!("[progress] {p}%");
                    }
                    None => {}
                }
            }
            Some(envelope::Payload::JobFinished(fin)) => {
                if fin.job_id != job_id {
                    continue;
                }
                if fin.error.is_empty() {
                    eprintln!("[finished] exit_code={}", fin.exit_code);
                } else {
                    eprintln!("[finished] exit_code={} error={}", fin.exit_code, fin.error);
                }
                std::process::exit(fin.exit_code);
            }
            Some(envelope::Payload::JobRejected(rej)) => {
                if rej.job_id != job_id {
                    continue;
                }
                eprintln!("[rejected] {}", rej.reason);
                std::process::exit(1);
            }
            Some(envelope::Payload::ApprovalRequest(req)) => {
                if req.job_id != job_id {
                    continue;
                }
                eprintln!("[needs-approval] Job requires approval: {}", req.reason);
                if !req.detected_domains.is_empty() {
                    eprintln!("  Detected domains: {}", req.detected_domains.join(", "));
                }
                eprintln!("  Run `ahandctl --ipc <socket> approve` in another terminal to approve.");
            }
            _ => {}
        }
    }

    Ok(())
}

// ── IPC cancel ───────────────────────────────────────────────────────

async fn ipc_cancel(socket_path: &str, job_id: &str) -> anyhow::Result<()> {
    let stream = tokio::net::UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(&mut reader);

    let device_id = format!("ctl-{}", std::process::id());

    let cancel_env = Envelope {
        device_id: device_id.clone(),
        msg_id: "cancel-0".to_string(),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::CancelJob(CancelJob {
            job_id: job_id.to_string(),
        })),
        ..Default::default()
    };

    write_frame(&mut writer, &cancel_env.encode_to_vec()).await?;
    eprintln!("[cancel] sent cancel request for job {job_id}");

    // Wait for JobFinished confirmation.
    loop {
        let data = match read_frame(&mut reader).await {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        };

        let envelope = Envelope::decode(data.as_slice())?;

        if let Some(envelope::Payload::JobFinished(fin)) = envelope.payload
            && fin.job_id == job_id
        {
            if fin.error.is_empty() {
                eprintln!("[finished] exit_code={}", fin.exit_code);
            } else {
                eprintln!("[finished] exit_code={} error={}", fin.exit_code, fin.error);
            }
            break;
        }
    }

    Ok(())
}

// ── WS functions (existing) ──────────────────────────────────────────

async fn connect_and_hello(
    url: &str,
) -> anyhow::Result<(
    futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        tungstenite::Message,
    >,
    futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
    String,
)> {
    let (ws_stream, _) = tokio_tungstenite::connect_async(url).await?;
    let (mut sink, stream) = ws_stream.split();

    let device_id = format!("ctl-{}", std::process::id());

    let hello = Envelope {
        device_id: device_id.clone(),
        msg_id: "hello-0".to_string(),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::Hello(Hello {
            version: env!("CARGO_PKG_VERSION").to_string(),
            hostname: gethostname::gethostname()
                .to_string_lossy()
                .to_string(),
            os: std::env::consts::OS.to_string(),
            capabilities: vec!["ctl".to_string()],
            last_ack: 0,
        })),
        ..Default::default()
    };

    sink.send(tungstenite::Message::Binary(hello.encode_to_vec()))
        .await?;

    Ok((sink, stream, device_id))
}

async fn ws_exec(url: &str, tool: &str, args: &[String]) -> anyhow::Result<()> {
    let (mut sink, mut stream, device_id) = connect_and_hello(url).await?;

    let job_id = format!("ctl-job-{}", std::process::id());

    let req = Envelope {
        device_id: device_id.clone(),
        msg_id: "req-0".to_string(),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::JobRequest(JobRequest {
            job_id: job_id.clone(),
            tool: tool.to_string(),
            args: args.to_vec(),
            ..Default::default()
        })),
        ..Default::default()
    };

    sink.send(tungstenite::Message::Binary(req.encode_to_vec()))
        .await?;

    info!(job_id = %job_id, "job submitted, waiting for output...");

    while let Some(msg) = stream.next().await {
        let msg = msg?;
        let data = match msg {
            tungstenite::Message::Binary(b) => b,
            tungstenite::Message::Close(_) => break,
            _ => continue,
        };

        let envelope = Envelope::decode(data.as_ref())?;

        match envelope.payload {
            Some(envelope::Payload::JobEvent(ev)) => {
                if ev.job_id != job_id {
                    continue;
                }
                match ev.event {
                    Some(ahand_protocol::job_event::Event::StdoutChunk(data)) => {
                        let text = String::from_utf8_lossy(&data);
                        print!("{text}");
                    }
                    Some(ahand_protocol::job_event::Event::StderrChunk(data)) => {
                        let text = String::from_utf8_lossy(&data);
                        eprint!("{text}");
                    }
                    Some(ahand_protocol::job_event::Event::Progress(p)) => {
                        eprintln!("[progress] {p}%");
                    }
                    None => {}
                }
            }
            Some(envelope::Payload::JobFinished(fin)) => {
                if fin.job_id != job_id {
                    continue;
                }
                if fin.error.is_empty() {
                    eprintln!("[finished] exit_code={}", fin.exit_code);
                } else {
                    eprintln!("[finished] exit_code={} error={}", fin.exit_code, fin.error);
                }
                std::process::exit(fin.exit_code);
            }
            Some(envelope::Payload::JobRejected(rej)) => {
                if rej.job_id != job_id {
                    continue;
                }
                eprintln!("[rejected] {}", rej.reason);
                std::process::exit(1);
            }
            _ => {}
        }
    }

    Ok(())
}

async fn ws_cancel(url: &str, job_id: &str) -> anyhow::Result<()> {
    let (mut sink, mut stream, device_id) = connect_and_hello(url).await?;

    let cancel_env = Envelope {
        device_id: device_id.clone(),
        msg_id: "cancel-0".to_string(),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::CancelJob(CancelJob {
            job_id: job_id.to_string(),
        })),
        ..Default::default()
    };

    sink.send(tungstenite::Message::Binary(cancel_env.encode_to_vec()))
        .await?;

    eprintln!("[cancel] sent cancel request for job {job_id}");

    // Wait for the JobFinished confirmation.
    while let Some(msg) = stream.next().await {
        let msg = msg?;
        let data = match msg {
            tungstenite::Message::Binary(b) => b,
            tungstenite::Message::Close(_) => break,
            _ => continue,
        };

        let envelope = Envelope::decode(data.as_ref())?;

        if let Some(envelope::Payload::JobFinished(fin)) = envelope.payload
            && fin.job_id == job_id
        {
            if fin.error.is_empty() {
                eprintln!("[finished] exit_code={}", fin.exit_code);
            } else {
                eprintln!("[finished] exit_code={} error={}", fin.exit_code, fin.error);
            }
            break;
        }
    }

    sink.close().await?;
    Ok(())
}

async fn ws_ping(url: &str) -> anyhow::Result<()> {
    let (mut sink, _stream, device_id) = connect_and_hello(url).await?;
    println!("connected as {device_id}");
    sink.close().await?;
    println!("disconnected");
    Ok(())
}

// ── IPC approve ──────────────────────────────────────────────────────

async fn ipc_approve(socket_path: &str) -> anyhow::Result<()> {
    let stream = tokio::net::UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(&mut reader);

    let device_id = format!("ctl-{}", std::process::id());
    eprintln!("[approve] Connected as {device_id}. Listening for approval requests...");

    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let mut stdin_lines = stdin.lines();

    loop {
        let data = match read_frame(&mut reader).await {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                eprintln!("[approve] Connection closed.");
                break;
            }
            Err(e) => return Err(e.into()),
        };

        let envelope = Envelope::decode(data.as_slice())?;

        if let Some(envelope::Payload::ApprovalRequest(req)) = envelope.payload {
            eprintln!();
            eprintln!("[approval] Job {} (from {}) wants to run: {} {}", req.job_id, req.caller_uid, req.tool, req.args.join(" "));
            if !req.cwd.is_empty() {
                eprintln!("  Working directory: {}", req.cwd);
            }
            eprintln!("  Reason: {}", req.reason);
            if !req.detected_domains.is_empty() {
                eprintln!("  Detected domains: {}", req.detected_domains.join(", "));
            }
            if req.expires_ms > 0 {
                let remaining = req.expires_ms.saturating_sub(now_ms());
                eprintln!("  Expires in: {}s", remaining / 1000);
            }
            eprint!("Approve? [y/N/r(emember)]: ");

            // Flush stderr to ensure prompt is visible.
            let _ = tokio::io::stderr().flush().await;

            let line = match stdin_lines.next_line().await? {
                Some(l) => l,
                None => break,
            };
            let choice = line.trim().to_lowercase();

            let (approved, remember, reason) = match choice.as_str() {
                "y" | "yes" => (true, false, String::new()),
                "r" | "remember" => (true, true, String::new()),
                _ => {
                    // If the input is longer than a single char, treat it as a refusal reason.
                    let reason = if choice.len() > 1 && choice != "n" && choice != "no" {
                        choice.clone()
                    } else {
                        String::new()
                    };
                    (false, false, reason)
                }
            };

            let resp_env = Envelope {
                device_id: device_id.clone(),
                msg_id: format!("approve-{}", now_ms()),
                ts_ms: now_ms(),
                payload: Some(envelope::Payload::ApprovalResponse(ApprovalResponse {
                    job_id: req.job_id.clone(),
                    approved,
                    remember,
                    reason: reason.clone(),
                })),
                ..Default::default()
            };
            write_frame(&mut writer, &resp_env.encode_to_vec()).await?;

            if approved {
                eprintln!("[approval] Approved job {}{}", req.job_id, if remember { " (remembered)" } else { "" });
            } else if reason.is_empty() {
                eprintln!("[approval] Denied job {}", req.job_id);
            } else {
                eprintln!("[approval] Denied job {} with reason: {}", req.job_id, reason);
            }
        }
    }

    Ok(())
}

// ── IPC policy ───────────────────────────────────────────────────────

async fn ipc_policy(socket_path: &str, action: PolicyAction) -> anyhow::Result<()> {
    let stream = tokio::net::UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(&mut reader);

    let device_id = format!("ctl-{}", std::process::id());

    let request_env = match &action {
        PolicyAction::Show => Envelope {
            device_id: device_id.clone(),
            msg_id: "policy-query-0".to_string(),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::PolicyQuery(PolicyQuery {})),
            ..Default::default()
        },
        _ => {
            let update = build_policy_update(&action);
            Envelope {
                device_id: device_id.clone(),
                msg_id: "policy-update-0".to_string(),
                ts_ms: now_ms(),
                payload: Some(envelope::Payload::PolicyUpdate(update)),
                ..Default::default()
            }
        }
    };

    write_frame(&mut writer, &request_env.encode_to_vec()).await?;

    // Wait for PolicyState response.
    loop {
        let data = match read_frame(&mut reader).await {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                eprintln!("[policy] Connection closed before receiving response.");
                break;
            }
            Err(e) => return Err(e.into()),
        };

        let envelope = Envelope::decode(data.as_slice())?;

        if let Some(envelope::Payload::PolicyState(state)) = envelope.payload {
            print_policy_state(&state);
            break;
        }
    }

    Ok(())
}

// ── WS policy ────────────────────────────────────────────────────────

async fn ws_policy(url: &str, action: PolicyAction) -> anyhow::Result<()> {
    let (mut sink, mut stream, device_id) = connect_and_hello(url).await?;

    let request_env = match &action {
        PolicyAction::Show => Envelope {
            device_id: device_id.clone(),
            msg_id: "policy-query-0".to_string(),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::PolicyQuery(PolicyQuery {})),
            ..Default::default()
        },
        _ => {
            let update = build_policy_update(&action);
            Envelope {
                device_id: device_id.clone(),
                msg_id: "policy-update-0".to_string(),
                ts_ms: now_ms(),
                payload: Some(envelope::Payload::PolicyUpdate(update)),
                ..Default::default()
            }
        }
    };

    sink.send(tungstenite::Message::Binary(
        request_env.encode_to_vec(),
    ))
    .await?;

    // Wait for PolicyState response.
    while let Some(msg) = stream.next().await {
        let msg = msg?;
        let data = match msg {
            tungstenite::Message::Binary(b) => b,
            tungstenite::Message::Close(_) => break,
            _ => continue,
        };

        let envelope = Envelope::decode(data.as_ref())?;

        if let Some(envelope::Payload::PolicyState(state)) = envelope.payload {
            print_policy_state(&state);
            break;
        }
    }

    sink.close().await?;
    Ok(())
}

// ── IPC session ─────────────────────────────────────────────────────

async fn ipc_session(socket_path: &str, action: SessionAction) -> anyhow::Result<()> {
    let stream = tokio::net::UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(&mut reader);

    let device_id = format!("ctl-{}", std::process::id());

    let request_env = build_session_envelope(&device_id, &action);
    write_frame(&mut writer, &request_env.encode_to_vec()).await?;

    // Wait for SessionState response(s).
    loop {
        let data = match read_frame(&mut reader).await {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        };

        let envelope = Envelope::decode(data.as_slice())?;

        if let Some(envelope::Payload::SessionState(state)) = envelope.payload {
            print_session_state(&state);
            break;
        }
    }

    Ok(())
}

// ── WS session ──────────────────────────────────────────────────────

async fn ws_session(url: &str, action: SessionAction) -> anyhow::Result<()> {
    let (mut sink, mut stream, device_id) = connect_and_hello(url).await?;

    let request_env = build_session_envelope(&device_id, &action);
    sink.send(tungstenite::Message::Binary(request_env.encode_to_vec()))
        .await?;

    // Wait for SessionState response(s).
    while let Some(msg) = stream.next().await {
        let msg = msg?;
        let data = match msg {
            tungstenite::Message::Binary(b) => b,
            tungstenite::Message::Close(_) => break,
            _ => continue,
        };

        let envelope = Envelope::decode(data.as_ref())?;

        if let Some(envelope::Payload::SessionState(state)) = envelope.payload {
            print_session_state(&state);
            break;
        }
    }

    sink.close().await?;
    Ok(())
}

// ── Session helpers ─────────────────────────────────────────────────

fn build_session_envelope(device_id: &str, action: &SessionAction) -> Envelope {
    match action {
        SessionAction::Show { caller } => Envelope {
            device_id: device_id.to_string(),
            msg_id: "session-query-0".to_string(),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::SessionQuery(SessionQuery {
                caller_uid: caller.clone(),
            })),
            ..Default::default()
        },
        SessionAction::Set { mode, caller, timeout } => {
            let mode_val = match mode.as_str() {
                "inactive" => 0,
                "strict" => 1,
                "trust" => 2,
                "auto_accept" | "auto" => 3,
                other => {
                    eprintln!("Unknown mode: {other}. Use: inactive, strict, trust, auto_accept");
                    std::process::exit(1);
                }
            };
            Envelope {
                device_id: device_id.to_string(),
                msg_id: "session-set-0".to_string(),
                ts_ms: now_ms(),
                payload: Some(envelope::Payload::SetSessionMode(SetSessionMode {
                    caller_uid: caller.clone(),
                    mode: mode_val,
                    trust_timeout_mins: *timeout,
                })),
                ..Default::default()
            }
        }
    }
}

fn print_session_state(state: &ahand_protocol::SessionState) {
    let mode_name = match state.mode {
        0 => "inactive",
        1 => "strict",
        2 => "trust",
        3 => "auto_accept",
        _ => "unknown",
    };
    println!("Session: caller={} mode={}", state.caller_uid, mode_name);
    if state.trust_expires_ms > 0 {
        let remaining = state.trust_expires_ms.saturating_sub(now_ms());
        println!("  Trust expires in: {}s", remaining / 1000);
    }
    if state.trust_timeout_mins > 0 {
        println!("  Trust timeout: {}min", state.trust_timeout_mins);
    }
}

// ── Policy helpers ───────────────────────────────────────────────────

fn build_policy_update(action: &PolicyAction) -> PolicyUpdate {
    match action {
        PolicyAction::Show => unreachable!(),
        PolicyAction::AllowTool { tools } => PolicyUpdate {
            add_allowed_tools: tools.clone(),
            ..Default::default()
        },
        PolicyAction::DisallowTool { tools } => PolicyUpdate {
            remove_allowed_tools: tools.clone(),
            ..Default::default()
        },
        PolicyAction::DenyTool { tools } => PolicyUpdate {
            add_denied_tools: tools.clone(),
            ..Default::default()
        },
        PolicyAction::UndenyTool { tools } => PolicyUpdate {
            remove_denied_tools: tools.clone(),
            ..Default::default()
        },
        PolicyAction::AllowDomain { domains } => PolicyUpdate {
            add_allowed_domains: domains.clone(),
            ..Default::default()
        },
        PolicyAction::DisallowDomain { domains } => PolicyUpdate {
            remove_allowed_domains: domains.clone(),
            ..Default::default()
        },
        PolicyAction::SetTimeout { seconds } => PolicyUpdate {
            approval_timeout_secs: *seconds,
            ..Default::default()
        },
    }
}

fn print_policy_state(state: &ahand_protocol::PolicyState) {
    println!("Policy:");
    println!("  Allowed tools:   {}", format_list(&state.allowed_tools));
    println!("  Denied tools:    {}", format_list(&state.denied_tools));
    println!("  Denied paths:    {}", format_list(&state.denied_paths));
    println!("  Allowed domains: {}", format_list(&state.allowed_domains));
    println!(
        "  Approval timeout: {}s ({})",
        state.approval_timeout_secs,
        humanize_duration(state.approval_timeout_secs)
    );
}

fn format_list(items: &[String]) -> String {
    if items.is_empty() {
        "(none)".to_string()
    } else {
        items.join(", ")
    }
}

fn humanize_duration(secs: u64) -> String {
    if secs >= 86400 {
        let days = secs / 86400;
        let hours = (secs % 86400) / 3600;
        if hours > 0 {
            format!("{days}d {hours}h")
        } else {
            format!("{days}d")
        }
    } else if secs >= 3600 {
        let hours = secs / 3600;
        let mins = (secs % 3600) / 60;
        if mins > 0 {
            format!("{hours}h {mins}m")
        } else {
            format!("{hours}h")
        }
    } else if secs >= 60 {
        let mins = secs / 60;
        format!("{mins}m")
    } else {
        format!("{secs}s")
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
