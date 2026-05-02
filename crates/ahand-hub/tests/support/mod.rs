#![allow(dead_code)]

use std::time::Duration;

use ahand_hub::config::{Config, S3Config, StoreConfig};
use ahand_hub::state::AppState;
use ahand_hub_core::device::NewDevice;
use ahand_hub_core::traits::DeviceStore;
use ahand_hub_store::test_support::TestStack;
use axum::body::Body;
use axum::http::{Request, header::AUTHORIZATION};
use ed25519_dalek::{Signer, SigningKey};
use futures_util::{SinkExt, StreamExt};
use prost::Message;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use ahand_protocol::{
    BootstrapAuth, BrowserRequest, BrowserResponse, CancelJob, Ed25519Auth, Envelope, FileRequest,
    FileResponse, Hello, HelloAccepted, HelloChallenge, JobEvent, JobFinished, JobRequest,
    envelope, hello, job_event,
};

pub fn service_request(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(AUTHORIZATION, "Bearer service-test-token")
        .body(Body::empty())
        .unwrap()
}

pub fn test_config() -> Config {
    Config {
        bind_addr: "127.0.0.1:0".into(),
        service_token: "service-test-token".into(),
        dashboard_shared_password: "shared-secret".into(),
        dashboard_allowed_origins: Vec::new(),
        device_bootstrap_token: "bootstrap-test-token".into(),
        device_bootstrap_device_id: "device-2".into(),
        device_hello_max_age_ms: 30_000,
        device_staleness_probe_interval_ms: 30_000,
        device_staleness_timeout_ms: 180_000,
        device_expected_heartbeat_secs: 60,
        device_presence_ttl_secs: 60,
        device_presence_refresh_ms: 20_000,
        job_timeout_grace_ms: 50,
        device_disconnect_grace_ms: 100,
        jwt_secret: "service-test-secret".into(),
        audit_retention_days: 90,
        audit_fallback_path: std::env::temp_dir().join("ahand-hub-test-audit-fallback.jsonl"),
        output_retention_ms: 60_000,
        webhook_url: None,
        webhook_secret: None,
        webhook_max_retries: 8,
        webhook_max_concurrency: 50,
        webhook_timeout_ms: 5_000,
        file_request_timeout_ms: 30_000,
        store: StoreConfig::Memory,
        s3: None,
    }
}

pub fn persistent_test_config(stack: &TestStack) -> Config {
    Config {
        bind_addr: "127.0.0.1:0".into(),
        service_token: "service-test-token".into(),
        dashboard_shared_password: "shared-secret".into(),
        dashboard_allowed_origins: Vec::new(),
        device_bootstrap_token: "bootstrap-test-token".into(),
        device_bootstrap_device_id: "device-2".into(),
        device_hello_max_age_ms: 30_000,
        device_staleness_probe_interval_ms: 30_000,
        device_staleness_timeout_ms: 180_000,
        device_expected_heartbeat_secs: 60,
        device_presence_ttl_secs: 60,
        device_presence_refresh_ms: 20_000,
        job_timeout_grace_ms: 50,
        device_disconnect_grace_ms: 100,
        jwt_secret: "service-test-secret".into(),
        audit_retention_days: 90,
        audit_fallback_path: std::env::temp_dir()
            .join("ahand-hub-persistent-test-audit-fallback.jsonl"),
        output_retention_ms: 60_000,
        webhook_url: None,
        webhook_secret: None,
        webhook_max_retries: 8,
        webhook_max_concurrency: 50,
        webhook_timeout_ms: 5_000,
        file_request_timeout_ms: 30_000,
        store: StoreConfig::Persistent {
            database_url: stack.database_url().into(),
            redis_url: stack.redis_url().into(),
        },
        s3: None,
    }
}

/// Build a test config with an S3 block. The actual `S3Client` is built
/// later (see `test_state_with_s3`) with explicit synthetic credentials,
/// avoiding process-wide env mutation. The endpoint points at an
/// unreachable port (1) so any real S3 traffic fails fast.
pub fn test_s3_config() -> S3Config {
    S3Config {
        bucket: "test-bucket".into(),
        region: "us-east-1".into(),
        endpoint: Some("http://127.0.0.1:1".into()),
        // Threshold deliberately small so 4KB-ish payloads trigger the
        // swap path without forcing tests to allocate megabytes.
        file_transfer_threshold_bytes: 1024,
        url_expiration_secs: 3600,
    }
}

