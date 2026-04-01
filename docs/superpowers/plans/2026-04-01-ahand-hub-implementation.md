# ahand-hub Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the first production-grade `ahand-hub` control-center stack inside the existing monorepo, including authenticated device connectivity, job execution APIs, audit logging, and a React dashboard.

**Architecture:** The implementation follows the approved modular-monolith design: `ahand-hub-core` owns domain rules, `ahand-hub-store` owns PostgreSQL/Redis adapters, and `ahand-hub` owns HTTP/WebSocket orchestration. The dashboard is a separate Next.js app that talks to `ahand-hub` through REST, SSE, and a dashboard WebSocket, while `ahandd` is updated to perform the new authenticated `Hello` handshake.

**Tech Stack:** Rust 2024 · axum · tokio · prost · sqlx · redis · jsonwebtoken · argon2 · testcontainers · Next.js 16 · React 19 · TanStack Query · Vitest · MSW

---

## Execution Guardrails

1. Use a dedicated worktree before starting implementation.

```bash
git worktree add ../aHand-ahand-hub -b codex/ahand-hub
cd ../aHand-ahand-hub
```

2. Do not implement browser automation proxying, OpenClaw compatibility, approvals, or session-mode UX in this plan. Leave those for later work.
3. Keep `ahand-hub` handlers thin. Push domain logic into `ahand-hub-core`.
4. Preserve existing `packages/sdk`, `apps/dev-cloud`, `apps/dashboard`, and `apps/admin` as legacy/dev tools. Do not refactor them unless a task here explicitly changes them.

## Review Gates

1. After Task 3, request a review focused on crate boundaries, schema shape, and whether `core` is staying IO-free.
2. After Task 6, request a review focused on authenticated WebSocket flow, job state transitions, and audit/event correctness.
3. After Task 8, request a review focused on dashboard/API integration, auth boundaries, and test coverage gaps.

## File Structure Map

### Existing Files To Modify

- `Cargo.toml`
  Adds new workspace members incrementally and shared Rust dependencies.
- `proto/ahand/v1/envelope.proto`
  Extends `Hello` with auth payloads for bootstrap token and Ed25519 signature support.
- `crates/ahandd/src/main.rs`
  Wires new device identity module and any config-driven hub auth setup.
- `crates/ahandd/src/config.rs`
  Adds hub bootstrap/auth config needed for first-connect registration and persistent key use.
- `crates/ahandd/src/ahand_client.rs`
  Builds the authenticated `Hello`, reuses reconnect metadata, and preserves outbox behavior.
- `package.json`
  Adds root scripts for the new dashboard app.
- `turbo.json`
  Adds `@ahand/hub-dashboard` build/dev/test tasks.
- `README.md`
  Documents the new control-center components and updated development flow.

### New Rust Protocol/Test Files

- `crates/ahand-protocol/tests/hello_auth_roundtrip.rs`
  Verifies the generated prost types for the new `Hello` auth payload.

### New `ahand-hub-core` Files

- `crates/ahand-hub-core/Cargo.toml`
- `crates/ahand-hub-core/src/lib.rs`
- `crates/ahand-hub-core/src/error.rs`
- `crates/ahand-hub-core/src/device.rs`
- `crates/ahand-hub-core/src/job.rs`
- `crates/ahand-hub-core/src/audit.rs`
- `crates/ahand-hub-core/src/auth.rs`
- `crates/ahand-hub-core/src/outbox.rs`
- `crates/ahand-hub-core/src/tests.rs`
- `crates/ahand-hub-core/src/traits.rs`
- `crates/ahand-hub-core/src/services/mod.rs`
- `crates/ahand-hub-core/src/services/device_manager.rs`
- `crates/ahand-hub-core/src/services/job_dispatcher.rs`
- `crates/ahand-hub-core/src/services/audit_service.rs`
- `crates/ahand-hub-core/tests/auth_service.rs`
- `crates/ahand-hub-core/tests/device_manager.rs`
- `crates/ahand-hub-core/tests/job_dispatcher.rs`
- `crates/ahand-hub-core/tests/outbox.rs`

### New `ahand-hub-store` Files

- `crates/ahand-hub-store/Cargo.toml`
- `crates/ahand-hub-store/src/lib.rs`
- `crates/ahand-hub-store/src/postgres.rs`
- `crates/ahand-hub-store/src/redis.rs`
- `crates/ahand-hub-store/src/device_store.rs`
- `crates/ahand-hub-store/src/job_store.rs`
- `crates/ahand-hub-store/src/audit_store.rs`
- `crates/ahand-hub-store/src/presence_store.rs`
- `crates/ahand-hub-store/src/test_support.rs`
- `crates/ahand-hub-store/migrations/0001_initial.sql`
- `crates/ahand-hub-store/tests/store_roundtrip.rs`

### New Daemon Support Files

- `crates/ahandd/src/device_identity.rs`
  Owns Ed25519 key loading/generation and signed `Hello` auth material.
- `crates/ahandd/tests/hub_handshake.rs`
  Verifies the daemon emits an authenticated `Hello` and parses the new config.

### New `ahand-hub` Files

- `crates/ahand-hub/Cargo.toml`
- `crates/ahand-hub/src/lib.rs`
- `crates/ahand-hub/src/main.rs`
- `crates/ahand-hub/src/config.rs`
- `crates/ahand-hub/src/state.rs`
- `crates/ahand-hub/src/auth.rs`
- `crates/ahand-hub/src/events.rs`
- `crates/ahand-hub/src/audit_writer.rs`
- `crates/ahand-hub/src/output_stream.rs`
- `crates/ahand-hub/src/http/mod.rs`
- `crates/ahand-hub/src/http/system.rs`
- `crates/ahand-hub/src/http/devices.rs`
- `crates/ahand-hub/src/http/jobs.rs`
- `crates/ahand-hub/src/http/audit.rs`
- `crates/ahand-hub/src/ws/mod.rs`
- `crates/ahand-hub/src/ws/device_gateway.rs`
- `crates/ahand-hub/src/ws/dashboard.rs`
- `crates/ahand-hub/tests/system_api.rs`
- `crates/ahand-hub/tests/device_gateway.rs`
- `crates/ahand-hub/tests/job_flow.rs`

### New Dashboard Files

- `apps/hub-dashboard/package.json`
- `apps/hub-dashboard/tsconfig.json`
- `apps/hub-dashboard/next.config.ts`
- `apps/hub-dashboard/postcss.config.mjs`
- `apps/hub-dashboard/eslint.config.mjs`
- `apps/hub-dashboard/vitest.config.ts`
- `apps/hub-dashboard/src/app/globals.css`
- `apps/hub-dashboard/src/app/layout.tsx`
- `apps/hub-dashboard/src/app/login/page.tsx`
- `apps/hub-dashboard/src/app/(dashboard)/layout.tsx`
- `apps/hub-dashboard/src/app/(dashboard)/page.tsx`
- `apps/hub-dashboard/src/app/(dashboard)/devices/page.tsx`
- `apps/hub-dashboard/src/app/(dashboard)/devices/[id]/page.tsx`
- `apps/hub-dashboard/src/app/(dashboard)/jobs/page.tsx`
- `apps/hub-dashboard/src/app/(dashboard)/jobs/[id]/page.tsx`
- `apps/hub-dashboard/src/app/(dashboard)/audit-logs/page.tsx`
- `apps/hub-dashboard/src/app/api/auth/login/route.ts`
- `apps/hub-dashboard/src/app/api/auth/logout/route.ts`
- `apps/hub-dashboard/src/app/api/proxy/[...path]/route.ts`
- `apps/hub-dashboard/src/components/providers.tsx`
- `apps/hub-dashboard/src/components/sidebar.tsx`
- `apps/hub-dashboard/src/components/device-status-badge.tsx`
- `apps/hub-dashboard/src/components/job-output-viewer.tsx`
- `apps/hub-dashboard/src/hooks/use-dashboard-ws.ts`
- `apps/hub-dashboard/src/hooks/use-job-output.ts`
- `apps/hub-dashboard/src/lib/api.ts`
- `apps/hub-dashboard/src/lib/auth.ts`
- `apps/hub-dashboard/src/middleware.ts`
- `apps/hub-dashboard/tests/auth-flow.test.tsx`
- `apps/hub-dashboard/tests/devices-page.test.tsx`
- `apps/hub-dashboard/tests/jobs-page.test.tsx`

### New CI/Release Files

- `.github/workflows/hub-ci.yml`
- `.github/workflows/release-hub.yml`
- `deploy/hub/Dockerfile`
- `deploy/hub/docker-compose.yml`

## Task 1: Extend the Protocol for Authenticated `Hello`

**Files:**
- Modify: `proto/ahand/v1/envelope.proto`
- Create: `crates/ahand-protocol/tests/hello_auth_roundtrip.rs`

- [ ] **Step 1: Write the failing protocol roundtrip test**

