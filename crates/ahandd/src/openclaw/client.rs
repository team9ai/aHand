//! OpenClaw Gateway WebSocket client.
//!
//! Manages the connection to an OpenClaw Gateway and handles message routing.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

use crate::approval::ApprovalManager;
use crate::config::OpenClawConfig;
use crate::registry::JobRegistry;
use crate::session::SessionManager;
use crate::store::RunStore;

use super::device_identity::{build_auth_payload, default_identity_path, DeviceIdentity};
use super::handler::OpenClawHandler;
use super::pairing::{
    default_pairing_path, generate_node_id, load_pairing_state, save_pairing_state, GatewayInfo,
};
use super::protocol::{
    AuthParams, ClientInfo, ConnectChallengePayload, ConnectParams, DeviceParams, EventFrame,
    GatewayFrame, HelloOk, NodeEvent, NodeInvokeRequest, NodeInvokeResult, RequestFrame,
    ResponseFrame, PROTOCOL_VERSION,
};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// OpenClaw Gateway client
pub struct OpenClawClient {
    config: OpenClawConfig,
    registry: Arc<JobRegistry>,
    session_mgr: Arc<SessionManager>,
    approval_mgr: Arc<ApprovalManager>,
    store: Option<Arc<RunStore>>,
}

impl OpenClawClient {
    pub fn new(
        config: OpenClawConfig,
        registry: Arc<JobRegistry>,
        session_mgr: Arc<SessionManager>,
        approval_mgr: Arc<ApprovalManager>,
        store: Option<Arc<RunStore>>,
    ) -> Self {
        Self {
            config,
            registry,
            session_mgr,
            approval_mgr,
            store,
        }
    }

    /// Run the client with automatic reconnection
    pub async fn run(&self) -> anyhow::Result<()> {
        let mut backoff = 1u64;

        loop {
            let host = self
                .config
                .gateway_host
                .as_deref()
                .unwrap_or("127.0.0.1");
            let port = self.config.gateway_port.unwrap_or(18789);

            info!(
                host = %host,
                port = port,
                "connecting to OpenClaw Gateway"
            );

            match self.connect().await {
                Ok(()) => {
                    info!("connection closed normally");
                    backoff = 1;
                }
                Err(e) => {
                    warn!(error = %e, "connection failed");
                }
            }

            info!(backoff_secs = backoff, "reconnecting");
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(30);
        }
    }

