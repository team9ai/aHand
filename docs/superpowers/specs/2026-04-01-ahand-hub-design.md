# ahand-hub Design Spec

Status: approved design, pending implementation plan  
Date: 2026-04-01

## 1. Background

`aHand` already has a local daemon, protobuf protocol definitions, a development cloud service, and temporary admin tooling. What it does not have yet is a production-grade control center service. The goal of this project is to add a Rust control center inside the existing monorepo so agents can remotely use registered computers to execute commands, create workspaces, inspect state, and provide a foundation for later human observation/takeover, approvals, auditing, and multi-protocol support.

The V1 service name is `ahand-hub`.

## 2. V1 Goals and Non-Goals

### 2.1 V1 Goals

V1 includes only these five capability groups:

1. Device registration and connection management
2. Command execution with streamed output
3. REST management API
4. Audit logging
5. React dashboard

### 2.2 V1 Non-Goals

The following items are explicitly out of scope for V1. Implementation should leave extension points, but they are not part of V1 acceptance:

1. Session mode management (`Inactive`, `Strict`, `Trust`, `AutoAccept`)
2. Approval request/response flows
3. Browser automation proxying
4. Human observation or takeover of a device
5. OpenClaw gateway node protocol compatibility
6. Policy configuration (Mode 5)
7. Organization/user multi-tenancy
8. Offline job queueing

## 3. Constraints and Confirmed Decisions

This spec is based on the following confirmed decisions:

1. `ahand-hub` lives inside the `aHand` monorepo, not in a separate repository.
2. The architecture is "modular monolith first, split later".
3. PostgreSQL is used for persistence; Redis is used for hot state and real-time data.
4. Device authentication is Ed25519-first, with token-assisted bootstrap into the key-based model.
5. External interfaces are REST plus WebSocket.
6. Early isolation is device-centric rather than full tenant-aware identity modeling.
7. Internal services use preconfigured service tokens, the dashboard uses a shared password that exchanges for JWTs, and external access can use device-scoped JWTs issued by a business backend.
8. Rust crates must be split sensibly; a single giant crate is not acceptable.
9. Test quality is a hard requirement. Rust and frontend coverage should aim as close to 100% as practical.

## 4. Overall Architecture

V1 is deployed as a single service process, but the codebase is explicitly split into three new Rust crates and one frontend app:

```text
crates/
├── ahand-protocol/        # existing crate, remains the protocol type source
├── ahand-hub-core/        # domain models, state machines, traits, auth, business rules
├── ahand-hub-store/       # PostgreSQL + Redis adapters
└── ahand-hub/             # binary entrypoint, REST, WebSocket, orchestration, background tasks

apps/
└── hub-dashboard/         # new React/Next.js dashboard
```

High-level structure:

```text
Devices (ahandd)
    |
    | WebSocket + protobuf envelope
    v
+-------------------------------+
|         ahand-hub             |
|                               |
|  +-------------------------+  |
|  | REST API / Dashboard WS |  |
|  +-------------------------+  |
|  | Device WS Gateway       |  |
|  +-------------------------+  |
|  | Runtime / Background    |  |
|  +------------+------------+  |
|               |               |
+---------------|---------------+
                |
                v
       +------------------+
       | ahand-hub-core   |
       +------------------+
                |
                v
       +------------------+
       | ahand-hub-store  |
       +------------------+
          |           |
          v           v
         PG         Redis
```

Core principles:

1. `ahand-hub` the binary should stay thin. Business logic belongs in `ahand-hub-core`.
2. Persistence and cache behavior should live in `ahand-hub-store`, not in network handlers.
3. Every state transition must have a clear state machine and direct test coverage.

## 5. Crate Boundaries and Responsibilities

### 5.1 `crates/ahand-hub-core`

Responsibilities:

1. Define domain models such as `Device`, `DeviceRegistration`, `DevicePresence`, `Job`, `JobStatus`, `AuditEntry`, and `AuthContext`
2. Define storage abstraction traits such as `DeviceStore`, `JobStore`, `AuditStore`, `PresenceStore`, and `TokenStore`
3. Implement business services such as `DeviceManager`, `JobDispatcher`, `AuthService`, and `AuditService`
4. Implement the `Outbox` and replay rules for reliable message delivery
5. Define shared error types and legal state transitions

Constraints:

1. No direct dependency on the database or web framework
2. No IO-heavy dependencies beyond time, serialization, and crypto support
3. Every state-machine transition requires direct unit coverage

### 5.2 `crates/ahand-hub-store`

Responsibilities:

1. Implement the storage traits defined in `ahand-hub-core`
2. Provide PostgreSQL access and migrations
3. Provide Redis-backed presence, output streaming, and event fan-out
4. Provide test helpers suitable for containerized integration tests

Constraints:

1. This crate stores and retrieves data; it does not own business decisions
2. It exposes domain-oriented interfaces upward, not raw SQL or raw Redis semantics
3. Migrations are managed through `sqlx::migrate!` and versioned with the crate

### 5.3 `crates/ahand-hub`

Responsibilities:

1. Build configuration, logging, storage, and core services
2. Expose the axum REST API
3. Maintain the device WebSocket gateway
4. Expose the dashboard event WebSocket
5. Run background tasks such as heartbeat cleanup, audit flushing, and retention cleanup

Constraints:

1. Handlers should only do parsing, auth, and service invocation
2. Complex business flow should not be assembled inside handlers
3. `main.rs` should stay focused on startup wiring

### 5.4 `apps/hub-dashboard`

Responsibilities:

1. Provide dashboard login, device list, job list, job detail, and audit log views
2. Consume the `ahand-hub` REST API, SSE output stream, and dashboard WebSocket
3. Integrate with the existing monorepo build flow

## 6. Authentication and Identity Model

### 6.1 Three Caller Classes

#### Service Token

Used by internal services. This is the administrator-level identity class. Typical callers are Team9 internal services and internal automation.

#### Dashboard JWT

Issued by `ahand-hub` after the operator logs in with the shared dashboard password. V1 does not model full user accounts; this JWT is simply a trusted management session.

#### Device JWT

Issued by a business backend for external integration scenarios. It is limited to the device's own scope.

### 6.2 Device Authentication

Device authentication is Ed25519-first. Tokens are used only for bootstrap or assisted registration, not as the long-term device identity.

Suggested handshake:

1. Device connects to `/ws`
2. Device sends `Hello`
3. `Hello` carries authentication:
   - `ed25519`: public key, signature, timestamp
   - `bearer_token`: bootstrap token or external device JWT
4. `ahand-hub` verifies the signature or token
5. `ahand-hub` finds or creates the device record
6. `ahand-hub` marks the device online and restores any unacked outbound messages

Bootstrap binding rules:

1. `POST /api/devices` pre-registers a device and returns a one-time bootstrap token
2. The device may use that bootstrap token on its first connection
3. On the first successful connection, the device must present its Ed25519 public key
4. `ahand-hub` binds that key to the device record
5. All later connections should use Ed25519 signatures by default
6. The bootstrap token is invalidated immediately after successful first use

The signed payload is fixed:

```text
ahand-hub|{device_id}|{signed_at_ms}
```

Restrictions:

1. Signature validity window defaults to 5 minutes
2. Device `id` is derived from the public key or a fixed pre-registration identity and remains stable
3. Anonymous `Hello` must not create an active business connection

## 7. Device Connection and WebSocket Design

### 7.1 Device Connection Lifecycle

```text
device -> connect /ws
device -> Hello(version, host, os, capabilities, last_ack, auth)
hub    -> verify auth
hub    -> upsert device record
hub    -> restore outbox from last_ack
hub    -> mark online in Redis
hub <-> exchange envelopes
hub <-> ping/pong heartbeat
disconnect -> keep replay buffer for retain window
timeout -> clean in-memory session and mark offline
```

### 7.2 Presence Model

Online state has two layers:

1. In-memory connection pool for the actual live WebSocket sessions in the current process
2. Redis presence with TTL for dashboard queries and future multi-instance compatibility

Default timings:

1. Heartbeat interval: 30 seconds
2. Heartbeat timeout: 90 seconds
3. Outbox retention after disconnect: 10 minutes

### 7.3 Outbox and Reliable Delivery

V1 keeps the existing `seq/ack` model. Each device session maintains a bounded `Outbox`:

1. Every outbound message gets a monotonically increasing `seq`
2. The peer returns `ack`
3. The service drops all buffered messages with `seq <= ack`
4. On reconnect, the device uses `last_ack` so the service can replay the remainder