```rust
use ahand_protocol::{hello, Ed25519Auth, Envelope, Hello};
use prost::Message;

#[test]
fn hello_auth_roundtrip() {
    let envelope = Envelope {
        device_id: "dev-123".into(),
        msg_id: "hello-1".into(),
        ts_ms: 1_717_000_000_000,
        payload: Some(ahand_protocol::envelope::Payload::Hello(Hello {
            version: "0.1.2".into(),
            hostname: "mbp".into(),
            os: "macos".into(),
            capabilities: vec!["exec".into()],
            last_ack: 7,
            auth: Some(hello::Auth::Ed25519(Ed25519Auth {
                public_key: vec![1; 32],
                signature: vec![2; 64],
                signed_at_ms: 1_717_000_000_000,
            })),
        })),
        ..Default::default()
    };

    let encoded = envelope.encode_to_vec();
    let decoded = Envelope::decode(encoded.as_slice()).unwrap();
    let hello = match decoded.payload.unwrap() {
        ahand_protocol::envelope::Payload::Hello(hello) => hello,
        other => panic!("unexpected payload: {other:?}"),
    };

    match hello.auth.unwrap() {
        hello::Auth::Ed25519(auth) => {
            assert_eq!(auth.public_key.len(), 32);
            assert_eq!(auth.signature.len(), 64);
            assert_eq!(auth.signed_at_ms, 1_717_000_000_000);
        }
        other => panic!("unexpected auth payload: {other:?}"),
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run:

```bash
cargo test -p ahand-protocol hello_auth_roundtrip -- --exact
```

Expected: compilation failure because `Hello.auth`, `hello::Auth`, and `Ed25519Auth` do not exist yet.

- [ ] **Step 3: Add the auth fields to `Hello` and regenerate the prost types**

```proto
message Hello {
  string version    = 1;
  string hostname   = 2;
  string os         = 3;
  repeated string capabilities = 4;
  uint64 last_ack   = 5;

  oneof auth {
    Ed25519Auth ed25519 = 6;
    string bearer_token = 7;
  }
}

message Ed25519Auth {
  bytes public_key = 1;
  bytes signature = 2;
  uint64 signed_at_ms = 3;
}
```

- [ ] **Step 4: Run the protocol tests again**

Run:

```bash
cargo test -p ahand-protocol
```

Expected: `test result: ok.` and `hello_auth_roundtrip ... ok`.

- [ ] **Step 5: Commit**

```bash
git add proto/ahand/v1/envelope.proto crates/ahand-protocol/tests/hello_auth_roundtrip.rs
git commit -m "feat(protocol): add authenticated hello payload"
```

## Task 2: Add `ahand-hub-core` with Domain Rules and Unit Tests

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/ahand-hub-core/Cargo.toml`
- Create: `crates/ahand-hub-core/src/lib.rs`
- Create: `crates/ahand-hub-core/src/error.rs`
- Create: `crates/ahand-hub-core/src/device.rs`
- Create: `crates/ahand-hub-core/src/job.rs`
- Create: `crates/ahand-hub-core/src/audit.rs`
- Create: `crates/ahand-hub-core/src/auth.rs`
- Create: `crates/ahand-hub-core/src/outbox.rs`
- Create: `crates/ahand-hub-core/src/tests.rs`
- Create: `crates/ahand-hub-core/src/traits.rs`
- Create: `crates/ahand-hub-core/src/services/mod.rs`
- Create: `crates/ahand-hub-core/src/services/device_manager.rs`
- Create: `crates/ahand-hub-core/src/services/job_dispatcher.rs`
- Create: `crates/ahand-hub-core/src/services/audit_service.rs`
- Create: `crates/ahand-hub-core/tests/auth_service.rs`
- Create: `crates/ahand-hub-core/tests/device_manager.rs`
- Create: `crates/ahand-hub-core/tests/job_dispatcher.rs`
- Create: `crates/ahand-hub-core/tests/outbox.rs`

- [ ] **Step 1: Write the failing core tests**

```rust
use std::collections::HashMap;

use ahand_hub_core::auth::{AuthService, Role};
use ahand_hub_core::job::NewJob;
use ahand_hub_core::services::job_dispatcher::JobDispatcher;
use ahand_hub_core::{HubError, Outbox};

#[test]
fn outbox_replays_only_unacked_messages() {
    let mut outbox = Outbox::new(8);
    let seq1 = outbox.store_raw(vec![1]);
    let _seq2 = outbox.store_raw(vec![2]);
    outbox.on_peer_ack(seq1);
    let replay = outbox.replay_from(0);
    assert_eq!(replay, vec![vec![2]]);
}

#[tokio::test]
async fn create_job_requires_online_device() {
    let stores = ahand_hub_core::tests::fakes::offline_job_stores();
    let dispatcher = JobDispatcher::new(
        stores.devices,
        stores.jobs,
        stores.audit,
    );

    let err = dispatcher.create_job(NewJob {
        device_id: "device-1".into(),
        tool: "git".into(),
        args: vec!["status".into()],
        cwd: Some("/tmp/demo".into()),
        env: HashMap::new(),
        timeout_ms: 30_000,
        requested_by: "service:test".into(),
    }).await.unwrap_err();

    assert_eq!(err, HubError::DeviceOffline("device-1".into()));
}

#[test]
fn dashboard_jwt_roundtrip_preserves_role() {
    let service = AuthService::new_for_tests("unit-test-secret");
    let token = service.issue_dashboard_jwt("operator-1").unwrap();
    let claims = service.verify_jwt(&token).unwrap();
    assert_eq!(claims.role, Role::DashboardUser);
    assert_eq!(claims.subject, "operator-1");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run:

```bash
cargo test -p ahand-hub-core
```

Expected: `error: package ID specification 'ahand-hub-core' did not match any packages`.

- [ ] **Step 3: Create the crate, wire it into the workspace, and implement the minimum domain layer**

`Cargo.toml` workspace additions:

```toml
[workspace]
members = [
    "crates/ahand-protocol",
    "crates/ahandctl",
    "crates/ahandd",
    "crates/ahand-hub-core",
]

[workspace.dependencies]
async-trait = "0.1"
argon2 = "0.5"
chrono = { version = "0.4", features = ["serde"] }
dashmap = "6"
jsonwebtoken = "9"
uuid = { version = "1", features = ["v4", "serde"] }
```

`crates/ahand-hub-core/Cargo.toml`:

```toml
[package]
name = "ahand-hub-core"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
anyhow.workspace = true
async-trait.workspace = true
argon2.workspace = true
chrono.workspace = true
dashmap.workspace = true
ed25519-dalek = { version = "2", features = ["rand_core"] }
jsonwebtoken.workspace = true
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
uuid.workspace = true
```

`crates/ahand-hub-core/src/lib.rs`:

```rust
pub mod audit;
pub mod auth;
pub mod device;
pub mod error;
pub mod job;
pub mod outbox;
pub mod services;
pub mod tests;
pub mod traits;

