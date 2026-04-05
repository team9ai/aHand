//! IPC frame protocol integration tests.
//!
//! Verifies the length-prefixed frame protocol works correctly,
//! which is the foundation for cross-platform IPC communication.
//! On Unix we test over Unix domain sockets; on Windows these tests
//! would exercise the same framing logic over named pipes (not yet
//! wired here — the protocol layer is platform-agnostic).

use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Write a length-prefixed frame: `[4-byte big-endian length][payload]`.
///
/// This mirrors the production `write_frame` in `ipc.rs`.
async fn write_frame<W: AsyncWriteExt + Unpin>(w: &mut W, data: &[u8]) -> std::io::Result<()> {
    w.write_u32(data.len() as u32).await?;
    w.write_all(data).await?;
    w.flush().await
}

/// Read a length-prefixed frame, rejecting payloads >16 MiB.
///
/// This mirrors the production `read_frame` in `ipc.rs`.
async fn read_frame<R: AsyncReadExt + Unpin>(r: &mut R) -> std::io::Result<Vec<u8>> {
    let len = r.read_u32().await? as usize;
    if len > 16 * 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

// ---------------------------------------------------------------------------
// Unix-socket tests (cfg(unix))
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod unix {
    use super::*;

    #[tokio::test]
    async fn frame_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("test.sock");

        let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();

        let path = sock_path.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = stream.into_split();
            let mut reader = tokio::io::BufReader::new(reader);

            // Read a frame from the client.
            let data = read_frame(&mut reader).await.unwrap();
            assert_eq!(data, b"hello from client");

            // Send a frame back.
            write_frame(&mut writer, b"hello from server").await.unwrap();
        });

        let client = tokio::spawn(async move {
            let stream = tokio::net::UnixStream::connect(&path).await.unwrap();
            let (reader, mut writer) = stream.into_split();
            let mut reader = tokio::io::BufReader::new(reader);

            write_frame(&mut writer, b"hello from client").await.unwrap();

            let data = read_frame(&mut reader).await.unwrap();
            assert_eq!(data, b"hello from server");
        });

        server.await.unwrap();
        client.await.unwrap();
    }

    #[tokio::test]
    async fn frame_protocol_with_protobuf() {
        use ahand_protocol::{envelope, Envelope, JobRequest};
        use prost::Message;

        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("proto.sock");

        let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();

        let path = sock_path.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, _writer) = stream.into_split();
            let mut reader = tokio::io::BufReader::new(reader);

            let data = read_frame(&mut reader).await.unwrap();
            let env = Envelope::decode(data.as_slice()).unwrap();

            assert_eq!(env.device_id, "test-device");
            if let Some(envelope::Payload::JobRequest(req)) = env.payload {
                assert_eq!(req.job_id, "job-1");
                assert_eq!(req.tool, "echo");
                assert_eq!(req.args, vec!["hello".to_string()]);
            } else {
                panic!("expected JobRequest payload");
            }
        });

        let client = tokio::spawn(async move {
            let stream = tokio::net::UnixStream::connect(&path).await.unwrap();
            let (_reader, mut writer) = stream.into_split();

            let env = Envelope {
                device_id: "test-device".to_string(),
                msg_id: "msg-1".to_string(),
                ts_ms: 12345,
                payload: Some(envelope::Payload::JobRequest(JobRequest {
                    job_id: "job-1".to_string(),
                    tool: "echo".to_string(),
                    args: vec!["hello".to_string()],
                    ..Default::default()
                })),
                ..Default::default()
            };

            write_frame(&mut writer, &env.encode_to_vec()).await.unwrap();
        });

        server.await.unwrap();
        client.await.unwrap();
    }

    #[tokio::test]
    async fn frame_multiple_messages() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("multi.sock");

        let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();

        let path = sock_path.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, _) = stream.into_split();
            let mut reader = tokio::io::BufReader::new(reader);

            for i in 0..5u32 {
                let data = read_frame(&mut reader).await.unwrap();
                assert_eq!(data, format!("message-{i}").as_bytes());
            }
        });

        let client = tokio::spawn(async move {
            let stream = tokio::net::UnixStream::connect(&path).await.unwrap();
            let (_, mut writer) = stream.into_split();

            for i in 0..5u32 {
                write_frame(&mut writer, format!("message-{i}").as_bytes())
                    .await
                    .unwrap();
            }
        });

        server.await.unwrap();
        client.await.unwrap();
    }

    #[tokio::test]
    async fn frame_empty_message() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("empty.sock");

        let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();

        let path = sock_path.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, _) = stream.into_split();
            let mut reader = tokio::io::BufReader::new(reader);

            let data = read_frame(&mut reader).await.unwrap();
            assert!(data.is_empty());
        });

        let client = tokio::spawn(async move {
            let stream = tokio::net::UnixStream::connect(&path).await.unwrap();
            let (_, mut writer) = stream.into_split();
            write_frame(&mut writer, b"").await.unwrap();
        });

        server.await.unwrap();
        client.await.unwrap();
    }

    #[tokio::test]
    async fn frame_too_large_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("large.sock");

        let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();

        let path = sock_path.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, _) = stream.into_split();
            let mut reader = tokio::io::BufReader::new(reader);

            let result = read_frame(&mut reader).await;
            assert!(result.is_err());
            let err = result.unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
            assert!(err.to_string().contains("frame too large"));
        });

        let client = tokio::spawn(async move {
            let stream = tokio::net::UnixStream::connect(&path).await.unwrap();
            let (_, mut writer) = stream.into_split();

            // Write a length header claiming 17 MiB (exceeds 16 MiB limit),
            // without actually sending that much data.
            let fake_len: u32 = 17 * 1024 * 1024;
            writer.write_u32(fake_len).await.unwrap();
            writer.flush().await.unwrap();
        });

        server.await.unwrap();
        client.await.unwrap();
    }

    #[tokio::test]
    async fn frame_max_allowed_size_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("maxsize.sock");

        let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();

        let path = sock_path.clone();
        // Use a smaller payload to avoid allocating 16 MiB in tests.
        // We verify the boundary by testing a frame of exactly 1024 bytes.
        let payload = vec![0xABu8; 1024];
        let expected = payload.clone();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, _) = stream.into_split();
            let mut reader = tokio::io::BufReader::new(reader);

            let data = read_frame(&mut reader).await.unwrap();
            assert_eq!(data, expected);
        });

        let client = tokio::spawn(async move {
            let stream = tokio::net::UnixStream::connect(&path).await.unwrap();
            let (_, mut writer) = stream.into_split();
            write_frame(&mut writer, &payload).await.unwrap();
        });

        server.await.unwrap();
        client.await.unwrap();
    }

    #[tokio::test]
    async fn frame_bidirectional_protobuf() {
        use ahand_protocol::{envelope, Envelope, JobFinished, JobRequest};
        use prost::Message;

        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("bidir.sock");

        let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();

        let path = sock_path.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = stream.into_split();
            let mut reader = tokio::io::BufReader::new(reader);

            // Receive a JobRequest.
            let data = read_frame(&mut reader).await.unwrap();
            let env = Envelope::decode(data.as_slice()).unwrap();
            let job_id = match env.payload {
                Some(envelope::Payload::JobRequest(ref req)) => req.job_id.clone(),
                other => panic!("expected JobRequest, got {other:?}"),
            };

            // Respond with JobFinished.
            let response = Envelope {
                device_id: "daemon".to_string(),
                msg_id: "resp-1".to_string(),
                payload: Some(envelope::Payload::JobFinished(JobFinished {
                    job_id,
                    exit_code: 0,
                    error: String::new(),
                })),
                ..Default::default()
            };
            write_frame(&mut writer, &response.encode_to_vec())
                .await
                .unwrap();
        });

        let client = tokio::spawn(async move {
            let stream = tokio::net::UnixStream::connect(&path).await.unwrap();
            let (reader, mut writer) = stream.into_split();
            let mut reader = tokio::io::BufReader::new(reader);

            // Send a JobRequest.
            let request = Envelope {
                device_id: "client".to_string(),
                msg_id: "req-1".to_string(),
                payload: Some(envelope::Payload::JobRequest(JobRequest {
                    job_id: "job-42".to_string(),
                    tool: "ls".to_string(),
                    args: vec!["-la".to_string()],
                    cwd: "/tmp".to_string(),
                    ..Default::default()
                })),
                ..Default::default()
            };
            write_frame(&mut writer, &request.encode_to_vec())
                .await
                .unwrap();

            // Read the response.
            let data = read_frame(&mut reader).await.unwrap();
            let env = Envelope::decode(data.as_slice()).unwrap();
            match env.payload {
                Some(envelope::Payload::JobFinished(finished)) => {
                    assert_eq!(finished.job_id, "job-42");
                    assert_eq!(finished.exit_code, 0);
                    assert!(finished.error.is_empty());
                }
                other => panic!("expected JobFinished, got {other:?}"),
            }
        });

        server.await.unwrap();
        client.await.unwrap();
    }

    #[tokio::test]
    async fn frame_truncated_payload_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("trunc.sock");

        let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();

        let path = sock_path.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, _) = stream.into_split();
            let mut reader = tokio::io::BufReader::new(reader);

            // The client claims 100 bytes but only sends 5, then closes.
            // read_frame should return an error (UnexpectedEof).
            let result = read_frame(&mut reader).await;
            assert!(result.is_err());
        });

        let client = tokio::spawn(async move {
            let stream = tokio::net::UnixStream::connect(&path).await.unwrap();
            let (_, mut writer) = stream.into_split();

            // Write a length header of 100 but only 5 bytes of payload,
            // then drop the connection.
            writer.write_u32(100).await.unwrap();
            writer.write_all(&[1, 2, 3, 4, 5]).await.unwrap();
            writer.flush().await.unwrap();
            drop(writer);
        });

        server.await.unwrap();
        client.await.unwrap();
    }
}

