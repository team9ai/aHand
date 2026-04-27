# Hub Outbox Persistence Design

Date: 2026-04-27

## Background

The hub's per-device outbox (sequence counter + unacked message buffer) currently lives entirely in process memory in [`ConnectionRegistry`](../../../crates/ahand-hub/src/ws/device_gateway.rs). On any hub process restart — `ecs deploy`, OOM, panic, ECS task health-check rotation — all per-device state is wiped. When a previously-connected device reconnects after such a restart, the device's `Hello.last_ack` (highest seq it received last session) is greater than the server's freshly-zeroed `last_issued_seq`, and the server rejects the connection with `HubError::InvalidPeerAck { ack: N, max: 0 }`.

The daemon then sees its WS being closed mid-write and surfaces `IO error: Broken pipe (os error 32)` to the desktop UI. The daemon retries, hits the same rejection, retries again; the device is wedged in an infinite reject loop until the *device's* process is restarted (which clears its in-memory `local_ack`).

Today's incident on dev: the `ahand-hub-dev` task was redeployed at 2026-04-27 01:27, and `/ecs/ahand-hub` CloudWatch logs show the same `device socket ended with error error=invalid peer ack 9, max issued seq is 0` line firing every few seconds, accelerating to multiple times per second once the device's reconnect backoff collapsed.

This is structural — production has the same bug, just hasn't deployed recently.

## Goals

- Hub→device messages are durable across hub restarts (D2: write-through to Redis before WS send).
- Existing wedged devices unblock automatically on the first reconnect after this fix ships.
- Multi-replica hub is supported (R3: Redis-based device-level lock with fencing).
- No new infrastructure dependencies — Redis is already a first-class store (`presence_store`, `bootstrap_store`, `job_output_store`).
- No protocol/contract changes — devices keep speaking the same `Hello` / `Envelope` schema.

## Non-Goals

- **D3 cross-AZ strong consistency.** Single ElastiCache Redis with multi-AZ failover is sufficient.
- **Device-side persistence of `local_ack`.** The daemon's outbox stays in-memory; if the daemon process restarts, it sends `last_ack=0` and the server replays everything in the stream — at-least-once semantics are preserved by re-delivery, not by client persistence.
- **Bulk / batched send across devices.** Per-device send path is the unit of work.
- **Removing the in-memory `Outbox` type.** Kept for tests and the device-side daemon, where in-memory is correct.
- **Outbox-level retry on Redis transient failures.** Hub fails closed (rejects) and lets the daemon's reconnect logic handle it.

## High-Level Architecture

```
┌──────────────────────────────────────────────────────────────────────┐
│  Hub replica (any of N)                                              │
│                                                                      │
│  ┌─────────────────────┐    ┌────────────────────────┐               │
│  │  device_gateway::   │───▶│  OutboxStore trait     │               │
│  │  ConnectionRegistry │    │  (in ahand-hub-core)   │               │
│  └──────────┬──────────┘    └────────────┬───────────┘               │
│             │                            │                           │
│             │ owns per-conn:             │                           │
│             │  - session_id (uuid)       │ impl                      │
│             │  - mpsc to WS IO task      │ ▼                         │
│             │  - Pub/Sub subscriber      │ RedisOutboxStore          │
│             │    on kick:{device_id}     │ (in ahand-hub-store)      │
│             │  - lease renewer task      │                           │
│             │                            │                           │
└─────────────┼────────────────────────────┼───────────────────────────┘
              │                            │
              │ WS                         │ Redis (Lua + Pub/Sub)
              ▼                            ▼
        ┌───────────┐              ┌──────────────────┐
        │  Device   │              │ ElastiCache      │
        │  (ahandd) │              │  outbox:{id}     │ Stream
        └───────────┘              │  seq:{id}        │ String
                                   │  lock:device:{id}│ String + EX
                                   │  kick:{id}       │ Pub/Sub
                                   └──────────────────┘
```

