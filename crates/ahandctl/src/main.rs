use std::collections::HashMap;

use ahand_protocol::{
    ApprovalResponse, CancelJob, Envelope, ExecutionMode, Hello, JobRequest, PolicyQuery,
    PolicyUpdate, SessionQuery, SetSessionMode, StdinChunk, envelope,
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
mod identity;
mod upgrade;

struct ExecRequest {
    execution_mode: ExecutionMode,
    cwd: Option<String>,
    timeout_ms: u64,
    env: HashMap<String, String>,
    input_format: String,
    output_format: String,
    result_parser: String,
    format: String,
    tool: String,
    args: Vec<String>,
}

struct HermesRunArgs {
    hermes_path: String,
    prompt: Option<String>,
    prompt_file: Option<String>,
    cwd: Option<String>,
    timeout_ms: u64,
    model: Option<String>,
    session_id: Option<String>,
    env: Vec<String>,
    mcp_config: Option<String>,
    mcp_config_file: Option<String>,
    mcp_config_mode: Option<String>,
    instructions: Option<String>,
    instructions_file: Option<String>,
}

struct ClaudeCodeRunArgs {
    claude_path: String,
    prompt: Option<String>,
    prompt_file: Option<String>,
    cwd: Option<String>,
    timeout_ms: u64,
    model: Option<String>,
    session_id: Option<String>,
    max_turns: Option<String>,
    system_prompt: Option<String>,
    system_prompt_file: Option<String>,
    permission_mode: Option<String>,
    env: Vec<String>,
    mcp_config: Option<String>,
    mcp_config_file: Option<String>,
    mcp_config_mode: Option<String>,
    instructions: Option<String>,
    instructions_file: Option<String>,
}

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
        /// Execution attach mode: batch, pty, or pipe_stream
        #[arg(long, default_value = "batch")]
        execution_mode: String,
        /// Working directory for the job
        #[arg(long)]
        cwd: Option<String>,
        /// Job timeout in milliseconds (0 = no timeout)
        #[arg(long, default_value = "0")]
        timeout_ms: u64,
        /// Environment override in KEY=VALUE form; repeatable
        #[arg(long = "env")]
        env: Vec<String>,
        /// Stdin format: raw, text, claude-stream-json, or hermes-acp-json-rpc
        #[arg(long = "input-format", default_value = "raw")]
        input_format: String,
        /// Stdout format: raw, codex-jsonl, claude-stream-json, or hermes-acp-json-rpc
        #[arg(long = "output-format", default_value = "raw")]
        output_format: String,
        /// Output parser hint: raw, codex-jsonl, or claude-stream-json
        #[arg(long = "result-parser", default_value = "raw")]
        result_parser: String,
        /// Deprecated formatter hint: raw, codex, or claude-code
        #[arg(long = "format", default_value = "raw")]
        format: String,
        /// MCP config JSON body. Use --mcp-config-file for larger configs.
        #[arg(long = "mcp-config")]
        mcp_config: Option<String>,
        /// Read MCP config JSON body from a file
        #[arg(long = "mcp-config-file")]
        mcp_config_file: Option<String>,
        /// MCP config strategy. Omit for default merge; use replace to ignore inherited servers.
        #[arg(long = "mcp-config-mode")]
        mcp_config_mode: Option<String>,
        /// Tool to execute
        tool: String,
        /// Arguments to the tool
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Send a Hermes ACP prompt through ahandd
    Hermes {
        /// Hermes executable path
        hermes_path: String,
        /// Prompt text. Use --prompt-file for larger prompts.
        #[arg(long)]
        prompt: Option<String>,
        /// Read prompt text from a file
        #[arg(long)]
        prompt_file: Option<String>,
        /// Working directory for the Hermes session
        #[arg(long)]
        cwd: Option<String>,
        /// Job timeout in milliseconds (0 = no timeout)
        #[arg(long, default_value = "0")]
        timeout_ms: u64,
        /// Hermes model id
        #[arg(long)]
        model: Option<String>,
        /// Resume an existing Hermes ACP session id
        #[arg(long)]
        session_id: Option<String>,
        /// Environment override in KEY=VALUE form; repeatable
        #[arg(long = "env")]
        env: Vec<String>,
        /// MCP config JSON body. Use --mcp-config-file for larger configs.
        #[arg(long = "mcp-config")]
        mcp_config: Option<String>,
        /// Read MCP config JSON body from a file
        #[arg(long = "mcp-config-file")]
        mcp_config_file: Option<String>,
        /// MCP config strategy. Omit for default merge; use replace to ignore inherited servers.
        #[arg(long = "mcp-config-mode")]
        mcp_config_mode: Option<String>,
        /// Optional AHand instructions written as AGENTS.md/AGENTS.ahand.md in cwd
        #[arg(long)]
        instructions: Option<String>,
        /// Read optional AHand instructions from a file
        #[arg(long)]
        instructions_file: Option<String>,
    },
    /// Send a Claude Code stream-json prompt through ahandd
    ClaudeCode {
        /// Claude executable path
        claude_path: String,
        /// Prompt text. Use --prompt-file for larger prompts.
        #[arg(long)]
        prompt: Option<String>,
        /// Read prompt text from a file
        #[arg(long)]
        prompt_file: Option<String>,
        /// Working directory for the Claude Code session
        #[arg(long)]
        cwd: Option<String>,
        /// Job timeout in milliseconds (0 = no timeout)
        #[arg(long, default_value = "0")]
        timeout_ms: u64,
        /// Claude model id
        #[arg(long)]
        model: Option<String>,
        /// Resume an existing Claude Code session id
        #[arg(long)]
        session_id: Option<String>,
        /// Maximum Claude turns
        #[arg(long)]
        max_turns: Option<String>,
        /// Extra system prompt appended via Claude Code
        #[arg(long)]
        system_prompt: Option<String>,
        /// Read extra system prompt from a file
        #[arg(long)]
        system_prompt_file: Option<String>,
        /// Claude permission mode, for example default or bypassPermissions
        #[arg(long)]
        permission_mode: Option<String>,
        /// Environment override in KEY=VALUE form; repeatable
        #[arg(long = "env")]
        env: Vec<String>,
        /// MCP config JSON body. Use --mcp-config-file for larger configs.
        #[arg(long = "mcp-config")]
        mcp_config: Option<String>,
        /// Read MCP config JSON body from a file
        #[arg(long = "mcp-config-file")]
        mcp_config_file: Option<String>,
        /// MCP config strategy. Omit for default merge; use replace to ignore inherited servers.
        #[arg(long = "mcp-config-mode")]
        mcp_config_mode: Option<String>,
        /// Optional AHand instructions written as CLAUDE.md/CLAUDE.ahand.md in cwd
        #[arg(long)]
        instructions: Option<String>,
        /// Read optional AHand instructions from a file
        #[arg(long)]
        instructions_file: Option<String>,
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
    /// Manage the hub device identity used by ahandd
    Identity {
        #[command(subcommand)]
        action: IdentityAction,
    },
}

#[derive(Subcommand)]
enum IdentityAction {
    /// Print deviceId and base64 publicKey for manual hub registration
    Show {
        /// Config file path. If set, reads [hub].private_key_path from this file.
        #[arg(long)]
        config: Option<String>,
        /// Explicit identity file path. Overrides --config.
        #[arg(long)]
        identity_path: Option<String>,
    },
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
            if let Cmd::Configure {
                port,
                config,
                no_open,
            } = args.command
            {
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
        Cmd::Identity { action } => match action {
            IdentityAction::Show {
                config,
                identity_path,
            } => {
                return identity::show(config.clone(), identity_path.clone());
            }
        },
        _ => {}
    }

    if let Some(ipc_path) = &args.ipc {
        // IPC mode — connect via Unix socket.
        match args.command {
            Cmd::Exec {
                execution_mode,
                cwd,
                timeout_ms,
                env,
                input_format,
                output_format,
                result_parser,
                format,
                mcp_config,
                mcp_config_file,
                mcp_config_mode,
                tool,
                args: tool_args,
            } => {
                ipc_exec(
                    ipc_path,
                    ExecRequest {
                        execution_mode: parse_execution_mode(&execution_mode)?,
                        cwd,
                        timeout_ms,
                        env: parse_env_with_mcp(env, mcp_config, mcp_config_file, mcp_config_mode)?,
                        input_format: parse_input_format(&input_format)?,
                        output_format: parse_output_format(&output_format)?,
                        result_parser: parse_result_parser(&result_parser)?,
                        format: parse_format_for_parser(&format, &result_parser)?,
                        tool,
                        args: tool_args,
                    },
                )
                .await?;
            }
            Cmd::Hermes {
                hermes_path,
                prompt,
                prompt_file,
                cwd,
                timeout_ms,
                model,
                session_id,
                env,
                mcp_config,
                mcp_config_file,
                mcp_config_mode,
                instructions,
                instructions_file,
            } => {
                ipc_exec(
                    ipc_path,
                    hermes_exec_request(HermesRunArgs {
                        hermes_path,
                        prompt,
                        prompt_file,
                        cwd,
                        timeout_ms,
                        model,
                        session_id,
                        env,
                        mcp_config,
                        mcp_config_file,
                        mcp_config_mode,
                        instructions,
                        instructions_file,
                    })?,
                )
                .await?;
            }
            Cmd::ClaudeCode {
                claude_path,
                prompt,
                prompt_file,
                cwd,
                timeout_ms,
                model,
                session_id,
                max_turns,
                system_prompt,
                system_prompt_file,
                permission_mode,
                env,
                mcp_config,
                mcp_config_file,
                mcp_config_mode,
                instructions,
                instructions_file,
            } => {
                ipc_exec(
                    ipc_path,
                    claude_code_exec_request(ClaudeCodeRunArgs {
                        claude_path,
                        prompt,
                        prompt_file,
                        cwd,
                        timeout_ms,
                        model,
                        session_id,
                        max_turns,
                        system_prompt,
                        system_prompt_file,
                        permission_mode,
                        env,
                        mcp_config,
                        mcp_config_file,
                        mcp_config_mode,
                        instructions,
                        instructions_file,
                    })?,
                )
                .await?;
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
            Cmd::Configure { .. }
            | Cmd::BrowserInit { .. }
            | Cmd::Upgrade { .. }
            | Cmd::Start { .. }
            | Cmd::Stop
            | Cmd::Restart { .. }
            | Cmd::Status
            | Cmd::Identity { .. } => {
                unreachable!("Handled early, should not reach here");
            }
        }
    } else {
        // WS mode.
        match args.command {
            Cmd::Exec {
                execution_mode,
                cwd,
                timeout_ms,
                env,
                input_format,
                output_format,
                result_parser,
                format,
                mcp_config,
                mcp_config_file,
                mcp_config_mode,
                tool,
                args: tool_args,
            } => {
                ws_exec(
                    &args.url,
                    ExecRequest {
                        execution_mode: parse_execution_mode(&execution_mode)?,
                        cwd,
                        timeout_ms,
                        env: parse_env_with_mcp(env, mcp_config, mcp_config_file, mcp_config_mode)?,
                        input_format: parse_input_format(&input_format)?,
                        output_format: parse_output_format(&output_format)?,
                        result_parser: parse_result_parser(&result_parser)?,
                        format: parse_format_for_parser(&format, &result_parser)?,
                        tool,
                        args: tool_args,
                    },
                )
                .await?;
            }
            Cmd::Hermes {
                hermes_path,
                prompt,
                prompt_file,
                cwd,
                timeout_ms,
                model,
                session_id,
                env,
                mcp_config,
                mcp_config_file,
                mcp_config_mode,
                instructions,
                instructions_file,
            } => {
                ws_exec(
                    &args.url,
                    hermes_exec_request(HermesRunArgs {
                        hermes_path,
                        prompt,
                        prompt_file,
                        cwd,
                        timeout_ms,
                        model,
                        session_id,
                        env,
                        mcp_config,
                        mcp_config_file,
                        mcp_config_mode,
                        instructions,
                        instructions_file,
                    })?,
                )
                .await?;
            }
            Cmd::ClaudeCode {
                claude_path,
                prompt,
                prompt_file,
                cwd,
                timeout_ms,
                model,
                session_id,
                max_turns,
                system_prompt,
                system_prompt_file,
                permission_mode,
                env,
                mcp_config,
                mcp_config_file,
                mcp_config_mode,
                instructions,
                instructions_file,
            } => {
                ws_exec(
                    &args.url,
                    claude_code_exec_request(ClaudeCodeRunArgs {
                        claude_path,
                        prompt,
                        prompt_file,
                        cwd,
                        timeout_ms,
                        model,
                        session_id,
                        max_turns,
                        system_prompt,
                        system_prompt_file,
                        permission_mode,
                        env,
                        mcp_config,
                        mcp_config_file,
                        mcp_config_mode,
                        instructions,
                        instructions_file,
                    })?,
                )
                .await?;
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
            Cmd::Configure { .. }
            | Cmd::BrowserInit { .. }
            | Cmd::Upgrade { .. }
            | Cmd::Start { .. }
            | Cmd::Stop
            | Cmd::Restart { .. }
            | Cmd::Status
            | Cmd::Identity { .. } => {
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

fn parse_execution_mode(value: &str) -> anyhow::Result<ExecutionMode> {
    match value {
        "batch" => Ok(ExecutionMode::Batch),
        "pty" => Ok(ExecutionMode::Pty),
        "pipe_stream" | "pipe-stream" | "stream" => Ok(ExecutionMode::PipeStream),
        _ => anyhow::bail!("invalid execution mode {value:?}; use batch, pty, or pipe_stream"),
    }
}

fn parse_env(items: Vec<String>) -> anyhow::Result<HashMap<String, String>> {
    let mut env = HashMap::new();
    for item in items {
        let Some((key, value)) = item.split_once('=') else {
            anyhow::bail!("invalid --env value {item:?}; expected KEY=VALUE");
        };
        if key.is_empty() {
            anyhow::bail!("invalid --env value {item:?}; key cannot be empty");
        }
        env.insert(key.to_string(), value.to_string());
    }
    Ok(env)
}

fn parse_env_with_mcp(
    items: Vec<String>,
    mcp_config: Option<String>,
    mcp_config_file: Option<String>,
    mcp_config_mode: Option<String>,
) -> anyhow::Result<HashMap<String, String>> {
    let mut env = parse_env(items)?;
    if let Some(mcp_config) = read_optional_mcp_config(mcp_config, mcp_config_file)? {
        env.insert("AHAND_AGENT_MCP_CONFIG".to_string(), mcp_config);
    }
    if let Some(mcp_config_mode) = parse_mcp_config_mode(mcp_config_mode)? {
        env.insert("AHAND_AGENT_MCP_CONFIG_MODE".to_string(), mcp_config_mode);
    }
    Ok(env)
}

fn parse_result_parser(value: &str) -> anyhow::Result<String> {
    let normalized = value.trim();
    if ahand_protocol::is_known_result_parser(normalized) {
        Ok(normalized.to_string())
    } else {
        anyhow::bail!(
            "invalid result parser {value:?}; use raw, codex-jsonl, claude-stream-json, or hermes"
        );
    }
}

fn parse_input_format(value: &str) -> anyhow::Result<String> {
    let normalized = value.trim();
    if ahand_protocol::is_known_input_format(normalized) {
        Ok(normalized.to_string())
    } else {
        anyhow::bail!(
            "invalid input format {value:?}; use raw, text, claude-stream-json, or hermes-acp-json-rpc"
        );
    }
}

fn parse_output_format(value: &str) -> anyhow::Result<String> {
    let normalized = value.trim();
    if ahand_protocol::is_known_output_format(normalized) {
        Ok(normalized.to_string())
    } else {
        anyhow::bail!(
            "invalid output format {value:?}; use raw, codex-jsonl, claude-stream-json, or hermes-acp-json-rpc"
        );
    }
}

fn parse_format(value: &str) -> anyhow::Result<String> {
    let normalized = value.trim();
    if ahand_protocol::is_known_format(normalized) {
        Ok(normalized.to_string())
    } else {
        anyhow::bail!("invalid format {value:?}; use raw, codex, or claude-code");
    }
}

fn parse_format_for_parser(value: &str, parser: &str) -> anyhow::Result<String> {
    let format = parse_format(value)?;
    match (format.as_str(), parser.trim()) {
        (ahand_protocol::FORMAT_CODEX, ahand_protocol::RESULT_PARSER_CODEX_JSONL)
        | (ahand_protocol::FORMAT_CLAUDE_CODE, ahand_protocol::RESULT_PARSER_CLAUDE_STREAM_JSON)
        | (ahand_protocol::FORMAT_RAW, _) => Ok(format),
        (ahand_protocol::FORMAT_CODEX, _) => {
            anyhow::bail!("format codex requires --result-parser codex-jsonl")
        }
        (ahand_protocol::FORMAT_CLAUDE_CODE, _) => {
            anyhow::bail!("format claude-code requires --result-parser claude-stream-json")
        }
        _ => Ok(format),
    }
}

fn build_job_request(job_id: String, exec: ExecRequest) -> JobRequest {
    JobRequest {
        job_id,
        tool: exec.tool,
        args: exec.args,
        cwd: exec.cwd.unwrap_or_default(),
        env: exec.env,
        timeout_ms: exec.timeout_ms,
        interactive: ahand_protocol::execution_mode_interactive_compat(exec.execution_mode),
        execution_mode: exec.execution_mode as i32,
        result_parser: exec.result_parser,
        format: exec.format,
        input_format: exec.input_format,
        output_format: exec.output_format,
    }
}

fn hermes_exec_request(args: HermesRunArgs) -> anyhow::Result<ExecRequest> {
    let prompt = read_inline_or_file(args.prompt, args.prompt_file, "prompt")?;
    let instructions =
        read_optional_inline_or_file(args.instructions, args.instructions_file, "instructions")?;
    let mcp_config = read_optional_mcp_config(args.mcp_config, args.mcp_config_file)?;
    let mcp_config_mode = parse_mcp_config_mode(args.mcp_config_mode)?;
    let mut env = parse_env(args.env)?;
    env.insert(
        "AHAND_INPUT_FORMAT".to_string(),
        ahand_protocol::INPUT_FORMAT_HERMES_ACP_JSON_RPC.to_string(),
    );
    env.insert(
        "AHAND_OUTPUT_FORMAT".to_string(),
        ahand_protocol::OUTPUT_FORMAT_HERMES_ACP_JSON_RPC.to_string(),
    );
    env.insert(
        "AHAND_AGENT_EXECUTABLE".to_string(),
        args.hermes_path.clone(),
    );
    env.insert("AHAND_AGENT_PROMPT".to_string(), prompt);
    if let Some(model) = args.model {
        env.insert("AHAND_AGENT_MODEL".to_string(), model);
    }
    if let Some(session_id) = args.session_id {
        env.insert("AHAND_AGENT_SESSION_ID".to_string(), session_id);
    }
    if let Some(instructions) = instructions {
        env.insert("AHAND_AGENT_INSTRUCTIONS".to_string(), instructions);
    }
    if let Some(mcp_config) = mcp_config {
        env.insert("AHAND_AGENT_MCP_CONFIG".to_string(), mcp_config);
    }
    if let Some(mcp_config_mode) = mcp_config_mode {
        env.insert("AHAND_AGENT_MCP_CONFIG_MODE".to_string(), mcp_config_mode);
    }

    Ok(ExecRequest {
        execution_mode: ExecutionMode::PipeStream,
        cwd: args.cwd,
        timeout_ms: args.timeout_ms,
        env,
        input_format: ahand_protocol::INPUT_FORMAT_HERMES_ACP_JSON_RPC.to_string(),
        output_format: ahand_protocol::OUTPUT_FORMAT_HERMES_ACP_JSON_RPC.to_string(),
        result_parser: ahand_protocol::RESULT_PARSER_HERMES.to_string(),
        format: ahand_protocol::FORMAT_RAW.to_string(),
        tool: args.hermes_path,
        args: Vec::new(),
    })
}

fn claude_code_exec_request(args: ClaudeCodeRunArgs) -> anyhow::Result<ExecRequest> {
    let prompt = read_inline_or_file(args.prompt, args.prompt_file, "prompt")?;
    let system_prompt =
        read_optional_inline_or_file(args.system_prompt, args.system_prompt_file, "system-prompt")?;
    let instructions =
        read_optional_inline_or_file(args.instructions, args.instructions_file, "instructions")?;
    let mcp_config = read_optional_mcp_config(args.mcp_config, args.mcp_config_file)?;
    let mcp_config_mode = parse_mcp_config_mode(args.mcp_config_mode)?;
    let mut env = parse_env(args.env)?;
    env.insert(
        "AHAND_INPUT_FORMAT".to_string(),
        ahand_protocol::INPUT_FORMAT_CLAUDE_STREAM_JSON.to_string(),
    );
    env.insert(
        "AHAND_OUTPUT_FORMAT".to_string(),
        ahand_protocol::OUTPUT_FORMAT_CLAUDE_STREAM_JSON.to_string(),
    );
    env.insert(
        "AHAND_AGENT_EXECUTABLE".to_string(),
        args.claude_path.clone(),
    );
    env.insert("AHAND_AGENT_PROMPT".to_string(), prompt);
    if let Some(model) = args.model {
        env.insert("AHAND_AGENT_MODEL".to_string(), model);
    }
    if let Some(session_id) = args.session_id {
        env.insert("AHAND_AGENT_SESSION_ID".to_string(), session_id);
    }
    if let Some(max_turns) = args.max_turns {
        env.insert("AHAND_AGENT_MAX_TURNS".to_string(), max_turns);
    }
    if let Some(system_prompt) = system_prompt {
        env.insert("AHAND_AGENT_SYSTEM_PROMPT".to_string(), system_prompt);
    }
    if let Some(permission_mode) = args.permission_mode {
        env.insert("AHAND_AGENT_PERMISSION_MODE".to_string(), permission_mode);
    }
    if let Some(instructions) = instructions {
        env.insert("AHAND_AGENT_INSTRUCTIONS".to_string(), instructions);
    }
    if let Some(mcp_config) = mcp_config {
        env.insert("AHAND_AGENT_MCP_CONFIG".to_string(), mcp_config);
    }
    if let Some(mcp_config_mode) = mcp_config_mode {
        env.insert("AHAND_AGENT_MCP_CONFIG_MODE".to_string(), mcp_config_mode);
    }

    Ok(ExecRequest {
        execution_mode: ExecutionMode::PipeStream,
        cwd: args.cwd,
        timeout_ms: args.timeout_ms,
        env,
        input_format: ahand_protocol::INPUT_FORMAT_CLAUDE_STREAM_JSON.to_string(),
        output_format: ahand_protocol::OUTPUT_FORMAT_CLAUDE_STREAM_JSON.to_string(),
        result_parser: ahand_protocol::RESULT_PARSER_CLAUDE_STREAM_JSON.to_string(),
        format: ahand_protocol::FORMAT_CLAUDE_CODE.to_string(),
        tool: args.claude_path,
        args: Vec::new(),
    })
}

fn read_inline_or_file(
    inline: Option<String>,
    file: Option<String>,
    label: &str,
) -> anyhow::Result<String> {
    match (inline, file) {
        (Some(_), Some(_)) => anyhow::bail!("use either --{label} or --{label}-file, not both"),
        (Some(value), None) if !value.trim().is_empty() => Ok(value),
        (None, Some(path)) => std::fs::read_to_string(&path)
            .map_err(|error| anyhow::anyhow!("failed to read {label} file {path}: {error}")),
        _ => anyhow::bail!("missing --{label} or --{label}-file"),
    }
}

fn read_optional_inline_or_file(
    inline: Option<String>,
    file: Option<String>,
    label: &str,
) -> anyhow::Result<Option<String>> {
    match (inline, file) {
        (Some(_), Some(_)) => anyhow::bail!("use either --{label} or --{label}-file, not both"),
        (Some(value), None) if !value.trim().is_empty() => Ok(Some(value)),
        (None, Some(path)) => std::fs::read_to_string(&path)
            .map(Some)
            .map_err(|error| anyhow::anyhow!("failed to read {label} file {path}: {error}")),
        _ => Ok(None),
    }
}

fn read_optional_mcp_config(
    inline: Option<String>,
    file: Option<String>,
) -> anyhow::Result<Option<String>> {
    let Some(raw) = read_optional_inline_or_file(inline, file, "mcp-config")? else {
        return Ok(None);
    };
    let value: serde_json::Value = serde_json::from_str(&raw)?;
    if !value.is_object() {
        anyhow::bail!("mcp-config must be a JSON object");
    }
    if let Some(servers) = value.get("mcpServers")
        && !servers.is_object()
    {
        anyhow::bail!("mcp-config mcpServers must be a JSON object");
    }
    Ok(Some(serde_json::to_string(&value)?))
}

fn parse_mcp_config_mode(value: Option<String>) -> anyhow::Result<Option<String>> {
    match value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        None => Ok(None),
        Some("replace") => Ok(Some("replace".to_string())),
        Some(other) => anyhow::bail!("invalid mcp-config-mode {other:?}; use replace"),
    }
}

fn mode_accepts_stdin(mode: ExecutionMode) -> bool {
    matches!(mode, ExecutionMode::Pty | ExecutionMode::PipeStream)
}

// ── IPC exec ─────────────────────────────────────────────────────────

async fn ipc_exec(socket_path: &str, exec: ExecRequest) -> anyhow::Result<()> {
    let stream = tokio::net::UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(&mut reader);

    let device_id = format!("ctl-{}", std::process::id());
    let job_id = format!("ctl-job-{}", std::process::id());
    let execution_mode = exec.execution_mode;
    let forwards_stdin = mode_accepts_stdin(execution_mode);

    // Send JobRequest.
    let req = Envelope {
        device_id: device_id.clone(),
        msg_id: "req-0".to_string(),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::JobRequest(build_job_request(
            job_id.clone(),
            exec,
        ))),
        ..Default::default()
    };
    write_frame(&mut writer, &req.encode_to_vec()).await?;

    let _writer_guard = if forwards_stdin {
        let stdin_job_id = job_id.clone();
        let stdin_device_id = device_id.clone();
        tokio::spawn(async move {
            let mut stdin = tokio::io::stdin();
            let mut buf = vec![0u8; 8192];
            loop {
                match stdin.read(&mut buf).await {
                    Ok(0) => {
                        let chunk = Envelope {
                            device_id: stdin_device_id.clone(),
                            msg_id: format!("stdin-{}", now_ms()),
                            ts_ms: now_ms(),
                            payload: Some(envelope::Payload::StdinChunk(StdinChunk {
                                job_id: stdin_job_id.clone(),
                                data: Vec::new(),
                            })),
                            ..Default::default()
                        };
                        let _ = write_frame(&mut writer, &chunk.encode_to_vec()).await;
                        break;
                    }
                    Ok(n) => {
                        let chunk = Envelope {
                            device_id: stdin_device_id.clone(),
                            msg_id: format!("stdin-{}", now_ms()),
                            ts_ms: now_ms(),
                            payload: Some(envelope::Payload::StdinChunk(StdinChunk {
                                job_id: stdin_job_id.clone(),
                                data: buf[..n].to_vec(),
                            })),
                            ..Default::default()
                        };
                        if write_frame(&mut writer, &chunk.encode_to_vec())
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        None
    } else {
        Some(writer)
    };

    info!(job_id = %job_id, ?execution_mode, "IPC: job submitted, waiting for output...");

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
                eprintln!(
                    "  Run `ahandctl --ipc <socket> approve` in another terminal to approve."
                );
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
            hostname: gethostname::gethostname().to_string_lossy().to_string(),
            os: std::env::consts::OS.to_string(),
            capabilities: vec!["ctl".to_string()],
            last_ack: 0,
            auth: None,
        })),
        ..Default::default()
    };

    sink.send(tungstenite::Message::Binary(hello.encode_to_vec()))
        .await?;

    Ok((sink, stream, device_id))
}

async fn ws_exec(url: &str, exec: ExecRequest) -> anyhow::Result<()> {
    let (mut sink, mut stream, device_id) = connect_and_hello(url).await?;

    let job_id = format!("ctl-job-{}", std::process::id());
    let execution_mode = exec.execution_mode;
    let forwards_stdin = mode_accepts_stdin(execution_mode);

    let req = Envelope {
        device_id: device_id.clone(),
        msg_id: "req-0".to_string(),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::JobRequest(build_job_request(
            job_id.clone(),
            exec,
        ))),
        ..Default::default()
    };

    sink.send(tungstenite::Message::Binary(req.encode_to_vec()))
        .await?;

    if forwards_stdin {
        let stdin_job_id = job_id.clone();
        let stdin_device_id = device_id.clone();
        tokio::spawn(async move {
            let mut stdin = tokio::io::stdin();
            let mut buf = vec![0u8; 8192];
            loop {
                match stdin.read(&mut buf).await {
                    Ok(0) => {
                        let chunk = Envelope {
                            device_id: stdin_device_id.clone(),
                            msg_id: format!("stdin-{}", now_ms()),
                            ts_ms: now_ms(),
                            payload: Some(envelope::Payload::StdinChunk(StdinChunk {
                                job_id: stdin_job_id.clone(),
                                data: Vec::new(),
                            })),
                            ..Default::default()
                        };
                        let _ = sink
                            .send(tungstenite::Message::Binary(chunk.encode_to_vec()))
                            .await;
                        break;
                    }
                    Ok(n) => {
                        let chunk = Envelope {
                            device_id: stdin_device_id.clone(),
                            msg_id: format!("stdin-{}", now_ms()),
                            ts_ms: now_ms(),
                            payload: Some(envelope::Payload::StdinChunk(StdinChunk {
                                job_id: stdin_job_id.clone(),
                                data: buf[..n].to_vec(),
                            })),
                            ..Default::default()
                        };
                        if sink
                            .send(tungstenite::Message::Binary(chunk.encode_to_vec()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }

    info!(job_id = %job_id, ?execution_mode, "job submitted, waiting for output...");

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
            eprintln!(
                "[approval] Job {} (from {}) wants to run: {} {}",
                req.job_id,
                req.caller_uid,
                req.tool,
                req.args.join(" ")
            );
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
                eprintln!(
                    "[approval] Approved job {}{}",
                    req.job_id,
                    if remember { " (remembered)" } else { "" }
                );
            } else if reason.is_empty() {
                eprintln!("[approval] Denied job {}", req.job_id);
            } else {
                eprintln!(
                    "[approval] Denied job {} with reason: {}",
                    req.job_id, reason
                );
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

    sink.send(tungstenite::Message::Binary(request_env.encode_to_vec()))
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
        SessionAction::Set {
            mode,
            caller,
            timeout,
        } => {
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