/// Build an `S3Client` against a fake endpoint with explicit synthetic
/// credentials. Pre-signing is a pure local HMAC computation, so URL
/// composition is verifiable without a running S3 backend; live
/// PutObject/GetObject calls error out on connection refused — exactly
/// what we want when verifying that the swap path was *entered*.
pub fn build_test_s3_client(cfg: &S3Config) -> ahand_hub::s3::S3Client {
    use aws_config::BehaviorVersion;
    use aws_credential_types::Credentials;
    let endpoint = cfg.endpoint.as_ref().expect("test s3 config has endpoint");
    let creds = Credentials::new("test", "test", None, None, "ahand-hub-tests");
    let s3_config = aws_sdk_s3::config::Builder::new()
        .behavior_version(BehaviorVersion::latest())
        .credentials_provider(creds)
        .region(aws_sdk_s3::config::Region::new(cfg.region.clone()))
        .endpoint_url(endpoint)
        .force_path_style(true)
        .build();
    ahand_hub::s3::S3Client::from_aws_client(
        aws_sdk_s3::Client::from_conf(s3_config),
        &cfg.bucket,
        cfg.file_transfer_threshold_bytes,
        cfg.url_expiration_secs,
    )
}

pub async fn test_state_with_s3() -> AppState {
    let mut state = AppState::from_config(test_config())
        .await
        .expect("test state should build");
    state.s3_client = Some(std::sync::Arc::new(build_test_s3_client(&test_s3_config())));
    state
}

pub async fn test_state() -> AppState {
    let state = AppState::from_config(test_config())
        .await
        .expect("test state should build");
    state
        .devices
        .insert(NewDevice {
            id: "device-1".into(),
            public_key: Some(
                SigningKey::from_bytes(&[7u8; 32])
                    .verifying_key()
                    .to_bytes()
                    .to_vec(),
            ),
            hostname: "seeded-device".into(),
            os: "linux".into(),
            capabilities: vec!["exec".into()],
            version: Some("0.1.2".into()),
            auth_method: "ed25519".into(),
            external_user_id: None,
        })
        .await
        .unwrap();
    state.devices.mark_offline("device-1").await.unwrap();
    state
}

/// Build an `AppState` whose outbound webhook is configured against an
/// unreachable URL. Every `enqueue_*` call still persists to the
/// underlying `MemoryWebhookDeliveryStore`, which tests can inspect via
/// `state.webhook.store()`. Deliveries will never succeed (the URL
/// routes nowhere) but that's fine — we only care about verifying that
/// admin / ws code paths enqueued the expected events.
pub async fn test_state_with_webhook() -> AppState {
    let mut config = test_config();
    // Port 1 is reserved; a POST here fails fast on every platform so
    // the worker doesn't build up wall-clock debt during tests.
    config.webhook_url = Some("http://127.0.0.1:1/webhook".into());
    config.webhook_secret = Some("test-webhook-secret".into());
    // Keep attempts small so the worker doesn't spam the log if the
    // test takes a while to assert.
    config.webhook_max_retries = 1;
    config.webhook_max_concurrency = 2;
    let state = AppState::from_config(config)
        .await
        .expect("test state should build");
    state.devices.mark_offline("device-2").await.ok();
    state
}

pub async fn test_state_with_browser_device() -> AppState {
    let state = AppState::from_config(test_config())
        .await
        .expect("test state should build");
    state
        .devices
        .insert(NewDevice {
            id: "device-1".into(),
            public_key: Some(
                SigningKey::from_bytes(&[7u8; 32])
                    .verifying_key()
                    .to_bytes()
                    .to_vec(),
            ),
            hostname: "seeded-device".into(),
            os: "linux".into(),
            capabilities: vec!["exec".into(), "browser-playwright-cli".into()],
            version: Some("0.1.2".into()),
            auth_method: "ed25519".into(),
            external_user_id: None,
        })
        .await
        .unwrap();
    state.devices.mark_offline("device-1").await.unwrap();
    state
}

