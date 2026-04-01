#![allow(dead_code)]

use std::time::Duration;

use axum::body::Body;
use axum::http::{header::AUTHORIZATION, Request};
use ed25519_dalek::{Signer, SigningKey};
use futures_util::{SinkExt, StreamExt};
use prost::Message;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_tungstenite::MaybeTlsStream;

use ahand_protocol::{
    envelope, hello, job_event, BootstrapAuth, Ed25519Auth, Envelope, Hello, HelloChallenge,
    JobEvent, JobFinished, JobRequest,
};

pub fn service_request(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(AUTHORIZATION, "Bearer service-test-token")
        .body(Body::empty())
        .unwrap()
}

pub struct TestServer {
    base_http_url: String,
    base_ws_url: String,
    _task: JoinHandle<()>,
}

impl TestServer {
    pub fn ws_url(&self, path: &str) -> String {
        format!("{}{}", self.base_ws_url, path)
    }

    pub async fn get_json(&self, path: &str, token: &str) -> serde_json::Value {
        reqwest::Client::new()
            .get(format!("{}{}", self.base_http_url, path))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    pub async fn post_json(
        &self,
        path: &str,
        token: &str,
        body: serde_json::Value,
    ) -> serde_json::Value {
        reqwest::Client::new()
            .post(format!("{}{}", self.base_http_url, path))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
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
}

pub struct TestDevice {
    device_id: String,
    socket: tokio_tungstenite::WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
}

impl TestDevice {
    pub async fn recv_job_request(&mut self) -> JobRequest {
        while let Some(message) = self.socket.next().await {
            let message = message.unwrap();
            let tokio_tungstenite::tungstenite::Message::Binary(data) = message else {
                continue;
            };

            let envelope = Envelope::decode(data.as_ref()).unwrap();
            if let Some(envelope::Payload::JobRequest(job)) = envelope.payload {
                return job;
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
}

pub async fn spawn_test_server() -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = ahand_hub::build_test_app().await;
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(25)).await;

    TestServer {
        base_http_url: format!("http://{address}"),
        base_ws_url: format!("ws://{address}"),
        _task: task,
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

pub fn signed_hello(device_id: &str, challenge_nonce: &[u8]) -> Envelope {
    signed_hello_with_key_at(
        device_id,
        &SigningKey::from_bytes(&[7u8; 32]),
        now_ms(),
        challenge_nonce,
    )
}

pub fn signed_hello_at(device_id: &str, signed_at_ms: u64, challenge_nonce: &[u8]) -> Envelope {
    signed_hello_with_key_at(
        device_id,
        &SigningKey::from_bytes(&[7u8; 32]),
        signed_at_ms,
        challenge_nonce,
    )
}

pub fn signed_hello_with_key_at(
    device_id: &str,
    signing_key: &SigningKey,
    signed_at_ms: u64,
    challenge_nonce: &[u8],
) -> Envelope {
    let signature = signing_key
        .sign(&ahand_protocol::build_hello_auth_payload(
            device_id,
            signed_at_ms,
            challenge_nonce,
        ))
        .to_bytes()
        .to_vec();

    Envelope {
        device_id: device_id.into(),
        msg_id: "hello-1".into(),
        ts_ms: signed_at_ms,
        payload: Some(envelope::Payload::Hello(Hello {
            version: "0.1.2".into(),
            hostname: "devbox".into(),
            os: "linux".into(),
            capabilities: vec!["exec".into()],
            last_ack: 0,
            auth: Some(hello::Auth::Ed25519(Ed25519Auth {
                public_key: signing_key.verifying_key().to_bytes().to_vec(),
                signature,
                signed_at_ms,
            })),
        })),
        ..Default::default()
    }
}

pub fn bootstrap_hello(device_id: &str, token: &str, challenge_nonce: &[u8]) -> Envelope {
    let signed_at_ms = now_ms();
    let signing_key = SigningKey::from_bytes(&[7u8; 32]);
    let signature = signing_key
        .sign(&ahand_protocol::build_hello_auth_payload(
            device_id,
            signed_at_ms,
            challenge_nonce,
        ))
        .to_bytes()
        .to_vec();

    Envelope {
        device_id: device_id.into(),
        msg_id: "hello-bootstrap-1".into(),
        ts_ms: signed_at_ms,
        payload: Some(envelope::Payload::Hello(Hello {
            version: "0.1.2".into(),
            hostname: "bootstrap-box".into(),
            os: "linux".into(),
            capabilities: vec!["exec".into()],
            last_ack: 0,
            auth: Some(hello::Auth::Bootstrap(BootstrapAuth {
                bearer_token: token.into(),
                public_key: signing_key.verifying_key().to_bytes().to_vec(),
                signature,
                signed_at_ms,
            })),
        })),
        ..Default::default()
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
