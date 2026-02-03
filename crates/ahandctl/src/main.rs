use ahand_protocol::{envelope, CancelJob, Envelope, Hello, JobRequest};
use clap::{Parser, Subcommand};
use futures_util::{SinkExt, StreamExt};
use prost::Message;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite;
use tracing::info;

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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

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

        if let Some(envelope::Payload::JobFinished(fin)) = envelope.payload {
            if fin.job_id == job_id {
                if fin.error.is_empty() {
                    eprintln!("[finished] exit_code={}", fin.exit_code);
                } else {
                    eprintln!("[finished] exit_code={} error={}", fin.exit_code, fin.error);
                }
                break;
            }
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

    sink.send(tungstenite::Message::Binary(hello.encode_to_vec().into()))
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

    sink.send(tungstenite::Message::Binary(req.encode_to_vec().into()))
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

    sink.send(tungstenite::Message::Binary(cancel_env.encode_to_vec().into()))
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

        if let Some(envelope::Payload::JobFinished(fin)) = envelope.payload {
            if fin.job_id == job_id {
                if fin.error.is_empty() {
                    eprintln!("[finished] exit_code={}", fin.exit_code);
                } else {
                    eprintln!("[finished] exit_code={} error={}", fin.exit_code, fin.error);
                }
                break;
            }
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

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