The `Outbox` only stores service-to-device messages. Device-to-service output and status events are considered accepted once persisted to Redis/PostgreSQL and are not replayed by the service.

### 7.4 Dashboard Realtime Stream

The dashboard does not consume protobuf directly. It connects to `/ws/dashboard` and receives JSON events. V1 must support at least:

1. `device.online`
2. `device.offline`
3. `job.created`
4. `job.running`
5. `job.finished`
6. `job.failed`
7. `job.cancelled`

## 8. Job Model and Execution Flow

### 8.1 Job State Machine

```text
Pending -> Sent -> Running -> Finished
                        \-> Failed
                        \-> Cancelled

Pending -> Cancelled
Sent    -> Cancelled
Sent    -> Failed
Running -> Failed
```

Illegal transitions must return explicit errors. Silent overwrite is not allowed.

### 8.2 Job Submission Flow

`POST /api/jobs` should execute the following flow:

1. Authenticate and parse the request
2. Confirm the device exists and is online
3. Insert a `jobs` row in PostgreSQL with `pending`
4. Write audit event `job.created`
5. Send `JobRequest` over the device WebSocket
6. Move the job to `sent`
7. Return `202 Accepted`

V1 does not support offline queueing. If the device is offline, the API returns an error immediately.

### 8.3 Output Streaming

Realtime output is stored in Redis Streams:

```text
key: ahand:job:{job_id}:output
entry fields:
  type = stdout | stderr | finished | failed | cancelled
  data = chunk or json payload
```

Requirements:

1. The dashboard reads historical output first, then subscribes to live updates
2. REST exposes job output through SSE
3. The stream gets a TTL after job completion, default 1 hour
4. PostgreSQL stores only a summary, not the full output log

### 8.4 Cancellation and Timeout

Cancellation flow:

1. Only `pending`, `sent`, and `running` are cancellable
2. `ahand-hub` sends `CancelJob`
3. The job transitions to `cancelled`
4. An audit event is written

Timeout flow:

1. Creating the job starts a local timeout timer
2. On timeout, `ahand-hub` sends `CancelJob`
3. If the job still does not end cleanly, it becomes `failed` with error `timeout`

### 8.5 Device Disconnects

V1 uses a conservative failure policy:

1. If the job has not started running, a disconnect fails the job
2. If the job is already `running`, reconnect is allowed during the outbox retention window
3. If reconnect does not happen in time, the job fails with `device disconnected`

## 9. REST API Design

### 9.0 Role Matrix

| Role | Typical source | Boundary |
|------|----------------|----------|
| `Admin` | service token | full device management, job submission, job cancellation, audit access |
| `DashboardUser` | dashboard JWT | read-only device and job access, audit access, stats access |
| `Device` | business backend device JWT | access only to its own device-scoped resources |

### 9.1 Device APIs

```text
POST   /api/devices
GET    /api/devices
GET    /api/devices/:id
DELETE /api/devices/:id
GET    /api/devices/:id/capabilities
```

Semantics:

1. `POST /api/devices` pre-registers a device and returns identity plus connection bootstrap data
2. `GET /api/devices` returns PostgreSQL metadata merged with Redis presence
3. `DELETE /api/devices/:id` is an admin-only destructive action

Permissions:

1. `POST /api/devices` is `Admin` only
2. `GET /api/devices` is allowed for `Admin` and `DashboardUser`
3. `GET /api/devices/:id` is allowed for `Admin`, `DashboardUser`, and the matching `Device`
4. `DELETE /api/devices/:id` is `Admin` only

### 9.2 Job APIs

```text
POST /api/jobs
GET  /api/jobs
GET  /api/jobs/:id
GET  /api/jobs/:id/output
POST /api/jobs/:id/cancel
```

Rules:

1. `POST /api/jobs` is `Admin` only
2. `GET /api/jobs` and `GET /api/jobs/:id` are allowed for `Admin` and `DashboardUser`
3. `GET /api/jobs/:id/output` uses `text/event-stream`
4. `POST /api/jobs/:id/cancel` is `Admin` only
5. List APIs must support filtering by `device_id`, `status`, and pagination

### 9.3 Audit and System APIs

```text
GET  /api/audit-logs
POST /api/auth/login
GET  /api/auth/verify
GET  /api/health
GET  /api/stats
```

Additional rules:

1. `POST /api/auth/login` validates the shared dashboard password and returns a dashboard JWT
2. The dashboard frontend should normally use that JWT via same-origin cookie
3. `/ws/dashboard` requires `DashboardUser` or `Admin`; anonymous access is not allowed
4. `GET /api/health` may be anonymous; all other endpoints require authentication

### 9.4 Error Model

Errors should use a single response shape:

```json
{
  "error": {
    "code": "DEVICE_OFFLINE",
    "message": "Device abc123 is not currently connected"
  }
}
```

V1 must at least define these error codes:

1. `UNAUTHORIZED`
2. `FORBIDDEN`
3. `VALIDATION_ERROR`
4. `DEVICE_NOT_FOUND`
5. `DEVICE_OFFLINE`
6. `JOB_NOT_FOUND`
7. `JOB_NOT_CANCELLABLE`
8. `INTERNAL_ERROR`

## 10. Data Model

### 10.1 PostgreSQL

#### `devices`

Fields:

1. `id`: primary key, device identity
2. `public_key`: Ed25519 public key, nullable
3. `hostname`
4. `os`
5. `capabilities`: `TEXT[]`
6. `version`
7. `auth_method`: `ed25519` or `token`
8. `registered_at`
9. `last_seen_at`
10. `metadata`: extensible JSON

#### `jobs`

Fields:

1. `id`: UUID
2. `device_id`
3. `tool`
4. `args`
5. `cwd`
6. `env`
7. `timeout_ms`
8. `status`
9. `exit_code`
10. `error`
11. `output_summary`
12. `requested_by`
13. `created_at`
14. `started_at`
15. `finished_at`

#### `audit_logs`

Fields:

1. `id`
2. `timestamp`
3. `action`
4. `resource_type`
5. `resource_id`
6. `actor`
7. `detail`
8. `source_ip`

#### `auth_tokens`

Fields:

1. `id`: token hash
2. `name`
3. `role`
4. `created_at`
5. `expires_at`
6. `last_used_at`

### 10.2 Redis

```text
ahand:device:{device_id}:online
ahand:device:{device_id}:meta
ahand:devices:online
ahand:job:{job_id}:output
ahand:job:{job_id}:status
ahand:events
ahand:auth:jwt:{token_hash}
```

Purpose:

1. `device:*` maintains presence and dashboard-facing metadata
2. `job:*` maintains live output and short-lived status cache
3. `events` fans out dashboard event notifications
4. `auth:*` caches JWT verification outcomes

## 11. Audit Logging

### 11.1 Event Coverage

V1 must record at least:

1. `device.registered`
2. `device.connected`
3. `device.disconnected`
4. `device.deleted`
5. `job.created`
6. `job.sent`
7. `job.running`
8. `job.finished`
9. `job.failed`
10. `job.cancelled`
11. `auth.login_success`
12. `auth.login_failed`

### 11.2 Write Strategy

Audit logging must not block the main request path. Use async batching:

1. Business flow writes audit entries into a bounded channel
2. A background writer flushes to PostgreSQL in batches of either 100 records or 500ms
3. Flush failures retry automatically
4. Persistent failure writes to a local fallback file and emits an alert log

### 11.3 Retention

Default retention is 90 days. A scheduled cleanup removes expired audit records daily.

## 12. Dashboard Design

The dashboard is a new React app at `apps/hub-dashboard`. It should follow the interaction organization of the `openclaw-hive` dashboard where that helps, but it should not inherit that product's business model.

### 12.1 Tech Stack

1. Next.js
2. React
3. TypeScript
4. Tailwind CSS
5. shadcn/ui
6. TanStack Query

### 12.2 Routes

```text
/login
/
/devices
/devices/[id]
/jobs
/jobs/[id]
/audit-logs
```

### 12.3 Page Responsibilities

#### Overview

Shows online devices, offline devices, running jobs, and recent activity.

#### Devices

Shows the device table with filtering by status and search by `hostname` or `device_id`.

#### Device Detail

Shows basic metadata, capabilities, public key fingerprint, recent jobs, and live presence.

#### Jobs

Shows the job list with filtering by status and device.

#### Job Detail

Shows job metadata, state timeline, and a terminal-style realtime output panel.

#### Audit Logs

Shows audit entries with filters by action, resource, and time range, plus expandable structured `detail`.