The in-memory `ConnectionEntry` retains only:
- `session_id` (uuid for fencing)
- `mpsc::Sender<OutboundFrame>` to the per-connection WS IO task
- `watch::Sender<bool>` for graceful close

It no longer holds the outbox state. All outbox reads/writes go through `OutboxStore`.

## Module Boundaries

```
ahand-hub-core
  ├─ traits.rs
  │    + pub trait OutboxStore { ... 10 methods ... }
  └─ outbox.rs
       (existing in-memory Outbox kept; used by daemon and tests)

ahand-hub-store
  ├─ outbox_store.rs       (new — RedisOutboxStore)
  └─ lua.rs                (new — embedded Lua script source + cached SHA1)

ahand-hub
  ├─ ws/device_gateway.rs  (refactored — DI OutboxStore, fencing, lease renewer, kick subscriber)
  └─ state.rs              (wires RedisOutboxStore into AppState)
```

`OutboxStore` is the single seam. Tests use a fake (in-memory) impl; production uses `RedisOutboxStore`.

## OutboxStore Trait

```rust
#[async_trait]
pub trait OutboxStore: Send + Sync {
    /// Atomically: SET lock:device:{id} {session_id} NX EX 30. Returns true on success.
    async fn try_acquire_lock(&self, device_id: &str, session_id: &str) -> Result<bool>;

    /// Best-effort kick: PUBLISH kick:{device_id} {new_session_id}.
    async fn kick(&self, device_id: &str, new_session_id: &str) -> Result<()>;

    /// Subscribe to kick:{device_id}. Yields when a kick arrives.
    /// Implementation: long-lived background task; sender side is `watch::Sender<()>`.
    async fn subscribe_kick(&self, device_id: &str) -> Result<KickSubscription>;

    /// Renew lease: EXPIRE lock:device:{id} 30 if value matches. Returns false if lost.
    async fn renew_lock(&self, device_id: &str, session_id: &str) -> Result<bool>;

    /// Release lock: DEL lock:device:{id} if value matches.
    async fn release_lock(&self, device_id: &str, session_id: &str) -> Result<()>;

    /// Bootstrap or sync seq counter from Hello.last_ack.
    /// If seq:{id} > last_ack: trim outbox up to last_ack, return current seq.
    /// If seq:{id} < last_ack: this is a wedged-device-after-restart case — set
    ///   seq:{id} = last_ack, leave outbox empty (nothing to replay), log warn.
    /// Returns the (now-correct) last_issued_seq.
    async fn reconcile_on_hello(
        &self,
        device_id: &str,
        session_id: &str,
        last_ack: u64,
    ) -> Result<u64>;

    /// Read all unacked frames for replay (XRANGE outbox:{id} (0-{last_ack} +).
    async fn unacked_frames(&self, device_id: &str, last_ack: u64) -> Result<Vec<Vec<u8>>>;

    /// Reserve the next seq atomically: fence + INCR. Returns the assigned
    /// seq. Caller is then expected to stamp `envelope.seq = assigned`,
    /// encode, and call [`Self::xadd_frame`].
    async fn fenced_incr_seq(&self, device_id: &str, session_id: &str) -> Result<u64>;

    /// Append the encoded frame to the device's stream at ID `0-{seq}`,
    /// applying `MAXLEN ~ 10000` and `EXPIRE 30d`. Fenced.
    async fn xadd_frame(
        &self,
        device_id: &str,
        session_id: &str,
        seq: u64,
        frame: Vec<u8>,
    ) -> Result<()>;

    /// Trim acked frames: XTRIM outbox:{id} MINID 0-{ack+1}. Fire-and-forget allowed.
    async fn observe_ack(&self, device_id: &str, ack: u64) -> Result<()>;
}

/// Returned by `subscribe_kick`. Drops the underlying Redis Pub/Sub connection
/// and aborts the background task when dropped. The `recv` future resolves
/// when a kick arrives or the subscription is dropped.
pub struct KickSubscription {
    pub recv: tokio::sync::watch::Receiver<()>,
    _drop_guard: tokio::task::JoinHandle<()>,
}
```

