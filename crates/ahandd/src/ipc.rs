use std::path::PathBuf;
use std::sync::Arc;

use ahand_protocol::{BrowserResponse, Envelope, JobFinished, JobRejected, SessionMode, envelope};
use prost::Message;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info, warn};

use crate::approval::ApprovalManager;
use crate::browser::BrowserManager;
use crate::executor;
use crate::registry::{IsKnown, JobRegistry};
use crate::session::{SessionDecision, SessionManager};
use crate::store::RunStore;

/// Start the IPC server on the given socket path.
#[allow(clippy::too_many_arguments)]
pub async fn serve_ipc(
    socket_path: PathBuf,
    #[allow(unused_variables)] socket_mode: u32,
    registry: Arc<JobRegistry>,
    store: Option<Arc<RunStore>>,
    session_mgr: Arc<SessionManager>,
    approval_mgr: Arc<ApprovalManager>,
    approval_broadcast_tx: broadcast::Sender<Envelope>,
    device_id: String,
    browser_mgr: Arc<BrowserManager>,
) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        serve_ipc_unix(
            socket_path, socket_mode, registry, store, session_mgr,
            approval_mgr, approval_broadcast_tx, device_id, browser_mgr,
        ).await
    }
    #[cfg(windows)]
    {
        serve_ipc_windows(
            socket_path, registry, store, session_mgr,
            approval_mgr, approval_broadcast_tx, device_id, browser_mgr,
        ).await
    }
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
async fn serve_ipc_unix(
    socket_path: PathBuf,
    socket_mode: u32,
    registry: Arc<JobRegistry>,
    store: Option<Arc<RunStore>>,
    session_mgr: Arc<SessionManager>,
    approval_mgr: Arc<ApprovalManager>,
    approval_broadcast_tx: broadcast::Sender<Envelope>,
    device_id: String,
    browser_mgr: Arc<BrowserManager>,
) -> anyhow::Result<()> {
    use tokio::net::UnixListener;

    // Remove stale socket file if it exists.
    let _ = std::fs::remove_file(&socket_path);

    // Ensure parent directory exists.
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(&socket_path)?;

    // Set socket permissions.
    crate::fs_perms::restrict_owner_and_group(&socket_path)?;

    info!(path = %socket_path.display(), mode = format!("{:04o}", socket_mode), "IPC server listening");

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let caller_uid = match stream.peer_cred() {
                    Ok(cred) => format!("uid:{}", cred.uid()),
                    Err(e) => {
                        warn!(error = %e, "IPC: failed to get peer credentials");
                        "uid:unknown".to_string()
                    }
                };

                let (reader, writer) = stream.into_split();
                spawn_ipc_handler(
                    reader, writer,
                    Arc::clone(&registry), store.clone(),
                    Arc::clone(&session_mgr), Arc::clone(&approval_mgr),
                    approval_broadcast_tx.clone(), device_id.clone(),
                    caller_uid, Arc::clone(&browser_mgr),
                );
            }
            Err(e) => {
                error!(error = %e, "IPC accept error");
            }
        }
    }
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
async fn serve_ipc_windows(
    socket_path: PathBuf,
    registry: Arc<JobRegistry>,
    store: Option<Arc<RunStore>>,
    session_mgr: Arc<SessionManager>,
    approval_mgr: Arc<ApprovalManager>,
    approval_broadcast_tx: broadcast::Sender<Envelope>,
    device_id: String,
    browser_mgr: Arc<BrowserManager>,
) -> anyhow::Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;

    let pipe_name = socket_path.to_string_lossy().to_string();
    info!(path = %pipe_name, "IPC server listening (Named Pipe)");

    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&pipe_name)?;

    loop {
        server.connect().await?;
        let connected = server;
        server = ServerOptions::new().create(&pipe_name)?;

        let caller_uid = get_pipe_caller_identity(&connected);
        let (reader, writer) = tokio::io::split(connected);
        spawn_ipc_handler(
            reader, writer,
            Arc::clone(&registry), store.clone(),
            Arc::clone(&session_mgr), Arc::clone(&approval_mgr),
            approval_broadcast_tx.clone(), device_id.clone(),
            caller_uid, Arc::clone(&browser_mgr),
        );
    }
}