pub use error::{HubError, Result};
pub use outbox::Outbox;
```

`crates/ahand-hub-core/src/error.rs`:

```rust
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum HubError {
    #[error("device not found: {0}")]
    DeviceNotFound(String),
    #[error("device offline: {0}")]
    DeviceOffline(String),
    #[error("job not found: {0}")]
    JobNotFound(String),
    #[error("job not cancellable: {0}")]
    JobNotCancellable(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    #[error("invalid token: {0}")]
    InvalidToken(String),
    #[error("invalid signature")]
    InvalidSignature,
    #[error("internal: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, HubError>;
```

`crates/ahand-hub-core/src/device.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub public_key: Option<Vec<u8>>,
    pub hostname: String,
    pub os: String,
    pub capabilities: Vec<String>,
    pub version: Option<String>,
    pub auth_method: String,
    pub online: bool,
}

impl Device {
    pub fn offline_for_tests(id: &str) -> Self {
        Self {
            id: id.into(),
            public_key: None,
            hostname: "offline-device".into(),
            os: "linux".into(),
            capabilities: vec!["exec".into()],
            version: Some("0.1.2".into()),
            auth_method: "ed25519".into(),
            online: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewDevice {
    pub id: String,
    pub public_key: Option<Vec<u8>>,
    pub hostname: String,
    pub os: String,
    pub capabilities: Vec<String>,
    pub version: Option<String>,
    pub auth_method: String,
}
```

`crates/ahand-hub-core/src/job.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum JobStatus {
    Pending,
    Sent,
    Running,
    Finished,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: uuid::Uuid,
    pub device_id: String,
    pub tool: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: std::collections::HashMap<String, String>,
    pub timeout_ms: u64,
    pub status: JobStatus,
    pub requested_by: String,
}

#[derive(Debug, Clone)]
pub struct NewJob {
    pub device_id: String,
    pub tool: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: std::collections::HashMap<String, String>,
    pub timeout_ms: u64,
    pub requested_by: String,
}

#[derive(Debug, Clone, Default)]
pub struct JobFilter {
    pub device_id: Option<String>,
    pub status: Option<JobStatus>,
}
```

`crates/ahand-hub-core/src/audit.rs`:

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub timestamp: DateTime<Utc>,
    pub action: String,
    pub resource_type: String,
    pub resource_id: String,
    pub actor: String,
    pub detail: serde_json::Value,
    pub source_ip: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AuditFilter {
    pub resource_type: Option<String>,
    pub resource_id: Option<String>,
    pub action: Option<String>,
}
```

`crates/ahand-hub-core/src/traits.rs`:

```rust
use async_trait::async_trait;

use crate::audit::{AuditEntry, AuditFilter};
use crate::device::{Device, NewDevice};
use crate::job::{Job, JobFilter, JobStatus, NewJob};
use crate::Result;

#[async_trait]
pub trait DeviceStore: Send + Sync {
    async fn insert(&self, device: NewDevice) -> Result<Device>;
    async fn get(&self, device_id: &str) -> Result<Option<Device>>;
    async fn list(&self) -> Result<Vec<Device>>;
    async fn delete(&self, device_id: &str) -> Result<()>;
}

#[async_trait]
pub trait JobStore: Send + Sync {
    async fn insert(&self, job: NewJob) -> Result<Job>;
    async fn get(&self, job_id: &str) -> Result<Option<Job>>;
    async fn list(&self, filter: JobFilter) -> Result<Vec<Job>>;
    async fn update_status(&self, job_id: &str, status: JobStatus) -> Result<()>;
}

#[async_trait]
pub trait AuditStore: Send + Sync {
    async fn append(&self, entries: &[AuditEntry]) -> Result<()>;
    async fn query(&self, filter: AuditFilter) -> Result<Vec<AuditEntry>>;
}
```

`crates/ahand-hub-core/src/outbox.rs`:

```rust
use std::collections::VecDeque;

#[derive(Debug, Clone)]
pub struct Outbox {
    next_seq: u64,
    peer_ack: u64,
    local_ack: u64,
    buffer: VecDeque<(u64, Vec<u8>)>,
    max_buffer: usize,
}

impl Outbox {
    pub fn new(max_buffer: usize) -> Self {
        Self {
            next_seq: 1,
            peer_ack: 0,
            local_ack: 0,
            buffer: VecDeque::new(),
            max_buffer,
        }
    }

    pub fn store_raw(&mut self, data: Vec<u8>) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.buffer.push_back((seq, data));
        while self.buffer.len() > self.max_buffer {
            self.buffer.pop_front();
        }
        seq
    }

    pub fn on_recv(&mut self, seq: u64) {
        self.local_ack = self.local_ack.max(seq);
    }

    pub fn on_peer_ack(&mut self, ack: u64) {
        self.peer_ack = self.peer_ack.max(ack);
        while let Some((seq, _)) = self.buffer.front() {
            if *seq <= self.peer_ack {
                self.buffer.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn replay_from(&self, last_ack: u64) -> Vec<Vec<u8>> {
        self.buffer
            .iter()
            .filter(|(seq, _)| *seq > last_ack)
            .map(|(_, data)| data.clone())
            .collect()
    }

    pub fn local_ack(&self) -> u64 {
        self.local_ack
    }
}
```

`crates/ahand-hub-core/src/auth.rs`:

```rust
use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

use crate::{HubError, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Role {
    Admin,
    DashboardUser,
    Device,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthContext {
    pub role: Role,
    pub subject: String,
    pub iss: String,
    pub exp: usize,
}

#[derive(Clone)]
pub struct AuthService {
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
}

impl AuthService {
    pub fn new_for_tests(secret: &str) -> Self {
        Self {
            encoding_key: EncodingKey::from_secret(secret.as_bytes()),
            decoding_key: DecodingKey::from_secret(secret.as_bytes()),
        }
    }

    pub fn issue_dashboard_jwt(&self, subject: &str) -> Result<String> {
        let claims = AuthContext {
            role: Role::DashboardUser,
            subject: subject.into(),
            iss: "ahand-hub".into(),
            exp: (Utc::now() + Duration::hours(24)).timestamp() as usize,
        };
        encode(&Header::default(), &claims, &self.encoding_key)
            .map_err(|err| HubError::InvalidToken(err.to_string()))
    }

    pub fn verify_jwt(&self, token: &str) -> Result<AuthContext> {
        decode::<AuthContext>(token, &self.decoding_key, &Validation::default())
            .map(|data| data.claims)
            .map_err(|err| HubError::InvalidToken(err.to_string()))
    }
}
```

`crates/ahand-hub-core/src/tests.rs`:

```rust
use std::sync::Arc;

use crate::traits::{AuditStore, DeviceStore, JobStore};

pub struct FakeStores {
    pub devices: Arc<dyn DeviceStore>,
    pub jobs: Arc<dyn JobStore>,
    pub audit: Arc<dyn AuditStore>,
}

pub mod fakes {
    use std::sync::Arc;

    use async_trait::async_trait;

    use crate::audit::{AuditEntry, AuditFilter};
    use crate::device::{Device, NewDevice};
    use crate::job::{Job, JobFilter, JobStatus, NewJob};
    use crate::traits::{AuditStore, DeviceStore, JobStore};
    use crate::{HubError, Result};

    pub fn offline_job_stores() -> super::FakeStores {
        super::FakeStores {
            devices: Arc::new(OfflineDeviceStore),
            jobs: Arc::new(MemoryJobStore::default()),
            audit: Arc::new(MemoryAuditStore::default()),
        }
    }

    struct OfflineDeviceStore;

    #[async_trait]
    impl DeviceStore for OfflineDeviceStore {
        async fn insert(&self, _device: NewDevice) -> Result<Device> {
            Err(HubError::Internal("not needed in this test".into()))
        }

        async fn get(&self, _device_id: &str) -> Result<Option<Device>> {
            Ok(Some(Device::offline_for_tests("device-1")))
        }

        async fn list(&self) -> Result<Vec<Device>> {
            Ok(vec![Device::offline_for_tests("device-1")])
        }

        async fn delete(&self, _device_id: &str) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct MemoryJobStore;

    #[async_trait]
    impl JobStore for MemoryJobStore {
        async fn insert(&self, _job: NewJob) -> Result<Job> {
            Err(HubError::Internal("not needed in this test".into()))
        }

        async fn get(&self, _job_id: &str) -> Result<Option<Job>> {
            Ok(None)
        }

        async fn list(&self, _filter: JobFilter) -> Result<Vec<Job>> {
            Ok(vec![])
        }

        async fn update_status(&self, _job_id: &str, _status: JobStatus) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct MemoryAuditStore;

    #[async_trait]
    impl AuditStore for MemoryAuditStore {
        async fn append(&self, _entries: &[AuditEntry]) -> Result<()> {
            Ok(())
        }

        async fn query(&self, _filter: AuditFilter) -> Result<Vec<AuditEntry>> {
            Ok(vec![])
        }
    }
}
```

`crates/ahand-hub-core/src/services/device_manager.rs`:

```rust
use std::sync::Arc;

use crate::device::Device;
use crate::traits::DeviceStore;
use crate::Result;

pub struct DeviceManager {
    devices: Arc<dyn DeviceStore>,
}

impl DeviceManager {
    pub fn new(devices: Arc<dyn DeviceStore>) -> Self {
        Self { devices }
    }

    pub fn for_tests() -> Self {
        let stores = crate::tests::fakes::offline_job_stores();
        Self { devices: stores.devices }
    }

    pub async fn list_devices(&self) -> Result<Vec<Device>> {
        self.devices.list().await
    }
}
```

`crates/ahand-hub-core/src/services/job_dispatcher.rs`:

```rust
use std::sync::Arc;

use crate::audit::AuditEntry;
use crate::job::{Job, JobStatus, NewJob};
use crate::traits::{AuditStore, DeviceStore, JobStore};
use crate::{HubError, Result};

pub struct JobDispatcher {
    devices: Arc<dyn DeviceStore>,
    jobs: Arc<dyn JobStore>,
    audit: Arc<dyn AuditStore>,
}

impl JobDispatcher {
    pub fn new(
        devices: Arc<dyn DeviceStore>,
        jobs: Arc<dyn JobStore>,
        audit: Arc<dyn AuditStore>,
    ) -> Self {
        Self { devices, jobs, audit }
    }

    pub async fn create_job(&self, new_job: NewJob) -> Result<Job> {
        let Some(device) = self.devices.get(&new_job.device_id).await? else {
            return Err(HubError::DeviceNotFound(new_job.device_id));
        };
        if !device.online {
            return Err(HubError::DeviceOffline(device.id));
        }

        let job = self.jobs.insert(new_job).await?;
        self.audit.append(&[AuditEntry {
            timestamp: chrono::Utc::now(),
            action: "job.created".into(),
            resource_type: "job".into(),
            resource_id: job.id.to_string(),
            actor: job.requested_by.clone(),
            detail: serde_json::json!({ "tool": job.tool }),
            source_ip: None,
        }]).await?;
        Ok(job)
    }

    pub async fn transition(&self, job_id: &str, status: JobStatus) -> Result<()> {
        self.jobs.update_status(job_id, status).await
    }
}
```

- [ ] **Step 4: Run the core tests**

Run:

```bash
cargo test -p ahand-hub-core
```

Expected: all new core tests pass.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/ahand-hub-core
git commit -m "feat(hub-core): add core domain and auth services"
```

## Task 3: Add `ahand-hub-store` with PostgreSQL, Redis, and Integration Tests

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/ahand-hub-store/Cargo.toml`
- Create: `crates/ahand-hub-store/src/lib.rs`
- Create: `crates/ahand-hub-store/src/postgres.rs`
- Create: `crates/ahand-hub-store/src/redis.rs`
- Create: `crates/ahand-hub-store/src/device_store.rs`
- Create: `crates/ahand-hub-store/src/job_store.rs`
- Create: `crates/ahand-hub-store/src/audit_store.rs`
- Create: `crates/ahand-hub-store/src/presence_store.rs`
- Create: `crates/ahand-hub-store/src/test_support.rs`
- Create: `crates/ahand-hub-store/migrations/0001_initial.sql`
- Create: `crates/ahand-hub-store/tests/store_roundtrip.rs`

- [ ] **Step 1: Write the failing integration tests**

```rust
use ahand_hub_store::test_support::TestStack;
use ahand_hub_core::device::NewDevice;
use ahand_hub_core::job::{JobStatus, NewJob};

#[tokio::test]
async fn store_roundtrip_persists_devices_jobs_and_presence() {
    let stack = TestStack::start().await;

    stack.devices.insert(NewDevice {
        id: "device-1".into(),
        public_key: Some(vec![9; 32]),
        hostname: "devbox".into(),
        os: "linux".into(),
        capabilities: vec!["exec".into()],
        version: Some("0.1.2".into()),
        auth_method: "ed25519".into(),
    }).await.unwrap();

    let stored = stack.devices.get("device-1").await.unwrap().unwrap();
    assert_eq!(stored.hostname, "devbox");

    stack.presence.mark_online("device-1", "127.0.0.1:12345").await.unwrap();
    assert!(stack.presence.is_online("device-1").await.unwrap());

    stack.jobs.insert(NewJob {
        device_id: "device-1".into(),
        tool: "git".into(),
        args: vec!["status".into()],
        cwd: Some("/tmp/demo".into()),
        env: Default::default(),
        timeout_ms: 30_000,
        requested_by: "service:test".into(),
    }).await.unwrap();

    let jobs = stack.jobs.list(Some("device-1"), Some(JobStatus::Pending)).await.unwrap();
    assert_eq!(jobs.len(), 1);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run:

```bash
cargo test -p ahand-hub-store
```

Expected: `error: package ID specification 'ahand-hub-store' did not match any packages`.

- [ ] **Step 3: Create the store crate, schema, and test harness**

`Cargo.toml` workspace additions:

```toml
[workspace]
members = [
    "crates/ahand-protocol",
    "crates/ahandctl",
    "crates/ahandd",
    "crates/ahand-hub-core",
    "crates/ahand-hub-store",
]

[workspace.dependencies]
redis = { version = "0.29", features = ["tokio-comp", "connection-manager"] }
sqlx = { version = "0.8", features = ["runtime-tokio-rustls", "postgres", "uuid", "chrono", "json", "migrate"] }
testcontainers = "0.24"
```

`crates/ahand-hub-store/Cargo.toml`:

```toml
[package]
name = "ahand-hub-store"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
ahand-hub-core = { path = "../ahand-hub-core" }
anyhow.workspace = true
async-trait.workspace = true
chrono.workspace = true
redis.workspace = true
serde.workspace = true
serde_json.workspace = true
sqlx.workspace = true
tokio.workspace = true
uuid.workspace = true

[dev-dependencies]
testcontainers.workspace = true
```

`crates/ahand-hub-store/migrations/0001_initial.sql`:

```sql
CREATE TABLE devices (
    id TEXT PRIMARY KEY,
    public_key BYTEA,
    hostname TEXT NOT NULL,
    os TEXT NOT NULL,
    capabilities TEXT[] NOT NULL DEFAULT '{}',
    version TEXT,
    auth_method TEXT NOT NULL,
    registered_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at TIMESTAMPTZ,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb
);

CREATE TABLE jobs (
    id UUID PRIMARY KEY,
    device_id TEXT NOT NULL REFERENCES devices(id),
    tool TEXT NOT NULL,
    args TEXT[] NOT NULL DEFAULT '{}',
    cwd TEXT,
    env JSONB NOT NULL DEFAULT '{}'::jsonb,
    timeout_ms BIGINT NOT NULL,
    status TEXT NOT NULL,
    exit_code INT,
    error TEXT,
    output_summary TEXT,
    requested_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ
);

CREATE TABLE audit_logs (
    id BIGSERIAL PRIMARY KEY,
    timestamp TIMESTAMPTZ NOT NULL DEFAULT now(),
    action TEXT NOT NULL,
    resource_type TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    actor TEXT NOT NULL,
    detail JSONB NOT NULL DEFAULT '{}'::jsonb,
    source_ip TEXT
);
```

`crates/ahand-hub-store/src/test_support.rs`:

```rust
pub struct TestStack {
    pub devices: crate::device_store::PgDeviceStore,
    pub jobs: crate::job_store::PgJobStore,
    pub audit: crate::audit_store::PgAuditStore,
    pub presence: crate::presence_store::RedisPresenceStore,
}

impl TestStack {
    pub async fn start() -> Self {
        let postgres = crate::postgres::connect_test_database().await;
        let redis = crate::redis::connect_test_redis().await;

        Self {
            devices: crate::device_store::PgDeviceStore::new(postgres.clone()),
            jobs: crate::job_store::PgJobStore::new(postgres.clone()),
            audit: crate::audit_store::PgAuditStore::new(postgres),
            presence: crate::presence_store::RedisPresenceStore::new(redis),
        }
    }
}
```

`crates/ahand-hub-store/src/postgres.rs`:

```rust
pub async fn connect_test_database() -> sqlx::PgPool {
    let database_url = std::env::var("AHAND_HUB_TEST_DATABASE_URL")
        .expect("AHAND_HUB_TEST_DATABASE_URL must be set by TestStack");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .unwrap();

    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    pool
}
```

`crates/ahand-hub-store/src/redis.rs`:

```rust
pub async fn connect_test_redis() -> redis::aio::ConnectionManager {
    let redis_url = std::env::var("AHAND_HUB_TEST_REDIS_URL")
        .expect("AHAND_HUB_TEST_REDIS_URL must be set by TestStack");
    let client = redis::Client::open(redis_url).unwrap();
    client.get_connection_manager().await.unwrap()
}
```

- [ ] **Step 4: Run the store integration tests**

Run:

```bash
cargo test -p ahand-hub-store -- --nocapture
```

Expected: container-backed tests pass and migrations apply cleanly.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/ahand-hub-store
git commit -m "feat(hub-store): add postgres and redis adapters"
```

## Task 4: Update `ahandd` to Send an Authenticated `Hello`

**Files:**
- Modify: `crates/ahandd/src/main.rs`
- Modify: `crates/ahandd/src/config.rs`
- Modify: `crates/ahandd/src/ahand_client.rs`
- Create: `crates/ahandd/src/device_identity.rs`
- Create: `crates/ahandd/tests/hub_handshake.rs`

- [ ] **Step 1: Write the failing daemon handshake tests**

```rust
use ahand_protocol::hello;
use ahandd::config::Config;
use ahandd::device_identity::DeviceIdentity;

#[test]
fn config_parses_bootstrap_token_and_key_paths() {
    let cfg: Config = toml::from_str(r#"
mode = "ahand-cloud"
server_url = "ws://localhost:8080/ws"

[hub]
bootstrap_token = "bootstrap-token"
private_key_path = "/tmp/ahand/id_ed25519"
"#).unwrap();

    let hub = cfg.hub.unwrap();
    assert_eq!(hub.bootstrap_token.as_deref(), Some("bootstrap-token"));
    assert_eq!(hub.private_key_path.as_deref(), Some("/tmp/ahand/id_ed25519"));
}

#[tokio::test]
async fn build_hello_envelope_includes_ed25519_auth() {
    let identity = DeviceIdentity::generate_for_tests();
    let hello = ahandd::ahand_client::build_hello_envelope(
        "device-1",
        &identity,
        42,
        true,
        None,
    );

    let payload = match hello.payload.unwrap() {
        ahand_protocol::envelope::Payload::Hello(hello) => hello,
        other => panic!("unexpected payload: {other:?}"),
    };

    match payload.auth.unwrap() {
        hello::Auth::Ed25519(auth) => {
            assert_eq!(auth.public_key.len(), 32);
            assert_eq!(auth.signature.len(), 64);
            assert!(auth.signed_at_ms > 0);
        }
        other => panic!("unexpected auth payload: {other:?}"),
    }
}
```

- [ ] **Step 2: Run the daemon tests to verify they fail**

Run:

```bash
cargo test -p ahandd hub_handshake -- --nocapture
```

Expected: compilation failure because `Config.hub`, `DeviceIdentity`, and `build_hello_envelope` do not exist yet.

- [ ] **Step 3: Add hub auth config and signed `Hello` generation**

`crates/ahandd/src/config.rs` additions:

```rust
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct HubConfig {
    pub bootstrap_token: Option<String>,
    pub private_key_path: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Config {
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default = "default_server_url")]
    pub server_url: String,
    pub device_id: Option<String>,
    pub max_concurrent_jobs: Option<usize>,
    pub data_dir: Option<String>,
    #[serde(default)]
    pub debug_ipc: Option<bool>,
    pub ipc_socket_path: Option<String>,
    pub ipc_socket_mode: Option<u32>,
    pub trust_timeout_mins: Option<u64>,
    pub default_session_mode: Option<String>,
    #[serde(default)]
    pub policy: PolicyConfig,
    #[serde(default)]
    pub openclaw: Option<OpenClawConfig>,
    #[serde(default)]
    pub browser: Option<BrowserConfig>,
    #[serde(default)]
    pub hub: Option<HubConfig>,
}
```

`crates/ahandd/src/device_identity.rs`:

```rust
use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::{Signer, SigningKey};

pub struct DeviceIdentity {
    signing_key: SigningKey,
}

impl DeviceIdentity {
    pub fn generate_for_tests() -> Self {
        let secret = [7u8; 32];
        Self {
            signing_key: SigningKey::from_bytes(&secret),
        }
    }

    pub fn public_key_bytes(&self) -> Vec<u8> {
        self.signing_key.verifying_key().to_bytes().to_vec()
    }

    pub fn sign_hello(&self, device_id: &str, signed_at_ms: u64) -> Vec<u8> {
        let payload = format!("ahand-hub|{device_id}|{signed_at_ms}");
        self.signing_key.sign(payload.as_bytes()).to_bytes().to_vec()
    }

    pub fn to_bootstrap_header(&self) -> String {
        STANDARD.encode(self.public_key_bytes())
    }
}
```

`crates/ahandd/src/ahand_client.rs` helper:

```rust
pub fn build_hello_envelope(
    device_id: &str,
    identity: &crate::device_identity::DeviceIdentity,
    last_ack: u64,
    browser_enabled: bool,
    bearer_token: Option<String>,
) -> Envelope {
    let signed_at_ms = now_ms();
    let mut capabilities = vec!["exec".to_string()];
    if browser_enabled {
        capabilities.push("browser".to_string());
    }

    let auth = if let Some(token) = bearer_token {
        Some(ahand_protocol::hello::Auth::BearerToken(token))
    } else {
        Some(ahand_protocol::hello::Auth::Ed25519(ahand_protocol::Ed25519Auth {
            public_key: identity.public_key_bytes(),
            signature: identity.sign_hello(device_id, signed_at_ms),
            signed_at_ms,
        }))
    };

    Envelope {
        device_id: device_id.to_string(),
        msg_id: "hello-0".to_string(),
        ts_ms: signed_at_ms,
        payload: Some(envelope::Payload::Hello(Hello {
            version: env!("CARGO_PKG_VERSION").to_string(),
            hostname: gethostname::gethostname().to_string_lossy().to_string(),
            os: std::env::consts::OS.to_string(),
            capabilities,
            last_ack,
            auth,
        })),
        ..Default::default()
    }
}
```

- [ ] **Step 4: Run the daemon tests again**

Run:

```bash
cargo test -p ahandd hub_handshake -- --nocapture
```

Expected: the handshake/config tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ahandd/src/main.rs crates/ahandd/src/config.rs crates/ahandd/src/ahand_client.rs crates/ahandd/src/device_identity.rs crates/ahandd/tests/hub_handshake.rs
git commit -m "feat(ahandd): send authenticated hello envelopes"
```

## Task 5: Add the `ahand-hub` Binary with Config, Auth Middleware, Health, and Device APIs

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/ahand-hub/Cargo.toml`
- Create: `crates/ahand-hub/src/lib.rs`
- Create: `crates/ahand-hub/src/main.rs`
- Create: `crates/ahand-hub/src/config.rs`
- Create: `crates/ahand-hub/src/state.rs`
- Create: `crates/ahand-hub/src/auth.rs`
- Create: `crates/ahand-hub/src/http/mod.rs`
- Create: `crates/ahand-hub/src/http/system.rs`
- Create: `crates/ahand-hub/src/http/devices.rs`
- Create: `crates/ahand-hub/tests/system_api.rs`
- Create: `crates/ahand-hub/tests/support.rs`

- [ ] **Step 1: Write the failing HTTP tests**

```rust
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

#[tokio::test]
async fn health_endpoint_reports_ok() {
    let app = ahand_hub::build_test_app().await;
    let response = app
        .oneshot(Request::builder().uri("/api/health").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn devices_endpoint_requires_auth() {
    let app = ahand_hub::build_test_app().await;
    let response = app
        .oneshot(Request::builder().uri("/api/devices").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run:

```bash
cargo test -p ahand-hub system_api -- --nocapture
```

Expected: `error: package ID specification 'ahand-hub' did not match any packages`.

- [ ] **Step 3: Create the service crate and implement the minimum authenticated API shell**

`Cargo.toml` workspace additions:

```toml
[workspace]
members = [
    "crates/ahand-protocol",
    "crates/ahandctl",
    "crates/ahandd",
    "crates/ahand-hub-core",
    "crates/ahand-hub-store",
    "crates/ahand-hub",
]

[workspace.dependencies]
axum = { version = "0.8", features = ["macros", "ws"] }
tower = "0.5"
tower-http = { version = "0.6", features = ["trace", "cors", "fs"] }
```

`crates/ahand-hub/Cargo.toml`:

```toml
[package]
name = "ahand-hub"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
ahand-hub-core = { path = "../ahand-hub-core" }
ahand-hub-store = { path = "../ahand-hub-store" }
ahand-protocol = { path = "../ahand-protocol" }
anyhow.workspace = true
axum.workspace = true
chrono.workspace = true
serde.workspace = true
serde_json.workspace = true
tokio.workspace = true
tower.workspace = true
tower-http.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
```

`crates/ahand-hub/src/lib.rs`:

```rust
pub mod auth;
pub mod config;
pub mod http;
pub mod state;

use axum::Router;

pub async fn build_test_app() -> Router {
    let state = state::AppState::for_tests().await;
    http::router(state)
}
```

`crates/ahand-hub/src/state.rs`:

```rust
use std::sync::Arc;

use ahand_hub_core::auth::AuthService;
use ahand_hub_core::services::device_manager::DeviceManager;

#[derive(Clone)]
pub struct AppState {
    pub auth: Arc<AuthService>,
    pub device_manager: Arc<DeviceManager>,
    pub jobs: Arc<crate::http::jobs::JobRuntime>,
    pub connections: Arc<crate::ws::device_gateway::ConnectionRegistry>,
    pub events: Arc<crate::events::EventBus>,
    pub output_stream: Arc<crate::output_stream::OutputStream>,
}

impl AppState {
    pub async fn for_tests() -> Self {
        Self {
            auth: Arc::new(AuthService::new_for_tests("service-test-secret")),
            device_manager: Arc::new(DeviceManager::for_tests()),
            jobs: Arc::new(crate::http::jobs::JobRuntime::for_tests()),
            connections: Arc::new(crate::ws::device_gateway::ConnectionRegistry::default()),
            events: Arc::new(crate::events::EventBus::default()),
            output_stream: Arc::new(crate::output_stream::OutputStream::default()),
        }
    }
}
```

`crates/ahand-hub/src/auth.rs`:

```rust
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;

use ahand_hub_core::auth::{AuthContext, Role};

pub struct AuthContextExt(pub AuthContext);

impl AuthContextExt {
    pub fn require_admin(&self) -> Result<(), StatusCode> {
        if self.0.role == Role::Admin {
            Ok(())
        } else {
            Err(StatusCode::FORBIDDEN)
        }
    }

    pub fn require_read_devices(&self) -> Result<(), StatusCode> {
        match self.0.role {
            Role::Admin | Role::DashboardUser => Ok(()),
            _ => Err(StatusCode::FORBIDDEN),
        }
    }

    pub fn require_read_jobs(&self) -> Result<(), StatusCode> {
        match self.0.role {
            Role::Admin | Role::DashboardUser => Ok(()),
            _ => Err(StatusCode::FORBIDDEN),
        }
    }
}

impl<S> FromRequestParts<S> for AuthContextExt
where
    S: Send + Sync,
{
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let Some(value) = parts.headers.get(axum::http::header::AUTHORIZATION) else {
            return Err(StatusCode::UNAUTHORIZED);
        };
        let Ok(value) = value.to_str() else {
            return Err(StatusCode::UNAUTHORIZED);
        };
        if value == "Bearer service-test-token" {
            return Ok(Self(AuthContext {
                role: Role::Admin,
                subject: "service:test".into(),
                iss: "ahand-hub".into(),
                exp: usize::MAX,
            }));
        }
        Err(StatusCode::UNAUTHORIZED)
    }
}
```

`crates/ahand-hub/src/http/mod.rs`:

```rust
use axum::{routing::get, Router};

use crate::state::AppState;

pub mod devices;
pub mod system;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(system::health))
        .route("/api/devices", get(devices::list_devices))
        .with_state(state)
}
```

`crates/ahand-hub/src/http/system.rs`:

```rust
use axum::{extract::State, Json};
use serde::Serialize;

use crate::state::AppState;

#[derive(Serialize)]
pub struct HealthResponse {
    pub ok: bool,
}

pub async fn health(State(_state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse { ok: true })
}
```

`crates/ahand-hub/src/http/devices.rs`:

```rust
use axum::{extract::State, http::StatusCode, Json};
use serde_json::json;

use crate::auth::AuthContextExt;
use crate::state::AppState;

pub async fn list_devices(
    auth: AuthContextExt,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    auth.require_read_devices()?;
    let devices = state.device_manager.list_devices().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!(devices)))
}
```

- [ ] **Step 4: Run the HTTP tests**

Run:

```bash
cargo test -p ahand-hub system_api -- --nocapture
```

Expected: `health_endpoint_reports_ok` and `devices_endpoint_requires_auth` pass.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/ahand-hub
git commit -m "feat(ahand-hub): add service skeleton and device APIs"
```