## Redis Schema

| Key | Type | Contents | Lifetime |
|---|---|---|---|
| `outbox:{device_id}` | Stream | Each entry: ID `0-{seq}`, field `frame=<encoded envelope bytes>` | `XTRIM MAXLEN ~ 10000` on every send; `XTRIM MINID 0-{peer_ack+1}` on ack; `EXPIRE 2592000` (30d) on every send |
| `seq:{device_id}` | String (u64 dec) | Monotonic counter, `INCR`-driven | `EXPIRE 2592000` on every send |
| `lock:device:{device_id}` | String | `{session_id}` (uuid v4) | `EX 30`, renewed every 10s |
| `kick:{device_id}` | Pub/Sub channel | One-shot kick events; payload = new session_id (informational only) | not persisted |

Stream entry IDs use the Redis `<ms>-<n>` format with `ms=0` so the daemon's logical seq number maps directly to the entry ID. Redis enforces strictly monotonic IDs, which gives us a free correctness check.

`MAXLEN ~ 10000` matches the existing in-memory `Outbox::new(10_000)` cap. Approximate trim (`~`) is O(1) amortized; exact trim is not required for correctness.

## Lua Scripts

All scripts are loaded once at hub startup (`SCRIPT LOAD`) and invoked via `EVALSHA`. On `NOSCRIPT` (e.g., Redis restart, FLUSHALL), the wrapper falls back to `EVAL` and re-caches the SHA.

### `acquire_lock`

```lua
-- KEYS[1] = lock:device:{id}
-- ARGV[1] = session_id
-- ARGV[2] = ttl_secs
local ok = redis.call('SET', KEYS[1], ARGV[1], 'NX', 'EX', ARGV[2])
if ok then return 1 else return 0 end
```

### `fenced_incr_seq`

```lua
-- KEYS[1] = lock:device:{id}
-- KEYS[2] = seq:{id}
-- ARGV[1] = session_id
-- ARGV[2] = retention_secs
if redis.call('GET', KEYS[1]) ~= ARGV[1] then
  return redis.error_reply('NOT_OWNER')
end
local seq = redis.call('INCR', KEYS[2])
redis.call('EXPIRE', KEYS[2], ARGV[2])
return seq
```

### `fenced_xadd`

```lua
-- KEYS[1] = lock:device:{id}
-- KEYS[2] = outbox:{id}
-- ARGV[1] = session_id
-- ARGV[2] = seq (decimal string)
-- ARGV[3] = frame (binary)
-- ARGV[4] = max_buffer (e.g., "10000")
-- ARGV[5] = retention_secs (e.g., "2592000")
if redis.call('GET', KEYS[1]) ~= ARGV[1] then
  return redis.error_reply('NOT_OWNER')
end
local id = '0-' .. ARGV[2]
redis.call('XADD', KEYS[2], 'MAXLEN', '~', ARGV[4], id, 'frame', ARGV[3])
redis.call('EXPIRE', KEYS[2], ARGV[5])
return 1
```

The two scripts together are what the trait's `send` method orchestrates. They are split rather than combined into one because the protobuf-encoded `Envelope` bytes must carry the assigned seq, and Lua cannot patch protobuf — so the seq must round-trip back to Rust before the bytes are encoded. See "Seq assignment + encoding ordering" under Connection Lifecycle for the rationale.

### `release_lock`

```lua
-- KEYS[1] = lock:device:{id}
-- ARGV[1] = session_id
if redis.call('GET', KEYS[1]) == ARGV[1] then
  return redis.call('DEL', KEYS[1])
end
return 0
```

### `reconcile_on_hello`

