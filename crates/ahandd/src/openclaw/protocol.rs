//! OpenClaw Gateway protocol types.
//!
//! Framed messages for communication with OpenClaw Gateway.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Protocol version
pub const PROTOCOL_VERSION: u32 = 3;

/// Gateway frame types (discriminated union)
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum GatewayFrame {
    /// Request frame
    #[serde(rename = "req")]
    Request(RequestFrame),
    /// Response frame
    #[serde(rename = "res")]
    Response(ResponseFrame),
    /// Event frame
    #[serde(rename = "event")]
    Event(EventFrame),
}

/// Request frame sent to Gateway
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestFrame {
    #[serde(rename = "type")]
    pub frame_type: String,
    pub id: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

impl RequestFrame {
    pub fn new(id: String, method: String, params: Option<serde_json::Value>) -> Self {
        Self {
            frame_type: "req".to_string(),
            id,
            method,
            params,
        }
    }
}

/// Response frame received from Gateway
#[derive(Debug, Clone, Deserialize)]
pub struct ResponseFrame {
    pub id: String,
    pub ok: bool,
    pub payload: Option<serde_json::Value>,
    pub error: Option<ErrorShape>,
}

/// Error shape in response
#[derive(Debug, Clone, Deserialize)]
pub struct ErrorShape {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub details: Option<serde_json::Value>,
}

/// Event frame received from Gateway
#[derive(Debug, Clone, Deserialize)]
pub struct EventFrame {
    pub event: String,
    #[serde(default)]
    pub payload: serde_json::Value,
    pub seq: Option<u64>,
}

/// Connect challenge event payload
#[derive(Debug, Clone, Deserialize)]
pub struct ConnectChallengePayload {
    pub nonce: Option<String>,
}

/// Connect params for handshake
#[derive(Debug, Clone, Serialize)]
pub struct ConnectParams {
    #[serde(rename = "minProtocol")]
    pub min_protocol: u32,
    #[serde(rename = "maxProtocol")]
    pub max_protocol: u32,
    pub client: ClientInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caps: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commands: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<HashMap<String, bool>>,
    #[serde(rename = "pathEnv", skip_serializing_if = "Option::is_none")]
    pub path_env: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<DeviceParams>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthParams>,
}

/// Client info for connect
#[derive(Debug, Clone, Serialize)]
pub struct ClientInfo {
    pub id: String,
    #[serde(rename = "displayName", skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub version: String,
    pub platform: String,
    pub mode: String,
    #[serde(rename = "instanceId", skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
}

/// Auth params for connect
#[derive(Debug, Clone, Serialize)]
pub struct AuthParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

/// Device identity params for connect (Ed25519 signing)
#[derive(Debug, Clone, Serialize)]
pub struct DeviceParams {
    pub id: String,
    #[serde(rename = "publicKey")]
    pub public_key: String,
    pub signature: String,
    #[serde(rename = "signedAt")]
    pub signed_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
}

/// HelloOk response from connect
#[derive(Debug, Clone, Deserialize)]
pub struct HelloOk {
    pub protocol: u32,
    pub server: ServerInfo,
    #[serde(default)]
    pub policy: PolicyInfo,
}

/// Server info from HelloOk
#[derive(Debug, Clone, Deserialize)]
pub struct ServerInfo {
    pub version: String,
    #[serde(rename = "connId")]
    pub conn_id: String,
}

/// Policy info from HelloOk
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PolicyInfo {
    #[serde(rename = "tickIntervalMs", default)]
    pub tick_interval_ms: Option<u64>,
}

/// node.invoke.request payload
#[derive(Debug, Clone, Deserialize)]
pub struct NodeInvokeRequest {
    pub id: String,
    #[serde(rename = "nodeId")]
    pub node_id: String,
    pub command: String,
    #[serde(rename = "paramsJSON")]
    pub params_json: Option<String>,
    #[serde(rename = "timeoutMs")]
    pub timeout_ms: Option<u64>,
    #[serde(rename = "idempotencyKey")]
    pub idempotency_key: Option<String>,
}

/// system.run params (decoded from paramsJSON)
#[derive(Debug, Clone, Deserialize)]
pub struct SystemRunParams {
    pub command: Vec<String>,
    #[serde(rename = "rawCommand")]
    pub raw_command: Option<String>,
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
    #[serde(rename = "timeoutMs")]
    pub timeout_ms: Option<u64>,
    #[serde(rename = "agentId")]
    pub agent_id: Option<String>,
    #[serde(rename = "sessionKey")]
    pub session_key: Option<String>,
    pub approved: Option<bool>,
    #[serde(rename = "approvalDecision")]
    pub approval_decision: Option<String>,
    #[serde(rename = "runId")]
    pub run_id: Option<String>,
}

/// system.which params
#[derive(Debug, Clone, Deserialize)]
pub struct SystemWhichParams {
    pub bins: Vec<String>,
}

/// system.which result
#[derive(Debug, Clone, Serialize)]
pub struct SystemWhichResult {
    pub bins: HashMap<String, String>,
}

/// system.execApprovals.get result
#[derive(Debug, Clone, Serialize)]
pub struct ExecApprovalsSnapshot {
    pub path: String,
    pub exists: bool,
    pub hash: String,
    pub file: ExecApprovalsFile,
}

/// system.execApprovals.set params
#[derive(Debug, Clone, Deserialize)]
pub struct ExecApprovalsSetParams {
    pub file: ExecApprovalsFile,
    #[serde(rename = "baseHash")]
    pub base_hash: Option<String>,
}

/// Exec approvals configuration file
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecApprovalsFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ask: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowlist: Option<Vec<AllowlistEntry>>,
    #[serde(default, rename = "autoAllowSkills", skip_serializing_if = "Option::is_none")]
    pub auto_allow_skills: Option<bool>,
}