pub async fn build_test_app() -> axum::Router {
    ahand_hub::build_app(test_state().await)
}

pub struct TestServer {
    base_http_url: String,
    base_ws_url: String,
    state: AppState,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl TestServer {
    pub fn http_base_url(&self) -> &str {
        &self.base_http_url
    }

    pub fn events(&self) -> std::sync::Arc<ahand_hub::events::EventBus> {
        self.state.events.clone()
    }

    pub fn state(&self) -> &AppState {
        &self.state
    }

    pub fn ws_url(&self, path: &str) -> String {
        format!("{}{}", self.base_ws_url, path)
    }

    pub async fn get(&self, path: &str, token: &str) -> reqwest::Response {
        reqwest::Client::new()
            .get(format!("{}{}", self.base_http_url, path))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
    }

    pub async fn get_json(&self, path: &str, token: &str) -> serde_json::Value {
        self.get(path, token).await.json().await.unwrap()
    }

    pub async fn post(
        &self,
        path: &str,
        token: &str,
        body: serde_json::Value,
    ) -> reqwest::Response {
        reqwest::Client::new()
            .post(format!("{}{}", self.base_http_url, path))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    pub async fn post_json(
        &self,
        path: &str,
        token: &str,
        body: serde_json::Value,
    ) -> serde_json::Value {
        self.post(path, token, body).await.json().await.unwrap()
    }

    pub async fn attach_test_device(&self, device_id: &str) -> TestDevice {
        let (mut socket, _) = tokio_tungstenite::connect_async(self.ws_url("/ws"))
            .await
            .unwrap();
        let challenge = read_hello_challenge(&mut socket).await;
        let hello = signed_hello(device_id, &challenge.nonce);
        socket
            .send(tokio_tungstenite::tungstenite::Message::Binary(
                hello.encode_to_vec().into(),
            ))
            .await
            .unwrap();
        let _ = read_hello_accepted(&mut socket).await;

        TestDevice {
            device_id: device_id.into(),
            socket,
        }
    }

    pub async fn connect_dashboard_socket(
        &self,
        session_token: Option<&str>,
    ) -> tokio_tungstenite::WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>> {
        self.connect_dashboard_socket_with_origin(session_token, Some(&self.base_http_url))
            .await
    }

    pub async fn connect_dashboard_socket_with_origin(
        &self,
        session_token: Option<&str>,
        origin: Option<&str>,
    ) -> tokio_tungstenite::WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>> {
        let mut request = self.ws_url("/ws/dashboard").into_client_request().unwrap();
        if let Some(token) = session_token {
            request.headers_mut().append(
                "cookie",
                format!("ahand_hub_session={token}").parse().unwrap(),
            );
        }
        if let Some(origin) = origin {
            request
                .headers_mut()
                .append("origin", origin.parse().unwrap());
        }

        let (socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        socket
    }

    pub async fn attach_browser_device(&self, device_id: &str) -> TestDevice {
        let (mut socket, _) = tokio_tungstenite::connect_async(self.ws_url("/ws"))
            .await
            .unwrap();
        let challenge = read_hello_challenge(&mut socket).await;
        let hello = signed_hello_with_browser(device_id, &challenge.nonce);
        socket
            .send(tokio_tungstenite::tungstenite::Message::Binary(
                hello.encode_to_vec().into(),
            ))
            .await
            .unwrap();
        let _ = read_hello_accepted(&mut socket).await;

        TestDevice {
            device_id: device_id.into(),
            socket,
        }
    }

    pub async fn attach_bootstrap_device(&self, device_id: &str, token: &str) -> TestDevice {
        let (mut socket, _) = tokio_tungstenite::connect_async(self.ws_url("/ws"))
            .await
            .unwrap();
        let challenge = read_hello_challenge(&mut socket).await;
        let hello = bootstrap_hello(device_id, token, &challenge.nonce);
        socket
            .send(tokio_tungstenite::tungstenite::Message::Binary(
                hello.encode_to_vec().into(),
            ))
            .await
            .unwrap();
        let _ = read_hello_accepted(&mut socket).await;

        TestDevice {
            device_id: device_id.into(),
            socket,
        }
    }

    pub async fn read_sse(&self, path: &str, token: &str) -> String {
        let response = reqwest::Client::new()
            .get(format!("{}{}", self.base_http_url, path))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();

        let mut stream = response.bytes_stream();
        let mut body = String::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            body.push_str(&String::from_utf8_lossy(&chunk));
            if body.contains("event: finished") {
                break;
            }
        }
        body
    }

    pub async fn read_sse_for(&self, path: &str, token: &str, duration: Duration) -> String {
        let response = reqwest::Client::new()
            .get(format!("{}{}", self.base_http_url, path))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();

        let mut stream = response.bytes_stream();
        let started_at = tokio::time::Instant::now();
        let mut body = String::new();

        while let Some(remaining) = duration.checked_sub(started_at.elapsed()) {
            match tokio::time::timeout(remaining, stream.next()).await {
                Ok(Some(Ok(chunk))) => body.push_str(&String::from_utf8_lossy(&chunk)),
                Ok(Some(Err(err))) => panic!("failed reading SSE chunk: {err}"),
                Ok(None) | Err(_) => break,
            }
        }

        body
    }

    pub async fn shutdown(self) {
        let mut task = self.task;
        if let Some(shutdown_tx) = self.shutdown_tx {
            let _ = shutdown_tx.send(());
        }
        if tokio::time::timeout(Duration::from_secs(1), &mut task)
            .await
            .is_err()
        {
            task.abort();
            let _ = task.await;
        }
        self.state.shutdown().await.unwrap();
    }
}

pub struct TestDevice {
    device_id: String,
    socket: tokio_tungstenite::WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
}

/// Build a [`TestDevice`] from a pre-handshaked socket. Used by tests
/// that need to drive the pre-register flow themselves (e.g. the
/// control-plane browser tests, which seed `external_user_id` on the
/// device row before the daemon hellos).
pub fn test_device_from_socket(
    device_id: &str,
    socket: tokio_tungstenite::WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
) -> TestDevice {
    TestDevice {
        device_id: device_id.into(),
        socket,
    }
}

impl TestDevice {
    pub async fn recv_job_request(&mut self) -> JobRequest {
        while let Some(message) = self.socket.next().await {
            let message = message.unwrap();
            if let tokio_tungstenite::tungstenite::Message::Binary(data) = message {
                let envelope = Envelope::decode(data.as_ref()).unwrap();
                if let Some(envelope::Payload::JobRequest(job)) = envelope.payload {
                    return job;
                }
            }
        }

        panic!("device socket closed before a job request arrived");
    }

    pub async fn send_stdout(&mut self, job_id: &str, chunk: &[u8]) {
        let envelope = Envelope {
            device_id: self.device_id.clone(),
            msg_id: format!("{job_id}-stdout"),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::JobEvent(JobEvent {
                job_id: job_id.into(),
                event: Some(job_event::Event::StdoutChunk(chunk.to_vec())),
            })),
            ..Default::default()
        };

        self.socket
            .send(tokio_tungstenite::tungstenite::Message::Binary(
                envelope.encode_to_vec().into(),
            ))
            .await
            .unwrap();
    }

    pub async fn send_finished(&mut self, job_id: &str, exit_code: i32, error: &str) {
        let envelope = Envelope {
            device_id: self.device_id.clone(),
            msg_id: format!("{job_id}-finished"),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::JobFinished(JobFinished {
                job_id: job_id.into(),
                exit_code,
                error: error.into(),
            })),
            ..Default::default()
        };

        self.socket
            .send(tokio_tungstenite::tungstenite::Message::Binary(
                envelope.encode_to_vec().into(),
            ))
            .await
            .unwrap();
    }

