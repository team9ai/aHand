# Daemon Auto-Update via Hub

Hub-initiated automatic update mechanism for ahandd. The hub can instruct daemons to update during registration (HelloAccepted) or at runtime (UpdateCommand), with the daemon downloading, verifying signatures, and installing new binaries autonomously.

## Context

Currently ahandd upgrades are fully manual (`ahandctl upgrade`). The hub already receives the daemon's version in the Hello handshake and stores it in the device record, but has no way to act on outdated versions. This design adds hub-driven update orchestration.

## Requirements

- **Trigger**: Hello handshake (automatic, based on global minimum version) + runtime push (admin clicks "update" in dashboard)
- **Behavior**: Silent auto-update, no local user confirmation required
- **Update source**: Hub provides full download URL + checksum; daemon downloads from that URL
- **Version policy**: Global minimum version; dashboard can also manually push a specific version to any device
- **Signing**: Independent Ed25519 code signing; daemon has built-in public key
- **Failure handling**: Exponential backoff retry (3 attempts); report failure to hub after exhaustion

## Protocol Changes

### Extended HelloAccepted

```protobuf
message HelloAccepted {
  string auth_method = 1;
  UpdateSuggestion update_suggestion = 2;  // optional: present when daemon version < min_version
}

message UpdateSuggestion {
  string update_id = 1;             // UUID, for status tracking (same as UpdateCommand)
  string target_version = 2;
  string download_url = 3;
  string checksum_sha256 = 4;
  bytes  signature = 5;             // Ed25519 signature over binary content
  string release_notes = 6;
}
```

### New Messages

```protobuf
// Hub -> Daemon: push update instruction (runtime)
message UpdateCommand {
  string update_id = 1;             // UUID, for status tracking
  string target_version = 2;
  string download_url = 3;
  string checksum_sha256 = 4;
  bytes  signature = 5;
  uint32 max_retries = 6;           // hub-suggested max retries (default 3)
}

// Daemon -> Hub: update progress report
message UpdateStatus {
  string update_id = 1;
  UpdateState state = 2;
  string current_version = 3;
  string target_version = 4;
  uint32 progress = 5;              // 0-100
  string error = 6;
}

enum UpdateState {
  UPDATE_STATE_PENDING = 0;
  UPDATE_STATE_DOWNLOADING = 1;
  UPDATE_STATE_VERIFYING = 2;
  UPDATE_STATE_INSTALLING = 3;
  UPDATE_STATE_RESTARTING = 4;
  UPDATE_STATE_COMPLETED = 5;
  UPDATE_STATE_FAILED = 6;
}
```

### Envelope Additions

```protobuf
// Add to Envelope oneof payload:
UpdateCommand  update_command  = 27;
UpdateStatus   update_status   = 28;
```

## Signing Mechanism

- A dedicated **release signing keypair** (Ed25519) is maintained by the project.
- **Private key**: used only in CI to sign release binaries. Never committed to the repository.
- **Public key**: compiled into the daemon binary at build time via `include_bytes!("../keys/release.pub")`, stored in the repo at `keys/release.pub`.
- **Signature target**: the raw binary file content (not URLs or metadata).
- Daemon verifies: SHA256 checksum match first, then Ed25519 signature against built-in public key. Both must pass before installation proceeds.

## Hub-Side Logic

### Configuration

New config fields:

- `min_device_version: Option<String>` — global minimum version (env: `AHAND_HUB_MIN_DEVICE_VERSION`)
- `update_download_url_template: String` — e.g. `https://github.com/team9ai/aHand/releases/download/rust-v{version}/ahandd-{os}-{arch}`
- `update_signature_url_template: String` — signature file URL template

These can be modified at runtime via API without restarting the hub.

### Registration-Time Check

In `device_gateway.rs::run_device_socket()`, before sending `HelloAccepted`:

1. Extract `hello.version` and compare with `min_device_version` using semver.
2. If daemon version < minimum: construct `UpdateSuggestion` using `hello.os` + platform mapping to generate the correct `download_url`, `checksum_sha256`, and `signature`.
3. Attach to `HelloAccepted.update_suggestion`.
4. Record in `device_updates` table with `initiated_by = 'system'`.

### Dashboard Manual Push

New HTTP endpoint:

```
POST /api/devices/{device_id}/update
Body: { "target_version": "0.3.0" }
```

Processing:
1. Verify admin permission (`require_admin()`).
2. Verify device is online (check `ConnectionRegistry`).
3. Construct `download_url`, fetch `checksum_sha256` and `signature` based on `target_version` + device OS.
4. Generate `update_id` (UUID).
5. Send `UpdateCommand` via `connections.send()`.
6. Record in `device_updates` table with `initiated_by = <admin_user_id>`.
7. Return `update_id` for status polling.

### UpdateStatus Reception

In device gateway message loop, handle `UpdateStatus`:

1. Update `device_updates` record in database (status, progress, error_message, updated_at).
2. Broadcast via EventBus to dashboard WebSocket for real-time progress.
3. Audit log: `device.update.status_changed`.

### Database Schema

```sql
CREATE TABLE device_updates (
    id TEXT PRIMARY KEY,
    device_id TEXT NOT NULL,
    from_version TEXT,
    target_version TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    progress INTEGER DEFAULT 0,
    error_message TEXT,
    initiated_by TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

### New API Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/devices/{id}/update` | POST | Push update to a specific device |
| `/api/devices/{id}/updates` | GET | Query update history for a device |
| `/api/settings/min-version` | GET/PUT | View/modify global minimum version |

## Daemon-Side Logic

### Handshake Phase

In `ahand_client.rs::connect_with_auth()`, after receiving `HelloAccepted`:

```rust
let accepted = recv_hello_accepted(&mut stream).await?;
if let Some(suggestion) = accepted.update_suggestion {
    spawn_update_task(suggestion, ...);
}
```

The update task runs in the background; it does not block the message loop.

### Runtime Phase

In the message loop `match envelope.payload`, add:

```rust
Some(envelope::Payload::UpdateCommand(cmd)) => {
    spawn_update_task(cmd, ...);
}
```

### Shared Update Executor

Both `UpdateSuggestion` and `UpdateCommand` feed into the same executor:

```
execute_update(params):
  1. Report UPDATE_STATE_DOWNLOADING (progress 0)
  2. Download binary to temp directory
  3. Report UPDATE_STATE_VERIFYING (progress 50)
  4. Verify SHA256 checksum
  5. Verify Ed25519 signature using built-in RELEASE_PUBLIC_KEY
  6. Report UPDATE_STATE_INSTALLING (progress 80)
  7. Replace binary at ~/.ahand/bin/ahandd, chmod +x
  8. Write version marker to ~/.ahand/version
  9. Report UPDATE_STATE_RESTARTING (progress 100)
  10. exec() new binary with same args (self-replacement restart)
```

### Restart Strategy

Use Unix `exec` syscall to replace the current process with the new binary. The daemon saves `std::env::args()` and `std::env::current_exe()` at startup. After installing the new binary, it calls `exec` with the saved arguments. PID remains the same; no external process manager dependency.

### Retry Strategy

- Default max retries: 3 (overridable by `UpdateCommand.max_retries`)
- Backoff intervals: 5s, 15s, 45s (exponential)
- Only download + checksum verification are retried
- **Signature verification failure is immediate abort** (retrying won't help)
- After all retries exhausted: report `UPDATE_STATE_FAILED` with error message to hub

### Built-in Release Public Key

```rust
const RELEASE_PUBLIC_KEY: &[u8; 32] = include_bytes!("../keys/release.pub");
```

## Dashboard UI

### Device Detail Page

- **Version info section**: current version, global min version, whether update is needed
- **"Push Update" button**: enabled when device is online; opens version input (defaults to latest); calls `POST /api/devices/{id}/update`
- **Update history list**: pulled from `device_updates` table; shows update_id, target version, status, timestamps
- **Real-time progress**: dashboard WebSocket receives `UpdateStatus` events; shows progress bar and status text on active updates

### Global Settings Page

- View/modify global `min_device_version`
- Version distribution overview (e.g. "3 devices on 0.1.2, 2 on 0.2.0")
- "Batch update all outdated devices" button

## Edge Cases

- **Disconnect during update**: On reconnect, hub checks `device_updates` table for pending updates. Does not re-send the same `update_id`. May issue a new update if the device version is still below minimum.
- **Downgrade protection**: Daemon rejects `target_version` < current version. Hub can add an `allow_downgrade` flag in UpdateCommand for explicit downgrades (future extension, not in initial implementation).
- **Concurrent updates**: Daemon executes at most one update at a time. Additional commands received during an active update are rejected with an error status.
- **Signature key rotation**: When the release public key changes, a transitional build must include both old and new public keys. The daemon tries verification with each key until one succeeds.

## Testing Strategy

### Protocol Tests
- Protobuf roundtrip: `UpdateSuggestion`, `UpdateCommand`, `UpdateStatus` encode/decode
- `HelloAccepted` backward compatibility: with and without `update_suggestion`

### Hub Tests
- Version comparison: daemon < min_version attaches suggestion; >= does not
- `POST /api/devices/{id}/update`: admin auth, offline device rejection, correct message construction
- `UpdateStatus` handler: database update correctness, EventBus broadcast

### Daemon Tests
- Update executor: checksum pass/fail, signature pass/fail
- Retry logic: download failure triggers backoff retry; signature failure aborts immediately
- Handshake suggestion triggers update task
- Runtime UpdateCommand triggers update task
- Concurrent update rejection

### Integration Tests
- Full flow: hub sets min_version -> daemon connects -> receives suggestion -> downloads (mock HTTP) -> verifies -> installs -> reports status
- Dashboard push -> daemon receives -> executes -> real-time status updates via WebSocket