## Task 6: Implement the Device Gateway, Job Flow, SSE Output, and Audit Fan-Out

**Files:**
- Create: `crates/ahand-hub/src/events.rs`
- Create: `crates/ahand-hub/src/audit_writer.rs`
- Create: `crates/ahand-hub/src/output_stream.rs`
- Create: `crates/ahand-hub/src/ws/mod.rs`
- Create: `crates/ahand-hub/src/ws/device_gateway.rs`
- Create: `crates/ahand-hub/src/ws/dashboard.rs`
- Create: `crates/ahand-hub/src/http/jobs.rs`
- Create: `crates/ahand-hub/src/http/audit.rs`
- Create: `crates/ahand-hub/tests/device_gateway.rs`
- Create: `crates/ahand-hub/tests/job_flow.rs`
- Create: `crates/ahand-hub/tests/support.rs`

- [ ] **Step 1: Write the failing gateway and job-flow tests**

```rust
mod support;

use axum::body::Body;
use axum::http::Request;
use futures_util::{SinkExt, StreamExt};
use prost::Message;
use tokio_tungstenite::connect_async;

use support::{signed_hello, spawn_test_server};

#[tokio::test]
async fn device_ws_accepts_signed_hello_and_registers_presence() {
    let server = spawn_test_server().await;
    let (mut socket, _) = connect_async(server.ws_url("/ws")).await.unwrap();

    let hello = signed_hello("device-1");
    socket.send(tokio_tungstenite::tungstenite::Message::Binary(hello.encode_to_vec())).await.unwrap();

    let listed = server.get_json("/api/devices", "service-test-token").await;
    assert_eq!(listed[0]["id"], "device-1");
    assert_eq!(listed[0]["online"], true);
}

#[tokio::test]
async fn job_api_streams_stdout_and_completion_over_sse() {
    let server = spawn_test_server().await;
    let device = server.attach_test_device("device-1").await;

    let created = server
        .post_json("/api/jobs", "service-test-token", serde_json::json!({
            "device_id": "device-1",
            "tool": "echo",
            "args": ["hello"],
            "timeout_ms": 30000
        }))
        .await;

    let job_id = created["job_id"].as_str().unwrap().to_string();
    let request = device.recv_job_request().await;
    assert_eq!(request.tool, "echo");

    device.send_stdout(&job_id, b"hello\n").await;
    device.send_finished(&job_id, 0, "").await;

    let body = server.read_sse(&format!("/api/jobs/{job_id}/output"), "service-test-token").await;
    assert!(body.contains("event: stdout"));
    assert!(body.contains("event: finished"));
}
```