    pub async fn recv_file_request(&mut self) -> FileRequest {
        while let Some(message) = self.socket.next().await {
            let message = message.unwrap();
            if let tokio_tungstenite::tungstenite::Message::Binary(data) = message {
                let envelope = Envelope::decode(data.as_ref()).unwrap();
                if let Some(envelope::Payload::FileRequest(req)) = envelope.payload {
                    return req;
                }
            }
        }
        panic!("device socket closed before a file request arrived");
    }

    pub async fn send_file_response(&mut self, response: FileResponse) {
        let envelope = Envelope {
            device_id: self.device_id.clone(),
            msg_id: format!("file-resp-{}", response.request_id),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::FileResponse(response)),
            ..Default::default()
        };
        self.socket
            .send(tokio_tungstenite::tungstenite::Message::Binary(
                envelope.encode_to_vec().into(),
            ))
            .await
            .unwrap();
    }

    pub async fn recv_browser_request(&mut self) -> BrowserRequest {
        while let Some(message) = self.socket.next().await {
            let message = message.unwrap();
            if let tokio_tungstenite::tungstenite::Message::Binary(data) = message {
                let envelope = Envelope::decode(data.as_ref()).unwrap();
                if let Some(envelope::Payload::BrowserRequest(req)) = envelope.payload {
                    return req;
                }
            }
        }
        panic!("device socket closed before a browser request arrived");
    }

    pub async fn send_browser_response(&mut self, response: BrowserResponse) {
        let envelope = Envelope {
            device_id: self.device_id.clone(),
            msg_id: format!("browser-resp-{}", response.request_id),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::BrowserResponse(response)),
            ..Default::default()
        };
        self.socket
            .send(tokio_tungstenite::tungstenite::Message::Binary(
                envelope.encode_to_vec().into(),
            ))
            .await
            .unwrap();
    }

    pub async fn recv_cancel_request(&mut self) -> CancelJob {
        while let Some(message) = self.socket.next().await {
            let message = message.unwrap();
            if let tokio_tungstenite::tungstenite::Message::Binary(data) = message {
                let envelope = Envelope::decode(data.as_ref()).unwrap();
                if let Some(envelope::Payload::CancelJob(cancel)) = envelope.payload {
                    return cancel;
                }
            }
        }

        panic!("device socket closed before a cancel request arrived");
    }
}

