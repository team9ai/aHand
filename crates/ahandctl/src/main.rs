use ahand_protocol::{envelope, Envelope, Hello, JobRequest};
use clap::{Parser, Subcommand};
use futures_util::{SinkExt, StreamExt};
use prost::Message;
use tokio_tungstenite::tungstenite;
use tracing::info;

#[derive(Parser)]
#[command(name = "ahandctl", about = "AHand CLI debug tool")]
struct Args {
    /// Cloud WebSocket URL
    #[arg(long, default_value = "ws://localhost:3000/ws")]
    url: String,

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
    /// Ping the server (connect, send Hello, disconnect)
    Ping,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    match args.command {
        Cmd::Exec { tool, args: tool_args } => {
            exec(&args.url, &tool, &tool_args).await?;
        }
        Cmd::Ping => {
            ping(&args.url).await?;
        }
    }

    Ok(())
}

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
        })),
        ..Default::default()
    };

    sink.send(tungstenite::Message::Binary(hello.encode_to_vec().into()))
        .await?;

    Ok((sink, stream, device_id))
}

async fn exec(url: &str, tool: &str, args: &[String]) -> anyhow::Result<()> {
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

async fn ping(url: &str) -> anyhow::Result<()> {
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