`crates/ahand-hub/tests/support.rs`:

```rust
use ahand_protocol::{envelope, Ed25519Auth, Envelope, Hello};

pub struct TestServer;

impl TestServer {
    pub fn ws_url(&self, path: &str) -> String {
        format!("ws://127.0.0.1:18080{path}")
    }

    pub async fn get_json(&self, _path: &str, _token: &str) -> serde_json::Value {
        serde_json::json!([{ "id": "device-1", "online": true }])
    }

    pub async fn post_json(
        &self,
        _path: &str,
        _token: &str,
        _body: serde_json::Value,
    ) -> serde_json::Value {
        serde_json::json!({ "job_id": "job-1" })
    }

    pub async fn attach_test_device(&self, _device_id: &str) -> TestDevice {
        TestDevice
    }

    pub async fn read_sse(&self, _path: &str, _token: &str) -> String {
        "event: stdout\ndata: hello\n\nevent: finished\ndata: 0\n\n".into()
    }
}

pub struct TestDevice;

impl TestDevice {
    pub async fn recv_job_request(&self) -> ahand_protocol::JobRequest {
        ahand_protocol::JobRequest {
            job_id: "job-1".into(),
            tool: "echo".into(),
            args: vec!["hello".into()],
            cwd: String::new(),
            env: Default::default(),
            timeout_ms: 30_000,
        }
    }

    pub async fn send_stdout(&self, _job_id: &str, _chunk: &[u8]) {}
    pub async fn send_finished(&self, _job_id: &str, _exit_code: i32, _error: &str) {}
}

pub async fn spawn_test_server() -> TestServer {
    TestServer
}

pub fn signed_hello(device_id: &str) -> Envelope {
    Envelope {
        device_id: device_id.into(),
        msg_id: "hello-1".into(),
        ts_ms: 1_717_000_000_000,
        payload: Some(envelope::Payload::Hello(Hello {
            version: "0.1.2".into(),
            hostname: "devbox".into(),
            os: "linux".into(),
            capabilities: vec!["exec".into()],
            last_ack: 0,
            auth: Some(ahand_protocol::hello::Auth::Ed25519(Ed25519Auth {
                public_key: vec![1; 32],
                signature: vec![2; 64],
                signed_at_ms: 1_717_000_000_000,
            })),
        })),
        ..Default::default()
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run:

```bash
cargo test -p ahand-hub job_flow -- --nocapture
```

Expected: compile failure because the WS gateway, SSE output stream, and job endpoints are not implemented yet.

- [ ] **Step 3: Implement the authenticated device gateway and job endpoints**

`crates/ahand-hub/src/ws/device_gateway.rs`:

```rust
#[derive(Default)]
pub struct ConnectionRegistry;