#[cfg(windows)]
fn get_pipe_caller_identity(pipe: &tokio::net::windows::named_pipe::NamedPipeServer) -> String {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Security::*;
    use windows_sys::Win32::System::Pipes::GetNamedPipeClientProcessId;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    unsafe {
        let handle = pipe.as_raw_handle() as isize;
        let mut pid = 0u32;
        if GetNamedPipeClientProcessId(handle, &mut pid) == 0 {
            return "user:unknown".to_string();
        }

        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process == 0 {
            return format!("pid:{pid}");
        }

        let mut token = 0isize;
        if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
            CloseHandle(process);
            return format!("pid:{pid}");
        }

        let mut info_len = 0u32;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut info_len);
        let mut buffer = vec![0u8; info_len as usize];
        if GetTokenInformation(
            token, TokenUser, buffer.as_mut_ptr() as *mut _,
            info_len, &mut info_len,
        ) == 0 {
            CloseHandle(token);
            CloseHandle(process);
            return format!("pid:{pid}");
        }

        let token_user = &*(buffer.as_ptr() as *const TOKEN_USER);
        let sid = token_user.User.Sid;

        let mut name_buf = [0u16; 256];
        let mut name_len = 256u32;
        let mut domain_buf = [0u16; 256];
        let mut domain_len = 256u32;
        let mut sid_type = 0;

        if LookupAccountSidW(
            std::ptr::null(), sid,
            name_buf.as_mut_ptr(), &mut name_len,
            domain_buf.as_mut_ptr(), &mut domain_len,
            &mut sid_type,
        ) == 0 {
            CloseHandle(token);
            CloseHandle(process);
            return format!("pid:{pid}");
        }

        CloseHandle(token);
        CloseHandle(process);

        let username = String::from_utf16_lossy(&name_buf[..name_len as usize]);
        format!("user:{username}")
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_ipc_handler<R, W>(
    reader: R,
    writer: W,
    registry: Arc<JobRegistry>,
    store: Option<Arc<RunStore>>,
    session_mgr: Arc<SessionManager>,
    approval_mgr: Arc<ApprovalManager>,
    approval_broadcast_tx: broadcast::Sender<Envelope>,
    device_id: String,
    caller_uid: String,
    browser_mgr: Arc<BrowserManager>,
) where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        if let Err(e) = handle_ipc_conn(
            reader, writer, registry, store, session_mgr,
            approval_mgr, approval_broadcast_tx, device_id,
            caller_uid, browser_mgr,
        ).await {
            warn!(error = %e, "IPC connection error");
        }
    });
}