```lua
-- KEYS[1] = lock:device:{id}
-- KEYS[2] = seq:{id}
-- KEYS[3] = outbox:{id}
-- ARGV[1] = session_id
-- ARGV[2] = last_ack (u64)
-- ARGV[3] = retention_secs
if redis.call('GET', KEYS[1]) ~= ARGV[1] then
  return redis.error_reply('NOT_OWNER')
end
local current = tonumber(redis.call('GET', KEYS[2])) or 0
local last_ack = tonumber(ARGV[2])
if last_ack > current then
  -- Bootstrap path: server lost state, trust device's last_ack as the floor.
  redis.call('SET', KEYS[2], last_ack)
  redis.call('DEL', KEYS[3])
  redis.call('EXPIRE', KEYS[2], ARGV[3])
  return last_ack
end
-- Normal path: trim already-acked entries.
if last_ack > 0 then
  redis.call('XTRIM', KEYS[3], 'MINID', '0-' .. (last_ack + 1))
end
return current
```

The renew operation reuses a small Lua script (`if GET == session_id then EXPIRE end`) for the same atomicity reason.

## Connection Lifecycle

### `register` (called after Hello signature verification)

1. Generate `session_id = uuid::Uuid::new_v4()`.
2. Try `try_acquire_lock(device_id, session_id)`.
3. If lock is held by someone else:
   - `kick(device_id, session_id)` — publish on `kick:{device_id}`.
   - Retry `try_acquire_lock` up to 5 times with 200ms backoff.
   - If still failing → reject Hello with close frame `lock_contention` (rare; means the previous owner is unreachable from Redis Pub/Sub).
4. Once lock is held:
   - Spawn lease renewer task (loop: every 10s, `renew_lock`; on failure, signal close to the WS IO task).
   - Spawn kick subscriber task (subscribes to `kick:{device_id}`; on receive, signal close).
5. Call `reconcile_on_hello(device_id, session_id, hello.last_ack)`. This either trims acked entries (normal) or seeds the counter (bootstrap path for wedged devices). Logs a warn on the bootstrap branch.
6. `unacked_frames(device_id, hello.last_ack)` → push each frame into the connection's mpsc to be replayed to the device.
7. Return success; per-connection state lives in the `ConnectionEntry` until close.

### `send` (hub→device, e.g., from job dispatch)

The trait exposes two primitives — `fenced_incr_seq` and `xadd_frame` — and the gateway (`ConnectionRegistry::send`) orchestrates them:

1. `OutboxStore::fenced_incr_seq(device_id, session_id)` → returns the assigned seq.
2. Mutate `envelope.seq = assigned`, encode to bytes.
3. `OutboxStore::xadd_frame(device_id, session_id, seq, frame)` → writes the frame into the stream with explicit ID `0-{seq}`.
4. Push the encoded frame into the connection's mpsc to be sent over the WS.

On `NOT_OWNER` from either script: this replica has been kicked or its lock expired. Mark the connection dead, signal close to the WS IO task, log error. Caller (job dispatch) sees `DeviceOffline`.

The orchestration lives in the gateway rather than inside the store because the bytes-must-carry-the-correct-seq invariant requires going through `ahand-protocol::Envelope` encoding — and `ahand-hub-core` (where the trait lives) does not and should not depend on `ahand-protocol`.

**Seq assignment + encoding ordering.** The seq is part of the protobuf-encoded `Envelope` bytes, so the bytes must carry the correct seq before they're durable in Redis. Lua cannot patch protobuf, which is why the send flow round-trips through Rust between `fenced_incr_seq` and `xadd_frame`.

Crash between the two round-trips leaves a seq gap (no entry in stream for that seq). This is harmless: the WS message was never actually sent either, and the protocol tolerates seq gaps — devices ack the highest seq they received, not "I received contiguous N seqs".

The second round-trip's fence catches the rare race where the lock was lost between the two calls (e.g., Pub/Sub kick from another replica, partition-then-rejoin). On `NOT_OWNER` from the second call, the connection is closed; the seq is just burned.