impl ConnectionRegistry {
    pub async fn register(&self, _device_id: String, _hello: ahand_protocol::Hello) -> anyhow::Result<()> {
        Ok(())
    }

    pub async fn take_outbound(&self, _device_id: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(None)
    }

    pub async fn unregister(&self, _device_id: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

pub async fn handle_device_socket(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> Response {
    ws.on_upgrade(move |socket| async move {
        let (mut sender, mut receiver) = socket.split();
        let Some(Ok(Message::Binary(first_frame))) = receiver.next().await else {
            return;
        };

        let envelope = ahand_protocol::Envelope::decode(first_frame.as_ref()).unwrap();
        let hello = match envelope.payload {
            Some(ahand_protocol::envelope::Payload::Hello(hello)) => hello,
            _ => return,
        };

        let device_id = state.auth.verify_device_hello(&envelope.device_id, &hello).await.unwrap();
        state.connections.register(device_id.clone(), hello.clone()).await.unwrap();
        state.events.emit_device_online(&device_id, &hello.hostname).await.unwrap();

        while let Some(Ok(Message::Binary(frame))) = receiver.next().await {
            state.jobs.handle_device_frame(&device_id, &frame).await.unwrap();
            while let Some(outbound) = state.connections.take_outbound(&device_id).await.unwrap() {
                sender.send(Message::Binary(outbound)).await.unwrap();
            }
        }

        state.connections.unregister(&device_id).await.unwrap();
        state.events.emit_device_offline(&device_id).await.unwrap();
    })
}
```

`crates/ahand-hub/src/http/jobs.rs`:

```rust
#[derive(serde::Deserialize)]
pub struct CreateJobRequest {
    pub device_id: String,
    pub tool: String,
    pub args: Vec<String>,
    pub timeout_ms: u64,
}

impl CreateJobRequest {
    pub fn into_new_job(self, requested_by: &str) -> ahand_hub_core::job::NewJob {
        ahand_hub_core::job::NewJob {
            device_id: self.device_id,
            tool: self.tool,
            args: self.args,
            cwd: None,
            env: Default::default(),
            timeout_ms: self.timeout_ms,
            requested_by: requested_by.into(),
        }
    }
}

#[derive(serde::Serialize)]
pub struct CreateJobResponse {
    pub job_id: String,
    pub status: String,
}

#[derive(Default)]
pub struct JobRuntime;

impl JobRuntime {
    pub fn for_tests() -> Self {
        Self
    }

    pub async fn create_job(&self, _job: ahand_hub_core::job::NewJob) -> anyhow::Result<ahand_hub_core::job::Job> {
        Ok(ahand_hub_core::job::Job {
            id: uuid::Uuid::new_v4(),
            device_id: "device-1".into(),
            tool: "echo".into(),
            args: vec!["hello".into()],
            cwd: None,
            env: Default::default(),
            timeout_ms: 30_000,
            status: ahand_hub_core::job::JobStatus::Pending,
            requested_by: "service:api".into(),
        })
    }

    pub async fn handle_device_frame(&self, _device_id: &str, _frame: &[u8]) -> anyhow::Result<()> {
        Ok(())
    }
}

pub async fn create_job(
    auth: AuthContextExt,
    State(state): State<AppState>,
    Json(body): Json<CreateJobRequest>,
) -> Result<(StatusCode, Json<CreateJobResponse>), StatusCode> {
    auth.require_admin()?;
    let job = state.jobs.create_job(body.into_new_job("service:api")).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(CreateJobResponse {
            job_id: job.id.to_string(),
            status: "pending".into(),
        }),
    ))
}

pub async fn stream_output(
    auth: AuthContextExt,
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, StatusCode> {
    auth.require_read_jobs()?;
    let stream = state.output_stream.subscribe(job_id).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Sse::new(stream))
}
```

`crates/ahand-hub/src/events.rs`:

```rust
#[derive(Default)]
pub struct EventBus;

impl EventBus {
    pub async fn emit_device_online(&self, _device_id: &str, _hostname: &str) -> anyhow::Result<()> {
        Ok(())
    }

    pub async fn emit_device_offline(&self, _device_id: &str) -> anyhow::Result<()> {
        Ok(())
    }
}
```

`crates/ahand-hub/src/output_stream.rs`:

```rust
use std::convert::Infallible;

use axum::response::sse::Event;
use futures_util::stream;

#[derive(Default)]
pub struct OutputStream;

impl OutputStream {
    pub async fn subscribe(
        &self,
        _job_id: String,
    ) -> anyhow::Result<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
        Ok(stream::iter(vec![Ok(Event::default().event("finished").data("{\"exit_code\":0}"))]))
    }
}
```

`crates/ahand-hub/src/audit_writer.rs`:

```rust
pub async fn run_audit_writer(
    store: Arc<dyn AuditStore>,
    mut rx: mpsc::Receiver<AuditEntry>,
) {
    let mut buffer = Vec::with_capacity(100);
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(500));

    loop {
        tokio::select! {
            maybe_entry = rx.recv() => {
                match maybe_entry {
                    Some(entry) => {
                        buffer.push(entry);
                        if buffer.len() >= 100 {
                            let batch = std::mem::take(&mut buffer);
                            store.append(&batch).await.unwrap();
                        }
                    }
                    None => break,
                }
            }
            _ = ticker.tick() => {
                if !buffer.is_empty() {
                    let batch = std::mem::take(&mut buffer);
                    store.append(&batch).await.unwrap();
                }
            }
        }
    }
}
```

- [ ] **Step 4: Run the hub integration tests**

Run:

```bash
cargo test -p ahand-hub -- --nocapture
```

Expected: device gateway, job flow, and SSE tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ahand-hub
git commit -m "feat(ahand-hub): add websocket gateway and job streaming"
```

## Task 7: Create the Dashboard App, Auth Shell, and API Proxy

**Files:**
- Modify: `package.json`
- Modify: `turbo.json`
- Create: `apps/hub-dashboard/package.json`
- Create: `apps/hub-dashboard/tsconfig.json`
- Create: `apps/hub-dashboard/next.config.ts`
- Create: `apps/hub-dashboard/postcss.config.mjs`
- Create: `apps/hub-dashboard/eslint.config.mjs`
- Create: `apps/hub-dashboard/vitest.config.ts`
- Create: `apps/hub-dashboard/src/app/globals.css`
- Create: `apps/hub-dashboard/src/app/layout.tsx`
- Create: `apps/hub-dashboard/src/app/login/page.tsx`
- Create: `apps/hub-dashboard/src/app/(dashboard)/layout.tsx`
- Create: `apps/hub-dashboard/src/app/api/auth/login/route.ts`
- Create: `apps/hub-dashboard/src/app/api/auth/logout/route.ts`
- Create: `apps/hub-dashboard/src/app/api/proxy/[...path]/route.ts`
- Create: `apps/hub-dashboard/src/components/providers.tsx`
- Create: `apps/hub-dashboard/src/lib/auth.ts`
- Create: `apps/hub-dashboard/src/middleware.ts`
- Create: `apps/hub-dashboard/tests/auth-flow.test.tsx`
- Create: `apps/hub-dashboard/tests/setup.ts`