### 12.4 Realtime Behavior

1. Overview, device presence, and job status updates are driven by `/ws/dashboard`
2. Job output is driven by SSE
3. React Query polling acts as a fallback path

## 13. Configuration and Runtime

Configuration sources override in this order:

1. CLI arguments
2. Environment variables
3. `config.toml`

Environment variable prefix:

```text
AHAND_HUB__
```

Key configuration areas:

1. HTTP and WebSocket listen address and port
2. PostgreSQL URL and pool sizing
3. Redis URL and pool sizing
4. Dashboard password hash
5. JWT secret, TTL, and external JWT verification settings
6. Service token definitions
7. Heartbeat, outbox retention, and output stream TTL
8. Audit batching and retention days

Startup flow:

1. Load configuration
2. Initialize tracing
3. Connect to PostgreSQL and run migrations
4. Connect to Redis
5. Build store and core services
6. Start background tasks
7. Start the axum server
8. Handle graceful shutdown

## 14. CI/CD and Build

V1 requires new CI coverage for `ahand-hub`:

1. `cargo fmt --check`
2. `cargo clippy --workspace -- -D warnings`
3. `cargo llvm-cov --workspace --fail-under-lines 100`
4. Dashboard `pnpm test --coverage`

Deployment shape:

1. `ahand-hub` runs as a single service/container
2. PostgreSQL and Redis are external dependencies
3. The dashboard may be deployed separately first, and can later be embedded as static assets if needed

V1 does not require service splitting yet, but the crate boundaries must preserve a clean path to future extraction.

## 15. Test Strategy

### 15.1 `ahand-hub-core`

Target near-100% line coverage. All state machines, auth logic, outbox behavior, and audit event generation must be directly unit tested.

Priority coverage:

1. Device registration, duplicate registration, invalid auth
2. Device connect/disconnect lifecycle
3. Job creation, cancellation, timeout, illegal transitions
4. JWT, service token, and Ed25519 validation
5. Outbox buffering, ack cleanup, reconnect replay, and overflow handling

### 15.2 `ahand-hub-store`

Use real PostgreSQL and Redis in containerized integration tests.

Priority coverage:

1. Migrations run from an empty database
2. CRUD and filtering for `devices`, `jobs`, and `audit_logs`
3. Presence TTL behavior
4. Redis Streams write/read/expiry behavior
5. Audit batch insertion and retention cleanup

### 15.3 `ahand-hub`

Focus on API and WebSocket integration coverage.

Priority coverage:

1. Auth for each token class
2. Device API permissions and responses
3. Submitting a job to a live device and receiving streamed output
4. Job cancellation
5. Disconnect/reconnect with replay
6. Dashboard WebSocket event delivery

### 15.4 Dashboard

1. Components and hooks use Vitest plus Testing Library
2. API behavior uses MSW
3. Critical page flows include login, device listing, and job output display

## 16. Impact on Existing Protocol and Code

V1 requires these structural changes in the current repository:

1. Update the workspace `Cargo.toml` to include the new crates
2. Extend the existing protobuf `Hello` message area to carry auth information
3. Align `ahandd` with the new handshake and reconnect fields required by `ahand-hub`
4. Add `apps/hub-dashboard`
5. Add `ahand-hub` CI, Docker build support, and database migrations

Existing `ahandctl`, `apps/dev-cloud`, and `packages/sdk` remain useful for development and compatibility work, but they are not the production control-center foundation.

## 17. Future Work

These items are intentionally postponed beyond V1:

1. Approval flows and session modes
2. Browser automation proxying
3. Human observation and device takeover
4. OpenClaw compatibility layer
5. Finer-grained authorization
6. Full organization/user tenancy
7. Offline queueing and scheduling
8. Multi-instance connection routing and leader election

## 18. Acceptance Criteria

V1 is complete only if all of the following are true:

1. Devices can register and maintain authenticated WebSocket connections to `ahand-hub`
2. Internal services can submit commands to online devices through the REST API
3. Command output can be viewed in realtime
4. All critical operations produce audit records
5. The dashboard can inspect devices, jobs, and audit data
6. CI runs the test suite by default and enforces strict coverage gates on core logic

This document defines scope and boundaries. It is not the implementation task breakdown. The next step, once this English version is approved, is to write the formal implementation plan from this spec.