pub async fn spawn_test_server() -> TestServer {
    spawn_server_with_state(test_state().await).await
}

pub async fn spawn_server_with_state(state: ahand_hub::state::AppState) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = ahand_hub::build_app(state.clone());
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(25)).await;

    TestServer {
        base_http_url: format!("http://{address}"),
        base_ws_url: format!("ws://{address}"),
        state,
        shutdown_tx: Some(shutdown_tx),
        task,
    }
}

pub async fn read_hello_challenge(
    socket: &mut tokio_tungstenite::WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
) -> HelloChallenge {
    let message = socket.next().await.unwrap().unwrap();
    let tokio_tungstenite::tungstenite::Message::Binary(data) = message else {
        panic!("expected binary hello challenge frame");
    };
    let envelope = Envelope::decode(data.as_ref()).unwrap();
    match envelope.payload.unwrap() {
        envelope::Payload::HelloChallenge(challenge) => challenge,
        other => panic!("unexpected handshake payload: {other:?}"),
    }
}

pub async fn read_hello_accepted(
    socket: &mut tokio_tungstenite::WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
) -> HelloAccepted {
    let message = socket.next().await.unwrap().unwrap();
    let tokio_tungstenite::tungstenite::Message::Binary(data) = message else {
        panic!("expected binary hello accepted frame");
    };
    let envelope = Envelope::decode(data.as_ref()).unwrap();
    match envelope.payload.unwrap() {
        envelope::Payload::HelloAccepted(accepted) => accepted,
        other => panic!("unexpected handshake payload: {other:?}"),
    }
}

pub fn signed_hello(device_id: &str, challenge_nonce: &[u8]) -> Envelope {
    signed_hello_with_last_ack(device_id, 0, challenge_nonce)
}

pub fn signed_hello_with_last_ack(
    device_id: &str,
    last_ack: u64,
    challenge_nonce: &[u8],
) -> Envelope {
    signed_hello_with_last_ack_at(
        device_id,
        last_ack,
        next_test_signed_at_ms(),
        challenge_nonce,
    )
}

pub fn signed_hello_with_last_ack_at(
    device_id: &str,
    last_ack: u64,
    signed_at_ms: u64,
    challenge_nonce: &[u8],
) -> Envelope {
    signed_hello_with_key_at(
        device_id,
        &SigningKey::from_bytes(&[7u8; 32]),
        last_ack,
        signed_at_ms,
        challenge_nonce,
    )
}