### `observe_ack` (device→hub envelope carries `ack`)

1. Best-effort `OutboxStore::observe_ack(device_id, ack)` → `XTRIM outbox:{id} MINID 0-{ack+1}`.
2. Fire-and-forget; failure is logged but does not close the connection. Worst case: stale entries linger until next successful trim or until `MAXLEN ~` evicts them.
3. No fence on this path — trimming acked frames is idempotent and harmless even if a kicked-out replica accidentally trims (the surviving owner's view is unaffected because it operates on the same stream).

### `unregister` (WS closed cleanly or via shutdown signal)

1. Stop lease renewer task, stop kick subscriber task.
2. `release_lock(device_id, session_id)` — Lua-checked DEL.
3. Outbox stream and seq counter remain in Redis (subject to 30d EXPIRE). Next reconnect picks up where this one left off.

## Bootstrap Path (Wedged-Device Recovery)

The bootstrap path inside `reconcile_on_hello` is the explicit, one-line answer to today's incident:

```
if last_ack > current_seq:
    SET seq:{id} = last_ack
    DEL outbox:{id}            # any in-flight from a previous server instance is gone
    log warn "bootstrap: trusted device's last_ack as seq floor"
```

This means the very first hub deploy carrying this code unblocks every currently-wedged device on first reconnect, with no manual intervention. The unacked messages from before the original incident are lost (they were lost the moment the in-memory state was wiped, well before this fix); going forward, persistence prevents the loss.

## Failure Modes

| Scenario | Behavior |
|---|---|
| Redis unreachable on `register` | Hello rejected with close frame `service_unavailable`. Sentry alarm. Daemon retries with backoff. |
| Redis unreachable mid-session (send fails) | Connection closed, lock release best-effort skipped (lease will expire in ≤30s). Daemon reconnects. |
| `NOSCRIPT` (Redis FLUSHALL or restart) | Wrapper falls back to `EVAL`, re-caches SHA, retries. Transparent to caller. |
| Lease renewal fails (Redis blip, lock taken over) | Connection signaled to close. Daemon reconnects, normal flow takes over. |
| Pub/Sub disconnected | Resubscribe with backoff. Handoff degrades from <1s to ≤30s (lease expiry) until reconnected. Correctness unaffected because fencing is the source of truth. |
| Lock contention (kick fails to dislodge previous owner within 5 retries × 200ms) | Reject Hello with `lock_contention`. Daemon retries with backoff; lease will eventually expire. |
| Stream grows past `MAXLEN ~ 10000` | Approximate trim drops the oldest unacked entries. Affected device sees a permanent seq gap (some old hub→device messages will never be replayed). Log warn with device_id and trim count. Operationally: this is a long-offline device whose backlog overflowed; matches existing in-memory behavior. |
| Hub crash between `fenced_incr_seq` and `fenced_xadd` | Seq gap; no message in stream, none on the wire. On reconnect, replay continues from the next entry that does exist. Device's `local_ack` is unaffected. |
| Two replicas race on initial lock | One wins via `SET NX`, the other publishes kick and retries. The kick is mostly noise here (no current owner to kick), but the loser's retry will then succeed on the next attempt. |

## Test Matrix

### Unit tests

- `RedisOutboxStore` against a mocked / fake Redis (existing `MockRedis`-style harness if available, else a thin `OutboxStore` impl in tests).
- Argument validation, error mapping (NOSCRIPT, NOT_OWNER, connection refused).
- Lua script SHA caching + fallback to EVAL.
- `ConnectionRegistry` with a fake `OutboxStore`: register/send/ack/replay/kick.
- Bootstrap-path branch (`reconcile_on_hello` with `last_ack > current_seq`) — both unit-level and through-the-registry.

### Integration tests (testcontainer Redis, mirrors `tests/store_roundtrip.rs`)

- End-to-end `register → send N messages → device acks K → device reconnects with last_ack=K → unacked frames replay`.
- Hub restart simulation: spin up `RedisOutboxStore` instance A, send messages, drop A, spin up B, simulate device reconnect → all unacked frames returned.
- Lock takeover: replica A holds lock, replica B's `register` publishes kick → A's kick subscriber fires close → B acquires within 1s.
- Bootstrap path: empty Redis + Hello with `last_ack=9` → register succeeds, `seq:{id}` set to 9, no replay needed.
- Fencing: replica A holds lock, manually `SET lock:device:{id} other_session` to simulate takeover → A's next `send` returns NOT_OWNER.
- `MAXLEN ~` trim: send >10k messages, verify oldest entries are dropped and replay returns the survivors.
- Redis-unreachable: stop the testcontainer mid-session, observe registry behavior matches "fail closed" contract.

### End-to-end (extends existing zombie-WS reconnect harness)

- Boot hub → connect daemon → enqueue several jobs (hub→device messages) → kill hub task → boot new hub task → daemon reconnects → assert all enqueued jobs were delivered (no loss). This is the primary regression test for the original incident.

### Performance smoke

- `send` latency p50/p99 under steady load (1 device, 1000 msgs/s) before vs. after — record numbers in PR description for future reference. No SLA gate; goal is to confirm we're in single-digit milliseconds and not e.g. 100ms.

## Deployment / Rollout

- **Infra:** none. Reuses existing `REDIS_URL` SSM parameter at `/ahand-hub/{env}/REDIS_URL`.
- **Schema migration:** none (Redis-only state, no Postgres changes).
- **Feature flag:** none. The change is non-optional; A/B testing a half-broken outbox doesn't make sense.
- **Rollout order:**
  1. Merge to `dev` branch → CI full pass.
  2. Deploy to `ahand-hub-dev` (existing GitHub Actions workflow `deploy-hub.yml`).
  3. Verify dev devices auto-recover (currently wedged) via CloudWatch logs: `device socket ended with error error=invalid peer ack` should drop to zero within minutes.
  4. Soak ≥24h, monitor Sentry.
  5. PR to `main` → deploy to `ahand-hub-prod`.
- **Rollback:** redeploy the previous task definition. Redis state authored by the new code is forward-only data (extra keys with TTL); the old code simply ignores it and falls back to in-memory. No cleanup needed.

## Backwards Compatibility

- **Wire protocol:** unchanged. Devices still send `Hello.last_ack`, server still replies with `HelloAccepted`.
- **Daemon (ahandd):** unchanged. The pinned revision in team9-client (`6dac9028`) keeps working without updates.
- **Old hub binaries during rollout:** if a partial rollout has both old and new replicas active simultaneously (multi-replica future), the old replica won't acquire the Redis lock and will keep using its in-memory outbox — devices routed to it would not get the durability guarantee but won't be broken. Today this is moot because there's only 1 replica.

## Out of Scope

- D3 cross-AZ persistence guarantees.
- Device-side persistence of `local_ack`.
- Removing the in-memory `Outbox` type from `ahand-hub-core`.
- Per-device send batching / pipelining.
- Pub/Sub at-least-once delivery (Pub/Sub is best-effort by design and only used for handoff acceleration).
- Reverse direction (device→hub) outbox; that already has at-least-once via the daemon's in-memory outbox + replay-on-reconnect, and a hub crash on the inbound path doesn't lose data because the device retransmits.

## Open Questions

- **Lease/renewal timings.** 30s lease + 10s renewal is a reasonable starting point; revisit if Redis latency observed in prod warrants tuning.
- **`MAXLEN ~ 10000`.** Matches existing in-memory cap. If we observe legitimate offline-device backlogs >10k, raise the cap or add per-device override; out of scope here.
- **Sentry / metrics surface.** Out of scope for this design but should be picked up in the implementation plan: counter for `outbox_send_not_owner`, histogram for `outbox_send_latency`, gauge for active fenced sessions, alert on Redis unreachable.