#[allow(clippy::too_many_arguments)]
async fn handle_ipc_conn<R, W>(
    reader: R,
    writer: W,
    registry: Arc<JobRegistry>,
    store: Option<Arc<RunStore>>,
    session_mgr: Arc<SessionManager>,
    approval_mgr: Arc<ApprovalManager>,
    approval_broadcast_tx: broadcast::Sender<Envelope>,
    device_id: String,
    caller_uid: String,
    browser_mgr: Arc<BrowserManager>,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut reader = tokio::io::BufReader::new(reader);

    info!(caller_uid = %caller_uid, "IPC: new connection");

    // Register the IPC caller so session queries return it.
    session_mgr.register_caller(&caller_uid).await;

    // Channel for sending responses back through the IPC stream.
    let (tx, mut rx) = mpsc::unbounded_channel::<Envelope>();

    // Subscribe to the approval broadcast channel.
    let mut approval_rx = approval_broadcast_tx.subscribe();

    // Task: forward outgoing envelopes and broadcast approval requests to the IPC stream.
    let send_handle = tokio::spawn(async move {
        let mut writer = writer;
        loop {
            tokio::select! {
                msg = rx.recv() => {
                    match msg {
                        Some(envelope) => {
                            let data = envelope.encode_to_vec();
                            if write_frame(&mut writer, &data).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
                bcast = approval_rx.recv() => {
                    match bcast {
                        Ok(envelope) => {
                            let data = envelope.encode_to_vec();
                            if write_frame(&mut writer, &data).await.is_err() {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!(missed = n, "IPC: broadcast lagged, missed messages");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    });

    // Read frames from the IPC stream.
    loop {
        let data = match read_frame(&mut reader).await {
            Ok(d) => d,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    // Client disconnected.
                    break;
                }
                warn!(error = %e, "IPC read error");
                break;
            }
        };

        let envelope = match Envelope::decode(data.as_slice()) {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "IPC: failed to decode envelope");
                continue;
            }
        };

        match envelope.payload {
            Some(envelope::Payload::JobRequest(req)) => {
                // Idempotency check.
                match registry.is_known(&req.job_id).await {
                    IsKnown::Running => {
                        warn!(job_id = %req.job_id, "IPC: duplicate job_id, already running");
                        continue;
                    }
                    IsKnown::Completed(c) => {
                        info!(job_id = %req.job_id, "IPC: duplicate job_id, returning cached result");
                        let finished_env = Envelope {
                            device_id: device_id.clone(),
                            msg_id: new_msg_id(),
                            ts_ms: now_ms(),
                            payload: Some(envelope::Payload::JobFinished(JobFinished {
                                job_id: req.job_id.clone(),
                                exit_code: c.exit_code,
                                error: c.error,
                            })),
                            ..Default::default()
                        };
                        let _ = tx.send(finished_env);
                        continue;
                    }
                    IsKnown::Unknown => {}
                }

                // Session mode check.
                match session_mgr.check(&req, &caller_uid).await {
                    SessionDecision::Deny(reason) => {
                        warn!(job_id = %req.job_id, reason = %reason, "IPC: job rejected by session mode");
                        let reject_env = Envelope {
                            device_id: device_id.clone(),
                            msg_id: new_msg_id(),
                            ts_ms: now_ms(),
                            payload: Some(envelope::Payload::JobRejected(JobRejected {
                                job_id: req.job_id.clone(),
                                reason,
                            })),
                            ..Default::default()
                        };
                        let _ = tx.send(reject_env);
                    }
                    SessionDecision::Allow => {
                        let job_id = req.job_id.clone();
                        let tx_clone = tx.clone();
                        let did = device_id.clone();
                        let reg = Arc::clone(&registry);
                        let st = store.clone();

                        let (cancel_tx, cancel_rx) = mpsc::channel(1);
                        reg.register(job_id.clone(), cancel_tx).await;

                        let active = reg.active_count().await;
                        info!(job_id = %job_id, active_jobs = active, "IPC: job accepted");

                        tokio::spawn(async move {
                            let _permit = reg.acquire_permit().await;
                            let (exit_code, error) =
                                executor::run_job(did, req, tx_clone, cancel_rx, st).await;
                            reg.remove(&job_id).await;
                            reg.mark_completed(job_id, exit_code, error).await;
                        });
                    }
                    SessionDecision::NeedsApproval {
                        reason,
                        previous_refusals,
                    } => {
                        info!(job_id = %req.job_id, reason = %reason, "IPC: job needs approval (strict mode)");

                        let (approval_req, approval_rx) = approval_mgr
                            .submit(req.clone(), &caller_uid, reason, previous_refusals)
                            .await;

                        // Send ApprovalRequest to this IPC client.
                        let approval_env = Envelope {
                            device_id: device_id.clone(),
                            msg_id: new_msg_id(),
                            ts_ms: now_ms(),
                            payload: Some(envelope::Payload::ApprovalRequest(approval_req.clone())),
                            ..Default::default()
                        };
                        let _ = tx.send(approval_env.clone());

                        // Also broadcast to other IPC clients.
                        let _ = approval_broadcast_tx.send(approval_env);

                        // Spawn a task to wait for approval.
                        let tx_clone = tx.clone();
                        let did = device_id.clone();
                        let reg = Arc::clone(&registry);
                        let st = store.clone();
                        let amgr = Arc::clone(&approval_mgr);
                        let smgr = Arc::clone(&session_mgr);
                        let timeout = amgr.default_timeout();
                        let job_id = req.job_id.clone();
                        let cuid = caller_uid.clone();

                        tokio::spawn(async move {
                            let result = tokio::time::timeout(timeout, approval_rx).await;
                            match result {
                                Ok(Ok(resp)) if resp.approved => {
                                    info!(job_id = %job_id, "IPC: approval granted");
                                    let (cancel_tx, cancel_rx) = mpsc::channel(1);
                                    reg.register(job_id.clone(), cancel_tx).await;
                                    let _permit = reg.acquire_permit().await;
                                    let (exit_code, error) =
                                        executor::run_job(did, req, tx_clone, cancel_rx, st).await;
                                    reg.remove(&job_id).await;
                                    reg.mark_completed(job_id, exit_code, error).await;
                                }
                                Ok(Ok(resp)) => {
                                    info!(job_id = %job_id, "IPC: approval denied");
                                    if !resp.reason.is_empty() {
                                        smgr.record_refusal(&cuid, &req.tool, &resp.reason).await;
                                    }
                                    amgr.expire(&job_id).await;
                                    let reject_env = Envelope {
                                        device_id: did,
                                        msg_id: new_msg_id(),
                                        ts_ms: now_ms(),
                                        payload: Some(envelope::Payload::JobRejected(
                                            JobRejected {
                                                job_id,
                                                reason: if resp.reason.is_empty() {
                                                    "approval denied".to_string()
                                                } else {
                                                    format!("approval denied: {}", resp.reason)
                                                },
                                            },
                                        )),
                                        ..Default::default()
                                    };
                                    let _ = tx_clone.send(reject_env);
                                }
                                _ => {
                                    info!(job_id = %job_id, "IPC: approval timed out");
                                    amgr.expire(&job_id).await;
                                    let reject_env = Envelope {
                                        device_id: did,
                                        msg_id: new_msg_id(),
                                        ts_ms: now_ms(),
                                        payload: Some(envelope::Payload::JobRejected(
                                            JobRejected {
                                                job_id,
                                                reason: "approval timed out".to_string(),
                                            },
                                        )),
                                        ..Default::default()
                                    };
                                    let _ = tx_clone.send(reject_env);
                                }
                            }
                        });
                    }
                }
            }
            Some(envelope::Payload::CancelJob(cancel)) => {
                info!(job_id = %cancel.job_id, "IPC: received cancel request");
                registry.cancel(&cancel.job_id).await;
            }
            Some(envelope::Payload::ApprovalResponse(resp)) => {
                info!(job_id = %resp.job_id, approved = resp.approved, "IPC: received approval response");
                if !resp.approved && !resp.reason.is_empty() {
                    if let Some((req, _)) = approval_mgr.resolve(&resp).await {
                        session_mgr
                            .record_refusal(&caller_uid, &req.tool, &resp.reason)
                            .await;
                    }
                } else {
                    approval_mgr.resolve(&resp).await;
                }
            }
            Some(envelope::Payload::SetSessionMode(msg)) => {
                let mode = SessionMode::try_from(msg.mode).unwrap_or(SessionMode::Inactive);
                info!(caller_uid = %msg.caller_uid, ?mode, "IPC: received set session mode");
                let state = session_mgr
                    .set_mode(&msg.caller_uid, mode, msg.trust_timeout_mins)
                    .await;
                let state_env = Envelope {
                    device_id: device_id.clone(),
                    msg_id: new_msg_id(),
                    ts_ms: now_ms(),
                    payload: Some(envelope::Payload::SessionState(state)),
                    ..Default::default()
                };
                let _ = tx.send(state_env);
            }
            Some(envelope::Payload::SessionQuery(query)) => {
                info!(caller_uid = %query.caller_uid, "IPC: received session query");
                let states = session_mgr.query_sessions(&query.caller_uid).await;
                for state in states {
                    let state_env = Envelope {
                        device_id: device_id.clone(),
                        msg_id: new_msg_id(),
                        ts_ms: now_ms(),
                        payload: Some(envelope::Payload::SessionState(state)),
                        ..Default::default()
                    };
                    let _ = tx.send(state_env);
                }
            }
            Some(envelope::Payload::BrowserRequest(req)) => {
                info!(
                    request_id = %req.request_id,
                    action = %req.action,
                    "IPC: received browser request"
                );

                if !browser_mgr.is_enabled() {
                    let resp_env = Envelope {
                        device_id: device_id.clone(),
                        msg_id: new_msg_id(),
                        ts_ms: now_ms(),
                        payload: Some(envelope::Payload::BrowserResponse(BrowserResponse {
                            request_id: req.request_id.clone(),
                            session_id: req.session_id.clone(),
                            success: false,
                            error: "browser capabilities not enabled".to_string(),
                            ..Default::default()
                        })),
                        ..Default::default()
                    };
                    let _ = tx.send(resp_env);
                } else if let Err(reason) = browser_mgr.check_domain(&req.action, &req.params_json)
                {
                    let resp_env = Envelope {
                        device_id: device_id.clone(),
                        msg_id: new_msg_id(),
                        ts_ms: now_ms(),
                        payload: Some(envelope::Payload::BrowserResponse(BrowserResponse {
                            request_id: req.request_id.clone(),
                            session_id: req.session_id.clone(),
                            success: false,
                            error: reason,
                            ..Default::default()
                        })),
                        ..Default::default()
                    };
                    let _ = tx.send(resp_env);
                } else {
                    let bmgr = Arc::clone(&browser_mgr);
                    let did = device_id.clone();
                    let tx_clone = tx.clone();
                    tokio::spawn(async move {
                        let result = bmgr
                            .execute(
                                &req.session_id,
                                &req.action,
                                &req.params_json,
                                req.timeout_ms,
                            )
                            .await;
                        let resp = match result {
                            Ok(r) => BrowserResponse {
                                request_id: req.request_id.clone(),
                                session_id: req.session_id.clone(),
                                success: r.success,
                                result_json: r.result_json,
                                error: r.error,
                                binary_data: r.binary_data,
                                binary_mime: r.binary_mime,
                            },
                            Err(e) => BrowserResponse {
                                request_id: req.request_id.clone(),
                                session_id: req.session_id.clone(),
                                success: false,
                                error: format!("browser command failed: {}", e),
                                ..Default::default()
                            },
                        };
                        if req.action == "close" {
                            bmgr.release_session(&req.session_id).await;
                        }
                        let resp_env = Envelope {
                            device_id: did,
                            msg_id: new_msg_id(),
                            ts_ms: now_ms(),
                            payload: Some(envelope::Payload::BrowserResponse(resp)),
                            ..Default::default()
                        };
                        let _ = tx_clone.send(resp_env);
                    });
                }
            }
            _ => {}
        }
    }

    drop(tx);
    let _ = send_handle.await;
    Ok(())
}

/// Read a length-prefixed frame: [4 bytes big-endian u32 length][N bytes payload].
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

/// Write a length-prefixed frame.
async fn write_frame<W: AsyncWriteExt + Unpin>(writer: &mut W, data: &[u8]) -> std::io::Result<()> {
    writer.write_u32(data.len() as u32).await?;
    writer.write_all(data).await?;
    writer.flush().await?;
    Ok(())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn new_msg_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!("ipc-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    #[tokio::test]
    async fn test_named_pipe_roundtrip() {
        use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let pipe_name = format!(r"\\.\pipe\ahand-test-{}", std::process::id());

        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&pipe_name)
            .unwrap();

        let server_task = tokio::spawn(async move {
            server.connect().await.unwrap();
            let (mut reader, mut writer) = tokio::io::split(server);

            // Read frame
            let len = reader.read_u32().await.unwrap() as usize;
            let mut buf = vec![0u8; len];
            reader.read_exact(&mut buf).await.unwrap();
            assert_eq!(buf, b"hello pipe");

            // Write frame back
            writer.write_u32(5).await.unwrap();
            writer.write_all(b"world").await.unwrap();
            writer.flush().await.unwrap();
        });

        // Brief delay for server to start
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let client = ClientOptions::new().open(&pipe_name).unwrap();
        let (mut reader, mut writer) = tokio::io::split(client);

        // Send frame
        writer.write_u32(10).await.unwrap();
        writer.write_all(b"hello pipe").await.unwrap();
        writer.flush().await.unwrap();

        // Read response
        let len = reader.read_u32().await.unwrap() as usize;
        let mut buf = vec![0u8; len];
        reader.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, b"world");

        server_task.await.unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn test_get_pipe_caller_identity_format() {
        // Will be tested when we have a connected pipe in the full integration test
        // For now, verify the function exists and compiles
    }

    #[test]
    fn test_now_ms_returns_nonzero() {
        let ts = super::now_ms();
        assert!(ts > 0);
    }

    #[test]
    fn test_new_msg_id_unique() {
        let a = super::new_msg_id();
        let b = super::new_msg_id();
        assert_ne!(a, b);
        assert!(a.starts_with("ipc-"));
    }
}