pub fn signed_hello_at(device_id: &str, signed_at_ms: u64, challenge_nonce: &[u8]) -> Envelope {
    signed_hello_with_last_ack_at(device_id, 0, signed_at_ms, challenge_nonce)
}

pub fn signed_hello_with_key_at(
    device_id: &str,
    signing_key: &SigningKey,
    last_ack: u64,
    signed_at_ms: u64,
    challenge_nonce: &[u8],
) -> Envelope {
    let mut hello = Hello {
        version: "0.1.2".into(),
        hostname: "devbox".into(),
        os: "linux".into(),
        capabilities: vec!["exec".into()],
        last_ack,
        auth: None,
    };
    let signature = signing_key
        .sign(&ahand_protocol::build_hello_auth_payload(
            device_id,
            &hello,
            signed_at_ms,
            challenge_nonce,
        ))
        .to_bytes()
        .to_vec();
    hello.auth = Some(hello::Auth::Ed25519(Ed25519Auth {
        public_key: signing_key.verifying_key().to_bytes().to_vec(),
        signature,
        signed_at_ms,
    }));

    Envelope {
        device_id: device_id.into(),
        msg_id: "hello-1".into(),
        ts_ms: signed_at_ms,
        payload: Some(envelope::Payload::Hello(hello)),
        ..Default::default()
    }
}

pub fn signed_hello_with_browser(device_id: &str, challenge_nonce: &[u8]) -> Envelope {
    let signed_at_ms = next_test_signed_at_ms();
    let signing_key = SigningKey::from_bytes(&[7u8; 32]);
    let mut hello = Hello {
        version: "0.1.2".into(),
        hostname: "devbox".into(),
        os: "linux".into(),
        capabilities: vec!["exec".into(), "browser-playwright-cli".into()],
        last_ack: 0,
        auth: None,
    };
    let signature = signing_key
        .sign(&ahand_protocol::build_hello_auth_payload(
            device_id,
            &hello,
            signed_at_ms,
            challenge_nonce,
        ))
        .to_bytes()
        .to_vec();
    hello.auth = Some(hello::Auth::Ed25519(Ed25519Auth {
        public_key: signing_key.verifying_key().to_bytes().to_vec(),
        signature,
        signed_at_ms,
    }));

    Envelope {
        device_id: device_id.into(),
        msg_id: "hello-browser-1".into(),
        ts_ms: signed_at_ms,
        payload: Some(envelope::Payload::Hello(hello)),
        ..Default::default()
    }
}

pub fn bootstrap_hello(device_id: &str, token: &str, challenge_nonce: &[u8]) -> Envelope {
    let signed_at_ms = now_ms();
    let signing_key = SigningKey::from_bytes(&[7u8; 32]);
    let mut hello = Hello {
        version: "0.1.2".into(),
        hostname: "bootstrap-box".into(),
        os: "linux".into(),
        capabilities: vec!["exec".into()],
        last_ack: 0,
        auth: None,
    };
    let signature = signing_key
        .sign(&ahand_protocol::build_hello_auth_payload(
            device_id,
            &hello,
            signed_at_ms,
            challenge_nonce,
        ))
        .to_bytes()
        .to_vec();
    hello.auth = Some(hello::Auth::Bootstrap(BootstrapAuth {
        bearer_token: token.into(),
        public_key: signing_key.verifying_key().to_bytes().to_vec(),
        signature,
        signed_at_ms,
    }));

    Envelope {
        device_id: device_id.into(),
        msg_id: "hello-bootstrap-1".into(),
        ts_ms: signed_at_ms,
        payload: Some(envelope::Payload::Hello(hello)),
        ..Default::default()
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn next_test_signed_at_ms() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};

    static LAST_SIGNED_AT_MS: AtomicU64 = AtomicU64::new(0);

    let mut candidate = now_ms();
    loop {
        let last = LAST_SIGNED_AT_MS.load(Ordering::Relaxed);
        let next = candidate.max(last.saturating_add(1));
        match LAST_SIGNED_AT_MS.compare_exchange(last, next, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return next,
            Err(observed) => candidate = candidate.max(observed.saturating_add(1)),
        }
    }
}