    /// Establish and maintain a single connection
    async fn connect(&self) -> anyhow::Result<()> {
        let url = self.build_url();
        let (ws, _response) = tokio_tungstenite::connect_async(&url).await?;
        let (mut sink, mut stream) = ws.split();

        info!("connected to Gateway");

        // Load or create pairing state
        let pairing_path = default_pairing_path();
        let mut pairing = load_pairing_state(&pairing_path)?.unwrap_or_default();

        // Ensure we have a node ID
        if pairing.node_id.is_empty() {
            pairing.node_id = self
                .config
                .node_id
                .clone()
                .unwrap_or_else(generate_node_id);
        }

        // Update display name if provided
        if let Some(name) = &self.config.display_name {
            pairing.display_name = Some(name.clone());
        }

        // Update gateway info
        pairing.gateway = Some(GatewayInfo {
            host: self
                .config
                .gateway_host
                .clone()
                .unwrap_or_else(|| "127.0.0.1".to_string()),
            port: self.config.gateway_port.unwrap_or(18789),
            tls: self.config.gateway_tls.unwrap_or(false),
            tls_fingerprint: self.config.gateway_tls_fingerprint.clone(),
        });

        // Save pairing state
        save_pairing_state(&pairing_path, &pairing)?;

        let node_id = pairing.node_id.clone();
        let display_name = pairing.display_name.clone();

        // Load or create device identity
        let identity_path = default_identity_path();
        let device_identity = DeviceIdentity::load_or_create(&identity_path)?;
        info!(device_id = %device_identity.device_id, "loaded device identity");

        // Create handler - use device_id as node_id since Gateway identifies nodes by device ID
        let handler = OpenClawHandler::new(
            device_identity.device_id.clone(),
            Arc::clone(&self.registry),
            Arc::clone(&self.session_mgr),
            Arc::clone(&self.approval_mgr),
            self.store.clone(),
            self.config.exec_approvals_path.as_ref().map(PathBuf::from),
        );

        // Create channel for sending responses
        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();

        // Spawn send task
        let tx_clone = tx.clone();
        let send_task = tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if let Err(e) = sink.send(msg).await {
                    warn!(error = %e, "failed to send message");
                    break;
                }
            }
        });

        // Pending requests
        let mut pending: HashMap<String, tokio::sync::oneshot::Sender<ResponseFrame>> =
            HashMap::new();
        let mut connect_nonce: Option<String> = None;
        let mut connect_sent = false;
        let mut connected = false;
        let mut pairing_requested = false;

        // Set up connect timeout
        let connect_timeout = tokio::time::sleep(Duration::from_millis(750));
        tokio::pin!(connect_timeout);

        // Process incoming messages
        loop {
            tokio::select! {
                // Connect timeout - send connect without challenge
                _ = &mut connect_timeout, if !connect_sent => {
                    debug!("connect timeout, sending connect without nonce");
                    self.send_connect(
                        &tx,
                        &node_id,
                        &display_name,
                        connect_nonce.as_deref(),
                        &device_identity,
                        &mut pending,
                    )?;
                    connect_sent = true;
                }

                // Incoming message
                msg_result = stream.next() => {
                    let msg = match msg_result {
                        Some(Ok(m)) => m,
                        Some(Err(e)) => {
                            error!(error = %e, "websocket error");
                            break;
                        }
                        None => {
                            info!("websocket stream ended");
                            break;
                        }
                    };

                    match msg {
                        Message::Text(text) => {
                            debug!(len = text.len(), "received text message");

                            // Try to parse as gateway frame
                            if let Ok(frame) = serde_json::from_str::<GatewayFrame>(&text) {
                                match frame {
                                    GatewayFrame::Event(evt) => {
                                        // Handle connect.challenge
                                        if evt.event == "connect.challenge" && !connect_sent {
                                            if let Ok(challenge) = serde_json::from_value::<ConnectChallengePayload>(evt.payload.clone()) {
                                                if let Some(nonce) = challenge.nonce {
                                                    connect_nonce = Some(nonce);
                                                    debug!("received connect challenge");
                                                    self.send_connect(
                                                        &tx,
                                                        &node_id,
                                                        &display_name,
                                                        connect_nonce.as_deref(),
                                                        &device_identity,
                                                        &mut pending,
                                                    )?;
                                                    connect_sent = true;
                                                }
                                            }
                                        }
                                        // Handle node.invoke.request
                                        else if evt.event == "node.invoke.request" && connected {
                                            if let Ok(invoke) = serde_json::from_value::<NodeInvokeRequest>(evt.payload) {
                                                let (result, exec_event) = handler.handle_invoke(invoke).await;

                                                // Send exec event if present
                                                if let Some(event_payload) = exec_event {
                                                    let event = NodeEvent {
                                                        event: if event_payload.success == Some(true) {
                                                            "exec.finished".to_string()
                                                        } else {
                                                            "exec.denied".to_string()
                                                        },
                                                        payload_json: serde_json::to_string(&event_payload).ok(),
                                                    };
                                                    let req = RequestFrame::new(
                                                        uuid::Uuid::new_v4().to_string(),
                                                        "node.event".to_string(),
                                                        Some(serde_json::to_value(&event)?),
                                                    );
                                                    let _ = tx.send(Message::Text(serde_json::to_string(&req)?));
                                                }

                                                // Send invoke result
                                                let req = RequestFrame::new(
                                                    uuid::Uuid::new_v4().to_string(),
                                                    "node.invoke.result".to_string(),
                                                    Some(serde_json::to_value(&result)?),
                                                );
                                                let _ = tx.send(Message::Text(serde_json::to_string(&req)?));
                                            }
                                        }
                                        // Handle tick
                                        else if evt.event == "tick" {
                                            debug!("received tick");
                                        }
                                        // Handle pairing resolved (approved/rejected)
                                        else if evt.event == "node.pair.resolved" {
                                            if let Some(decision) = evt.payload.get("decision").and_then(|v| v.as_str()) {
                                                if decision == "approved" {
                                                    info!("pairing approved! reconnecting...");
                                                    break; // Reconnect to establish authenticated session
                                                } else {
                                                    warn!(decision = %decision, "pairing request was not approved");
                                                }
                                            }
                                        }
                                    }
                                    GatewayFrame::Response(res) => {
                                        // Handle pending request response
                                        if let Some(sender) = pending.remove(&res.id) {
                                            let _ = sender.send(res.clone());
                                        }

                                        // Check if this is connect response
                                        if res.ok {
                                            if let Some(payload) = &res.payload {
                                                if let Ok(_hello) = serde_json::from_value::<HelloOk>(payload.clone()) {
                                                    info!("connected to Gateway successfully");
                                                    connected = true;
                                                    pairing_requested = false;
                                                }
                                            }
                                        } else {
                                            if let Some(err) = &res.error {
                                                // Handle NOT_PAIRED - Gateway automatically creates pairing request
                                                if err.code == "NOT_PAIRED" && !pairing_requested {
                                                    pairing_requested = true;
                                                    // Extract requestId from error details if available
                                                    let request_id = err.details.as_ref()
                                                        .and_then(|d| d.get("requestId"))
                                                        .and_then(|v| v.as_str());

                                                    if let Some(req_id) = request_id {
                                                        warn!(
                                                            request_id = %req_id,
                                                            "device not paired - approve with: openclaw nodes approve {}",
                                                            req_id
                                                        );
                                                    } else {
                                                        warn!("device not paired - check pending requests with: openclaw nodes pending");
                                                    }
                                                } else {
                                                    error!(code = %err.code, message = %err.message, "request failed");
                                                }
                                            }
                                        }
                                    }
                                    GatewayFrame::Request(_) => {
                                        // We shouldn't receive requests, only events
                                        debug!("received unexpected request frame");
                                    }
                                }
                            }
                        }
                        Message::Ping(data) => {
                            let _ = tx.send(Message::Pong(data));
                        }
                        Message::Close(_) => {
                            info!("received close frame");
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }

        send_task.abort();
        Ok(())
    }

    /// Send connect request
    fn send_connect(
        &self,
        tx: &mpsc::UnboundedSender<Message>,
        node_id: &str,
        display_name: &Option<String>,
        nonce: Option<&str>,
        device_identity: &DeviceIdentity,
        pending: &mut HashMap<String, tokio::sync::oneshot::Sender<ResponseFrame>>,
    ) -> anyhow::Result<()> {
        let id = uuid::Uuid::new_v4().to_string();

        let auth = if self.config.auth_token.is_some() || self.config.auth_password.is_some() {
            Some(AuthParams {
                token: self.config.auth_token.clone(),
                password: self.config.auth_password.clone(),
            })
        } else {
            None
        };

        // Build device identity params with signature
        let role = "node";
        let scopes: Vec<String> = vec![];
        let signed_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let auth_payload = build_auth_payload(
            &device_identity.device_id,
            "node-host",
            "node",
            role,
            &scopes,
            signed_at_ms,
            self.config.auth_token.as_deref(),
            nonce,
        );

        let signature = device_identity.sign(&auth_payload);

        let device = DeviceParams {
            id: device_identity.device_id.clone(),
            public_key: device_identity.public_key_base64url(),
            signature,
            signed_at: signed_at_ms,
            nonce: nonce.map(|s| s.to_string()),
        };

        let params = ConnectParams {
            min_protocol: PROTOCOL_VERSION,
            max_protocol: PROTOCOL_VERSION,
            client: ClientInfo {
                id: "node-host".to_string(),  // Required predefined client ID
                display_name: display_name.clone(),
                version: VERSION.to_string(),
                platform: std::env::consts::OS.to_string(),
                mode: "node".to_string(),
                instance_id: Some(node_id.to_string()),
            },
            caps: Some(vec!["system".to_string()]),
            commands: Some(vec![
                "system.run".to_string(),
                "system.which".to_string(),
                "system.execApprovals.get".to_string(),
                "system.execApprovals.set".to_string(),
            ]),
            permissions: None,
            path_env: std::env::var("PATH").ok(),
            role: Some(role.to_string()),
            scopes: Some(scopes),
            device: Some(device),
            auth,
        };

        let frame = RequestFrame::new(id.clone(), "connect".to_string(), Some(serde_json::to_value(&params)?));

        debug!(device_id = %device_identity.device_id, "sending connect request with device identity");
        tx.send(Message::Text(serde_json::to_string(&frame)?))?;

        // Create oneshot channel for response
        let (resp_tx, _resp_rx) = tokio::sync::oneshot::channel();
        pending.insert(id, resp_tx);

        Ok(())
    }

    /// Send pairing request when NOT_PAIRED
    fn send_pairing_request(
        &self,
        tx: &mpsc::UnboundedSender<Message>,
        device_id: &str,
        display_name: &Option<String>,
    ) -> anyhow::Result<()> {
        let id = uuid::Uuid::new_v4().to_string();

        #[derive(serde::Serialize)]
        struct PairRequestParams {
            #[serde(rename = "nodeId")]
            node_id: String,
            #[serde(rename = "displayName", skip_serializing_if = "Option::is_none")]
            display_name: Option<String>,
            platform: String,
            version: String,
            caps: Vec<String>,
            commands: Vec<String>,
        }

        let params = PairRequestParams {
            node_id: device_id.to_string(),
            display_name: display_name.clone(),
            platform: std::env::consts::OS.to_string(),
            version: VERSION.to_string(),
            caps: vec!["system".to_string()],
            commands: vec![
                "system.run".to_string(),
                "system.which".to_string(),
                "system.execApprovals.get".to_string(),
                "system.execApprovals.set".to_string(),
            ],
        };

        let frame = RequestFrame::new(
            id,
            "node.pair.request".to_string(),
            Some(serde_json::to_value(&params)?),
        );

        debug!(device_id = %device_id, "sending pairing request");
        tx.send(Message::Text(serde_json::to_string(&frame)?))?;

        Ok(())
    }

    /// Build WebSocket URL
    fn build_url(&self) -> String {
        let host = self
            .config
            .gateway_host
            .as_deref()
            .unwrap_or("127.0.0.1");
        let port = self.config.gateway_port.unwrap_or(18789);
        let scheme = if self.config.gateway_tls.unwrap_or(false) {
            "wss"
        } else {
            "ws"
        };

        format!("{}://{}:{}", scheme, host, port)
    }
}