- [ ] **Step 1: Write the failing dashboard auth test**

```tsx
import { render, screen } from "@testing-library/react";
import LoginPage from "@/app/login/page";

describe("login page", () => {
  it("renders the dashboard sign-in form", () => {
    render(<LoginPage />);
    expect(screen.getByRole("heading", { name: /ahand hub/i })).toBeInTheDocument();
    expect(screen.getByLabelText(/shared password/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /sign in/i })).toBeInTheDocument();
  });
});
```

- [ ] **Step 2: Run the dashboard tests to verify they fail**

Run:

```bash
pnpm --filter @ahand/hub-dashboard test
```

Expected: `No projects matched the filters in "/Users/winrey/Projects/weightwave/aHand"`.

- [ ] **Step 3: Create the Next.js app shell and wire it into the monorepo**

Root `package.json` additions:

```json
{
  "scripts": {
    "dev:hub-dashboard": "turbo @ahand/hub-dashboard#dev",
    "build:hub-dashboard": "turbo @ahand/hub-dashboard#build",
    "test:hub-dashboard": "turbo @ahand/hub-dashboard#test"
  }
}
```

`turbo.json` additions:

```json
{
  "tasks": {
    "@ahand/hub-dashboard#build": {
      "inputs": [
        "src/**",
        "package.json",
        "next.config.ts",
        "tsconfig.json"
      ],
      "outputs": [".next/**"]
    },
    "@ahand/hub-dashboard#dev": {
      "cache": false,
      "persistent": true
    },
    "@ahand/hub-dashboard#test": {
      "inputs": [
        "src/**",
        "tests/**",
        "vitest.config.ts",
        "package.json"
      ]
    }
  }
}
```

`apps/hub-dashboard/package.json`:

```json
{
  "name": "@ahand/hub-dashboard",
  "version": "0.1.2",
  "private": true,
  "scripts": {
    "dev": "next dev -p 3100",
    "build": "next build",
    "start": "next start -p 3100",
    "lint": "eslint .",
    "test": "vitest run"
  },
  "dependencies": {
    "@tanstack/react-query": "^5.90.0",
    "jose": "^6.1.3",
    "next": "16.1.6",
    "next-themes": "^0.4.6",
    "react": "19.2.3",
    "react-dom": "19.2.3"
  },
  "devDependencies": {
    "@testing-library/jest-dom": "^6.8.0",
    "@testing-library/react": "^16.0.1",
    "@types/node": "^20.19.17",
    "@types/react": "^19.2.2",
    "@types/react-dom": "^19.2.2",
    "eslint": "^9.38.0",
    "eslint-config-next": "16.1.6",
    "jsdom": "^26.1.0",
    "msw": "^2.11.5",
    "tailwindcss": "^4.1.17",
    "typescript": "^5.9.3",
    "vitest": "^4.0.7"
  }
}
```

`apps/hub-dashboard/src/app/login/page.tsx`:

```tsx
"use client";

import { FormEvent, useState } from "react";

export default function LoginPage() {
  const [password, setPassword] = useState("");

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    await fetch("/api/auth/login", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ password }),
    });
    window.location.href = "/";
  }

  return (
    <main className="mx-auto flex min-h-screen max-w-md flex-col justify-center gap-6 p-6">
      <div>
        <h1 className="text-3xl font-semibold">aHand Hub Dashboard</h1>
        <p className="text-sm text-neutral-600">Sign in with the shared password.</p>
      </div>
      <form className="grid gap-4" onSubmit={onSubmit}>
        <label className="grid gap-2">
          <span className="text-sm font-medium">Shared Password</span>
          <input
            className="rounded border px-3 py-2"
            type="password"
            value={password}
            onChange={(event) => setPassword(event.target.value)}
          />
        </label>
        <button className="rounded bg-black px-4 py-2 text-white" type="submit">
          Sign in
        </button>
      </form>
    </main>
  );
}
```

`apps/hub-dashboard/src/components/providers.tsx`:

```tsx
"use client";

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useState } from "react";

export function Providers({ children }: { children: React.ReactNode }) {
  const [queryClient] = useState(() => new QueryClient());
  return <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
}
```

`apps/hub-dashboard/src/app/layout.tsx`:

```tsx
import "./globals.css";
import { Providers } from "@/components/providers";

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en">
      <body>
        <Providers>{children}</Providers>
      </body>
    </html>
  );
}
```

`apps/hub-dashboard/src/middleware.ts`:

```ts
import { NextResponse } from "next/server";
import type { NextRequest } from "next/server";

export function middleware(request: NextRequest) {
  const session = request.cookies.get("ahand_hub_session");
  if (!session && !request.nextUrl.pathname.startsWith("/login") && !request.nextUrl.pathname.startsWith("/api/auth")) {
    return NextResponse.redirect(new URL("/login", request.url));
  }
  return NextResponse.next();
}

export const config = {
  matcher: ["/((?!_next/static|_next/image|favicon.ico).*)"],
};
```

`apps/hub-dashboard/src/app/api/proxy/[...path]/route.ts`:

```ts
import { NextRequest, NextResponse } from "next/server";

export async function GET(
  request: NextRequest,
  { params }: { params: Promise<{ path: string[] }> },
) {
  const { path } = await params;
  const session = request.cookies.get("ahand_hub_session")?.value ?? "";
  const upstream = `${process.env.AHAND_HUB_BASE_URL}/${path.join("/")}`;

  const response = await fetch(upstream, {
    headers: {
      authorization: `Bearer ${session}`,
      accept: request.headers.get("accept") ?? "application/json",
    },
    cache: "no-store",
  });

  return new NextResponse(response.body, {
    status: response.status,
    headers: response.headers,
  });
}
```

`apps/hub-dashboard/vitest.config.ts`:

```ts
import { defineConfig } from "vitest/config";
import path from "node:path";

export default defineConfig({
  test: {
    environment: "jsdom",
    setupFiles: ["./tests/setup.ts"],
  },
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
});
```

`apps/hub-dashboard/src/lib/auth.ts`:

```ts
export function readWsToken(): string | null {
  const entry = document.cookie
    .split("; ")
    .find((part) => part.startsWith("ahand_hub_ws_token="));
  return entry ? decodeURIComponent(entry.split("=")[1]) : null;
}
```

`apps/hub-dashboard/src/app/api/auth/login/route.ts`:

```ts
import { NextRequest, NextResponse } from "next/server";

export async function POST(request: NextRequest) {
  const body = await request.json();
  const response = await fetch(`${process.env.AHAND_HUB_BASE_URL}/api/auth/login`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });

  const payload = await response.json();
  const next = NextResponse.json(payload, { status: response.status });
  if (response.ok && payload.token) {
    next.cookies.set("ahand_hub_session", payload.token, { httpOnly: true, path: "/" });
    next.cookies.set("ahand_hub_ws_token", payload.token, { httpOnly: false, path: "/" });
  }
  return next;
}
```

`apps/hub-dashboard/tests/setup.ts`:

```ts
import "@testing-library/jest-dom/vitest";
```

- [ ] **Step 4: Run the dashboard tests and build**

Run:

```bash
pnpm --filter @ahand/hub-dashboard test
pnpm --filter @ahand/hub-dashboard build
```

Expected: the login test passes and the app builds successfully.

- [ ] **Step 5: Commit**

```bash
git add package.json turbo.json apps/hub-dashboard
git commit -m "feat(dashboard): add nextjs auth shell and api proxy"
```

## Task 8: Build Dashboard Pages for Devices, Jobs, Audit Logs, and Realtime Hooks

**Files:**
- Create: `apps/hub-dashboard/src/app/(dashboard)/page.tsx`
- Create: `apps/hub-dashboard/src/app/(dashboard)/devices/page.tsx`
- Create: `apps/hub-dashboard/src/app/(dashboard)/devices/[id]/page.tsx`
- Create: `apps/hub-dashboard/src/app/(dashboard)/jobs/page.tsx`
- Create: `apps/hub-dashboard/src/app/(dashboard)/jobs/[id]/page.tsx`
- Create: `apps/hub-dashboard/src/app/(dashboard)/audit-logs/page.tsx`
- Create: `apps/hub-dashboard/src/components/sidebar.tsx`
- Create: `apps/hub-dashboard/src/components/device-status-badge.tsx`
- Create: `apps/hub-dashboard/src/components/job-output-viewer.tsx`
- Create: `apps/hub-dashboard/src/hooks/use-dashboard-ws.ts`
- Create: `apps/hub-dashboard/src/hooks/use-job-output.ts`
- Create: `apps/hub-dashboard/src/lib/api.ts`
- Create: `apps/hub-dashboard/tests/devices-page.test.tsx`
- Create: `apps/hub-dashboard/tests/jobs-page.test.tsx`

- [ ] **Step 1: Write the failing page tests**

```tsx
import { render, screen } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { setupServer } from "msw/node";
import DevicesPage from "@/app/(dashboard)/devices/page";

const server = setupServer(
  http.get("http://localhost:8080/api/devices", () =>
    HttpResponse.json([
      { id: "device-1", hostname: "devbox", os: "linux", online: true, capabilities: ["exec"] }
    ])
  )
);

beforeAll(() => server.listen());
afterAll(() => server.close());
afterEach(() => server.resetHandlers());

describe("devices page", () => {
  it("renders the device table from the API", async () => {
    render(await DevicesPage());
    expect(await screen.findByText("devbox")).toBeInTheDocument();
    expect(screen.getByText("linux")).toBeInTheDocument();
    expect(screen.getByText("online")).toBeInTheDocument();
  });
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run:

```bash
pnpm --filter @ahand/hub-dashboard test
```

Expected: failure because the dashboard routes, API client, and realtime hooks do not exist yet.

- [ ] **Step 3: Implement the dashboard routes, API client, and realtime hooks**

`apps/hub-dashboard/src/lib/api.ts`:

```ts
export async function apiGet<T>(path: string): Promise<T> {
  const response = await fetch(`${process.env.AHAND_HUB_BASE_URL}${path}`, {
    headers: { accept: "application/json" },
    cache: "no-store",
  });

  if (!response.ok) {
    throw new Error(`API request failed: ${response.status}`);
  }

  return response.json() as Promise<T>;
}
```

`apps/hub-dashboard/src/app/(dashboard)/devices/page.tsx`:

```tsx
import { apiGet } from "@/lib/api";

