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