/// Allowlist entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllowlistEntry {
    pub pattern: String,
    #[serde(rename = "agentId", skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(rename = "lastUsedMs", skip_serializing_if = "Option::is_none")]
    pub last_used_ms: Option<u64>,
    #[serde(rename = "useCount", skip_serializing_if = "Option::is_none")]
    pub use_count: Option<u32>,
}

/// node.invoke.result params (sent to Gateway)
#[derive(Debug, Clone, Serialize)]
pub struct NodeInvokeResult {
    pub id: String,
    #[serde(rename = "nodeId")]
    pub node_id: String,
    pub ok: bool,
    #[serde(rename = "payloadJSON", skip_serializing_if = "Option::is_none")]
    pub payload_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<InvokeError>,
}

/// Error details for failed invocations
#[derive(Debug, Clone, Serialize)]
pub struct InvokeError {
    pub code: String,
    pub message: String,
}

impl InvokeError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new("INVALID_REQUEST", message)
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new("UNAVAILABLE", message)
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new("TIMEOUT", message)
    }
}

/// node.event params (sent to Gateway)
#[derive(Debug, Clone, Serialize)]
pub struct NodeEvent {
    pub event: String,
    #[serde(rename = "payloadJSON", skip_serializing_if = "Option::is_none")]
    pub payload_json: Option<String>,
}

/// Run result (same structure as OpenClaw)
#[derive(Debug, Clone, Serialize)]
pub struct RunResult {
    #[serde(rename = "exitCode", skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(rename = "timedOut")]
    pub timed_out: bool,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Exec event payload (for exec.denied / exec.finished events)
#[derive(Debug, Clone, Serialize)]
pub struct ExecEventPayload {
    #[serde(rename = "sessionKey")]
    pub session_key: String,
    #[serde(rename = "runId")]
    pub run_id: String,
    pub host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(rename = "exitCode", skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(rename = "timedOut", skip_serializing_if = "Option::is_none")]
    pub timed_out: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Connect message capabilities and commands
#[derive(Debug, Clone, Serialize)]
pub struct NodeCapabilities {
    #[serde(rename = "nodeId")]
    pub node_id: String,
    #[serde(rename = "displayName", skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub platform: String,
    pub version: String,
    #[serde(rename = "coreVersion", skip_serializing_if = "Option::is_none")]
    pub core_version: Option<String>,
    pub caps: Vec<String>,
    pub commands: Vec<String>,
    #[serde(rename = "pathEnv", skip_serializing_if = "Option::is_none")]
    pub path_env: Option<String>,
}

/// Constants for output truncation
pub const OUTPUT_CAP: usize = 200_000;
pub const OUTPUT_EVENT_TAIL: usize = 20_000;