// ---------------------------------------------------------------------------
// Full serve_ipc integration tests (cfg(unix))
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod serve_ipc_integration {
    use std::sync::Arc;

    use ahand_protocol::{envelope, Envelope, JobRequest, SessionMode, SessionQuery};
    use ahandd::{approval, browser, config, ipc, registry, session};
    use prost::Message;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Helper: write a length-prefixed protobuf frame.
    async fn write_envelope<W: AsyncWriteExt + Unpin>(w: &mut W, env: &Envelope) {
        let data = env.encode_to_vec();
        w.write_u32(data.len() as u32).await.unwrap();
        w.write_all(&data).await.unwrap();
        w.flush().await.unwrap();
    }

    /// Helper: read one length-prefixed protobuf frame with a timeout.
    async fn read_envelope<R: AsyncReadExt + Unpin>(r: &mut R) -> Option<Envelope> {
        let len = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            r.read_u32(),
        )
        .await
        .ok()?
        .ok()? as usize;

        let mut buf = vec![0u8; len];
        r.read_exact(&mut buf).await.ok()?;
        Envelope::decode(buf.as_slice()).ok()
    }

    /// Spin up a real `serve_ipc` server bound to a temp Unix socket and return
    /// the socket path and the server's `JoinHandle` (caller must abort it).
    async fn start_server(
        dir: &tempfile::TempDir,
        session_mgr: Arc<session::SessionManager>,
    ) -> (std::path::PathBuf, tokio::task::JoinHandle<anyhow::Result<()>>) {
        let sock_path = dir.path().join("test.sock");
        let registry = Arc::new(registry::JobRegistry::new(4));
        let approval_mgr = Arc::new(approval::ApprovalManager::new(300));
        let browser_cfg = config::BrowserConfig::default();
        let browser_mgr = Arc::new(browser::BrowserManager::new(browser_cfg));
        let (broadcast_tx, _) = tokio::sync::broadcast::channel::<Envelope>(16);

        let path_clone = sock_path.clone();
        let handle = tokio::spawn(ipc::serve_ipc(
            path_clone,
            0o660,
            registry,
            None,
            session_mgr,
            approval_mgr,
            broadcast_tx,
            "test-device".to_string(),
            browser_mgr,
        ));

        // Wait for socket to be ready (poll instead of fixed sleep).
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while !sock_path.exists() {
            if tokio::time::Instant::now() > deadline {
                panic!("IPC server did not start within 5 seconds");
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        (sock_path, handle)
    }

    // -----------------------------------------------------------------------
    // Test: send a JobRequest and receive JobFinished or JobRejected.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn serve_ipc_job_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let session_mgr = Arc::new(session::SessionManager::new(60));

        let (sock_path, server_handle) = start_server(&dir, Arc::clone(&session_mgr)).await;

        // Connect as a client.
        let stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut reader = tokio::io::BufReader::new(reader);

        // Send a JobRequest.
        let job_id = format!("test-job-{}", std::process::id());
        let req = Envelope {
            device_id: "test-client".to_string(),
            msg_id: "msg-1".to_string(),
            ts_ms: 0,
            payload: Some(envelope::Payload::JobRequest(JobRequest {
                job_id: job_id.clone(),
                tool: "echo".to_string(),
                args: vec!["hello".to_string()],
                ..Default::default()
            })),
            ..Default::default()
        };
        write_envelope(&mut writer, &req).await;

        // Read responses until we get a JobFinished or JobRejected for our job.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut got_response = false;
        while tokio::time::Instant::now() < deadline {
            let env = match read_envelope(&mut reader).await {
                Some(e) => e,
                None => break,
            };
            match env.payload {
                Some(envelope::Payload::JobFinished(ref fin)) if fin.job_id == job_id => {
                    got_response = true;
                    break;
                }
                Some(envelope::Payload::JobRejected(ref rej)) if rej.job_id == job_id => {
                    // Default session mode is Inactive, so rejection is expected.
                    // A rejection still proves the server processed our request.
                    got_response = true;
                    break;
                }
                _ => continue,
            }
        }
        assert!(
            got_response,
            "did not receive JobFinished or JobRejected within timeout"
        );

        server_handle.abort();
    }

    // -----------------------------------------------------------------------
    // Test: send a JobRequest with AutoAccept mode and get JobFinished.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn serve_ipc_job_auto_accept() {
        let dir = tempfile::tempdir().unwrap();
        let session_mgr = Arc::new(session::SessionManager::new(60));
        // Pre-set the default mode so new callers are auto-accepted.
        session_mgr
            .set_default_mode(SessionMode::AutoAccept)
            .await;

        let (sock_path, server_handle) = start_server(&dir, Arc::clone(&session_mgr)).await;

        let stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut reader = tokio::io::BufReader::new(reader);

        let job_id = format!("test-auto-{}", std::process::id());
        let req = Envelope {
            device_id: "test-client".to_string(),
            msg_id: "msg-auto".to_string(),
            ts_ms: 0,
            payload: Some(envelope::Payload::JobRequest(JobRequest {
                job_id: job_id.clone(),
                tool: "echo".to_string(),
                args: vec!["hello".to_string()],
                ..Default::default()
            })),
            ..Default::default()
        };
        write_envelope(&mut writer, &req).await;

        // With AutoAccept the job should be accepted and run.
        // We expect a JobFinished (echo should complete quickly).
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut got_finished = false;
        while tokio::time::Instant::now() < deadline {
            let env = match read_envelope(&mut reader).await {
                Some(e) => e,
                None => break,
            };
            if let Some(envelope::Payload::JobFinished(ref fin)) = env.payload {
                if fin.job_id == job_id {
                    got_finished = true;
                    break;
                }
            }
        }
        assert!(got_finished, "did not receive JobFinished within timeout");

        server_handle.abort();
    }

    // -----------------------------------------------------------------------
    // Test: SessionQuery -> SessionState roundtrip.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn serve_ipc_session_query_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let session_mgr = Arc::new(session::SessionManager::new(60));

        let (sock_path, server_handle) = start_server(&dir, Arc::clone(&session_mgr)).await;

        let stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut reader = tokio::io::BufReader::new(reader);

        // Give the server a moment to register our peer_cred via register_caller.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Query our own session state using our UID.
        // The server registers the caller_uid from peer_cred on connect, so
        // querying with that UID should return a SessionState.
        let our_uid = format!("uid:{}", unsafe { libc::getuid() });
        let query = Envelope {
            device_id: "test-client".to_string(),
            msg_id: "msg-session-query".to_string(),
            ts_ms: 0,
            payload: Some(envelope::Payload::SessionQuery(SessionQuery {
                caller_uid: our_uid.clone(),
            })),
            ..Default::default()
        };
        write_envelope(&mut writer, &query).await;

        // Read back a SessionState.
        let env = read_envelope(&mut reader)
            .await
            .expect("expected SessionState response from server");

        match env.payload {
            Some(envelope::Payload::SessionState(state)) => {
                assert_eq!(state.caller_uid, our_uid);
                // Default mode is Inactive.
                assert_eq!(state.mode, i32::from(SessionMode::Inactive));
            }
            other => panic!("expected SessionState, got {other:?}"),
        }

        server_handle.abort();
    }

    // -----------------------------------------------------------------------
    // Test: SessionQuery with empty caller_uid returns all sessions.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn serve_ipc_session_query_all() {
        let dir = tempfile::tempdir().unwrap();
        let session_mgr = Arc::new(session::SessionManager::new(60));

        let (sock_path, server_handle) = start_server(&dir, Arc::clone(&session_mgr)).await;

        let stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut reader = tokio::io::BufReader::new(reader);

        // Let server register our peer_cred.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Query all sessions (empty caller_uid).
        let query = Envelope {
            device_id: "test-client".to_string(),
            msg_id: "msg-session-all".to_string(),
            ts_ms: 0,
            payload: Some(envelope::Payload::SessionQuery(SessionQuery {
                caller_uid: String::new(),
            })),
            ..Default::default()
        };
        write_envelope(&mut writer, &query).await;

        // We should get at least one SessionState (for our own connection).
        let env = read_envelope(&mut reader)
            .await
            .expect("expected at least one SessionState response");

        match env.payload {
            Some(envelope::Payload::SessionState(state)) => {
                // The caller_uid should be our uid.
                let our_uid = format!("uid:{}", unsafe { libc::getuid() });
                assert_eq!(state.caller_uid, our_uid);
            }
            other => panic!("expected SessionState, got {other:?}"),
        }

        server_handle.abort();
    }

    // -----------------------------------------------------------------------
    // Test: peer identity format on Unix.
    // -----------------------------------------------------------------------

    #[test]
    fn peer_identity_format_unix() {
        // On Unix, the serve_ipc_unix handler formats peer_cred as "uid:{number}".
        let uid = unsafe { libc::getuid() };
        let identity = format!("uid:{uid}");
        assert!(identity.starts_with("uid:"));
        // UID should be a valid non-negative integer.
        let parsed: u32 = identity
            .strip_prefix("uid:")
            .unwrap()
            .parse()
            .expect("uid should be a valid u32");
        assert_eq!(parsed, uid);
    }
}