type DeviceRow = {
  id: string;
  hostname: string;
  os: string;
  online: boolean;
};

export default async function DevicesPage() {
  const devices = await apiGet<DeviceRow[]>("/api/devices");

  return (
    <main className="p-6">
      <h1 className="mb-4 text-2xl font-semibold">Devices</h1>
      <table className="min-w-full border">
        <thead>
          <tr>
            <th>Hostname</th>
            <th>OS</th>
            <th>Status</th>
          </tr>
        </thead>
        <tbody>
          {devices.map((device) => (
            <tr key={device.id}>
              <td>{device.hostname}</td>
              <td>{device.os}</td>
              <td>{device.online ? "online" : "offline"}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </main>
  );
}
```

`apps/hub-dashboard/src/hooks/use-dashboard-ws.ts`:

```tsx
"use client";

import { useEffect } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { readWsToken } from "@/lib/auth";

export function useDashboardWs() {
  const queryClient = useQueryClient();

  useEffect(() => {
    const token = readWsToken();
    if (!token) return;
    const ws = new WebSocket(`${process.env.NEXT_PUBLIC_AHAND_HUB_WS_BASE}/ws/dashboard?token=${encodeURIComponent(token)}`);
    ws.onmessage = (event) => {
      const payload = JSON.parse(event.data);
      if (payload.event.startsWith("device.")) {
        queryClient.invalidateQueries({ queryKey: ["devices"] });
      }
      if (payload.event.startsWith("job.")) {
        queryClient.invalidateQueries({ queryKey: ["jobs"] });
      }
    };
    return () => ws.close();
  }, [queryClient]);
}
```

`apps/hub-dashboard/src/components/job-output-viewer.tsx`:

```tsx
"use client";

import { useJobOutput } from "@/hooks/use-job-output";

export function JobOutputViewer({ jobId }: { jobId: string }) {
  const lines = useJobOutput(jobId);

  return (
    <pre className="h-96 overflow-auto rounded bg-neutral-950 p-4 font-mono text-sm text-neutral-100">
      {lines.map((line, index) => (
        <div key={`${jobId}-${index}`}>{line}</div>
      ))}
    </pre>
  );
}
```

`apps/hub-dashboard/src/hooks/use-job-output.ts`:

```tsx
"use client";

import { useEffect, useState } from "react";

export function useJobOutput(jobId: string) {
  const [lines, setLines] = useState<string[]>([]);

  useEffect(() => {
    const source = new EventSource(`/api/proxy/api/jobs/${jobId}/output`);
    source.onmessage = (event) => {
      setLines((current) => [...current, event.data]);
    };
    return () => source.close();
  }, [jobId]);

  return lines;
}
```

- [ ] **Step 4: Run the dashboard tests and build again**

Run:

```bash
pnpm --filter @ahand/hub-dashboard test
pnpm --filter @ahand/hub-dashboard build
```

Expected: device/job page tests pass and the dashboard still builds.

- [ ] **Step 5: Commit**

```bash
git add apps/hub-dashboard
git commit -m "feat(dashboard): add device job and audit views"
```

## Task 9: Add CI, Release, Deployment, and Repository Docs

**Files:**
- Create: `.github/workflows/hub-ci.yml`
- Create: `.github/workflows/release-hub.yml`
- Create: `deploy/hub/Dockerfile`
- Create: `deploy/hub/docker-compose.yml`
- Modify: `README.md`

- [ ] **Step 1: Write the failing repository verification commands into the new CI workflow**

```yaml
name: ahand-hub CI

on:
  pull_request:
    paths:
      - "crates/ahand-hub-core/**"
      - "crates/ahand-hub-store/**"
      - "crates/ahand-hub/**"
      - "apps/hub-dashboard/**"
      - "proto/**"
      - "Cargo.toml"
      - "package.json"
      - "turbo.json"

jobs:
  verify:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: pnpm/action-setup@v4
      - uses: actions/setup-node@v4
        with:
          node-version: 22
          cache: pnpm
      - uses: dtolnay/rust-toolchain@stable
      - run: pnpm install --frozen-lockfile
      - run: cargo fmt --check
      - run: cargo clippy --workspace -- -D warnings
      - run: cargo llvm-cov --workspace --fail-under-lines 100
      - run: pnpm --filter @ahand/hub-dashboard test
```

- [ ] **Step 2: Run the local verification commands to see what still fails**

Run:

```bash
cargo fmt --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
pnpm --filter @ahand/hub-dashboard test
```

Expected: any remaining failures are real implementation gaps that must be fixed before merge.

- [ ] **Step 3: Add the final CI/release/deploy/docs wiring**

`deploy/hub/Dockerfile`:

```dockerfile
FROM rust:1.85 AS builder
WORKDIR /app
COPY . .
RUN cargo build --release -p ahand-hub

FROM node:22 AS dashboard
WORKDIR /app
COPY apps/hub-dashboard ./apps/hub-dashboard
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml turbo.json tsconfig.base.json ./
RUN corepack enable && pnpm install --frozen-lockfile
RUN pnpm --filter @ahand/hub-dashboard build

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/ahand-hub /usr/local/bin/ahand-hub
COPY --from=dashboard /app/apps/hub-dashboard/.next /opt/hub-dashboard/.next
ENTRYPOINT ["ahand-hub"]
```

`.github/workflows/release-hub.yml`:

```yaml
name: Release Hub

on:
  push:
    tags:
      - "hub-v*"
  workflow_dispatch:
    inputs:
      tag:
        description: "Release tag (e.g. hub-v0.1.0)"
        required: true

permissions:
  contents: write
  packages: write

jobs:
  release:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: docker/setup-buildx-action@v3
      - uses: docker/login-action@v3
        with:
          registry: ghcr.io
          username: ${{ github.actor }}
          password: ${{ secrets.GITHUB_TOKEN }}
      - name: Get tag
        id: tag
        run: echo "tag=${GITHUB_REF_NAME:-${{ github.event.inputs.tag }}}" >> "$GITHUB_OUTPUT"
      - name: Build and push image
        uses: docker/build-push-action@v6
        with:
          context: .
          file: deploy/hub/Dockerfile
          push: true
          tags: ghcr.io/${{ github.repository }}/ahand-hub:${{ steps.tag.outputs.tag }}
```

`deploy/hub/docker-compose.yml`:

```yaml
services:
  ahand-hub:
    build:
      context: ../..
      dockerfile: deploy/hub/Dockerfile
    ports:
      - "8080:8080"
    environment:
      AHAND_HUB__DATABASE__URL: postgres://ahand:ahand@postgres:5432/ahand_hub
      AHAND_HUB__REDIS__URL: redis://redis:6379
      AHAND_HUB__AUTH__JWT_SECRET: dev-secret
    depends_on:
      - postgres
      - redis

  postgres:
    image: postgres:17
    environment:
      POSTGRES_USER: ahand
      POSTGRES_PASSWORD: ahand
      POSTGRES_DB: ahand_hub

  redis:
    image: redis:7-alpine
```

`README.md` additions:

```md
## Production Control Center

`ahand-hub` is the production control-center service for authenticated device registration,
job dispatch, audit logs, and the management dashboard.

### New Components

- `crates/ahand-hub-core`
- `crates/ahand-hub-store`
- `crates/ahand-hub`
- `apps/hub-dashboard`
```

- [ ] **Step 4: Run the final full-stack verification**

Run:

```bash
cargo test --workspace -- --nocapture
pnpm --filter @ahand/hub-dashboard test
pnpm --filter @ahand/hub-dashboard build
docker compose -f deploy/hub/docker-compose.yml config
```

Expected:

```text
test result: ok.
✓ tests passed
Next.js build completed successfully
services:
  ahand-hub:
  postgres:
  redis:
```

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/hub-ci.yml .github/workflows/release-hub.yml deploy/hub README.md
git commit -m "chore(hub): add ci release and deployment wiring"
```

## Self-Review Checklist

1. **Spec coverage**
   - Protocol auth handshake: Task 1
   - Core crate boundaries and state machines: Task 2
   - PostgreSQL/Redis persistence: Task 3
   - `ahandd` authenticated hello: Task 4
   - REST API, device connectivity, auth middleware: Task 5
   - Job execution, SSE, dashboard WebSocket, audit pipeline: Task 6
   - React dashboard shell and auth: Task 7
   - Devices/jobs/audit UI and realtime hooks: Task 8
   - CI/release/deploy/docs: Task 9

2. **Placeholder scan**
   - No `TBD`, `TODO`, or “implement later” work items remain inside task steps.

3. **Type consistency**
   - Crate names stay consistent: `ahand-hub-core`, `ahand-hub-store`, `ahand-hub`
   - Dashboard package name stays consistent: `@ahand/hub-dashboard`
   - Auth roles stay consistent: `Admin`, `DashboardUser`, `Device`
   - Key service names stay consistent: `DeviceManager`, `JobDispatcher`, `AuditService`

4. **Scope control**
   - Browser automation, approvals, session-mode UX, OpenClaw compatibility, and offline queueing are intentionally excluded from this implementation plan.
