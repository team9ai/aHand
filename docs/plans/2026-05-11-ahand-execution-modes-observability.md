# AHand 三种执行模式与过程数据可观测性改造计划

**Goal:** 将 AHand 的命令执行模型从单一 `interactive: bool` 语义升级为明确的三种执行模式：`batch`、`pty`、`pipe_stream`，并保证每种模式下子进程中间结果、agent 过程日志、任务状态、审计和诊断数据都可被采集、恢复和展示。

**Architecture:** 协议层新增 `ExecutionMode`，daemon 根据执行模式选择 batch / PTY / pipe-stream runtime；hub 继续以 job 为核心收敛状态和输出流；dashboard 在同一 job 页面展示实时输出、过程事件、artifact/log 和终态摘要。旧的 `interactive` 字段保留兼容，但新代码以 `execution_mode` 为准。

**Tech Stack:** Rust, protobuf/prost, TypeScript/ts-proto, Axum, Redis Streams, PostgreSQL, Next.js dashboard

**Related Docs:**
- `docs/architecture-overview.md` Section 3.4.2: 进程中间结果可观测性
- `docs/architecture-overview.md` Section 3.4.3: 过程数据采集契约
- `docs/remote-control-roadmap.md` Section 1: Remote Shell / Terminal

---

## 目标模型

### 三种执行模式

| 模式 | 语义 | 子进程环境 | 输入 | 输出 | 典型场景 |
|---|---|---|---|---|---|
| `batch` | 一次性命令或持续输出命令 | 普通 child process，非 TTY | 启动后不维护持续 stdin | stdout / stderr 分离流 | `git status`、`cargo test`、构建任务、日志 tail |
| `pty` | 终端交互任务 | 子进程绑定 pseudo-terminal | 支持按键输入和 resize | PTY 原始字节流 | shell、`vim`、`top`、安装向导、必须依赖 TTY 的全屏 TUI |
| `pipe_stream` | 全双工非 TTY pipe | 普通 child process，非 TTY | 持续写入 child stdin | stdout / stderr 分离流 | Claude Code、Codex、简单 REPL、语言服务器、需要 stdin 的 agent/工具 |

### 兼容策略

SDK 可以通过升级版本号完成面向 SDK 用户的 API 迁移，但不能把 SDK 升级当作唯一兼容边界。AHand 仍然需要在 hub HTTP API 和 protobuf wire protocol 层兼容旧字段，因为实际部署中可能同时存在：

- 旧 SDK 调用新版 hub
- 新 SDK 调用旧 hub
- 新 hub 投递任务给旧 daemon
- 旧 dashboard / 脚本 / curl 直接调用 hub HTTP API
- OpenClaw gateway 路径绕过新版 SDK

因此兼容策略分层处理：

| 层 | 兼容策略 |
|---|---|
| SDK public API | 新版 SDK 主推 `executionMode`；`interactive` 标记 deprecated。未来 major 版本可以从 SDK 类型中移除 `interactive`。 |
| Hub HTTP API | 继续接受 `interactive`，同时新增 `execution_mode`。如果两者同时存在，以 `execution_mode` 为准。 |
| Protobuf wire protocol | 保留 `JobRequest.interactive = 7`，新增 `execution_mode` 字段。不得删除、改名、复用或重排旧字段 tag。 |
| Daemon runtime | 新 daemon 优先读取 `execution_mode`；缺失时从 `interactive` 推导。 |
| Database | 新增列必须有默认值或兼容读取逻辑；历史 job 没有 `execution_mode` 时必须能推导为 `batch` / `pty`。 |
| Dashboard | 新 UI 只使用 `execution_mode`；展示旧 job 时从 `interactive` 或数据库默认值推导。 |
| OpenClaw | 继续映射现有请求到兼容模式，默认 `batch`，除非协议显式支持 PTY 或 pipe-stream。 |

现有协议只有 `JobRequest.interactive: bool`：

```text
interactive=false -> batch
interactive=true  -> pty
```

改造后新增 `execution_mode`：

```text
execution_mode = EXECUTION_MODE_BATCH
execution_mode = EXECUTION_MODE_PTY
execution_mode = EXECUTION_MODE_PIPE_STREAM
```

兼容规则：

1. 如果 `execution_mode` 非 unspecified，则以 `execution_mode` 为准。
2. 如果 `execution_mode` unspecified，则按旧字段推导：`interactive=true` 为 `pty`，否则为 `batch`。
3. 出站消息在过渡期同时填充 `execution_mode` 和 `interactive`。
4. dashboard、SDK 和 hub API 对外优先暴露 `execution_mode`，但短期继续接受 `interactive`。
5. `pipe_stream` 不能对旧 daemon 静默降级。若目标设备未声明支持 `pipe_stream`，hub 必须返回明确错误，例如 `409 device does not support pipe_stream execution mode`。
6. 数据库历史 job 没有 `execution_mode` 时，读取层应按旧字段或默认规则推导为 `batch` / `pty`，不要让旧数据在 dashboard 显示为 unknown。
7. `interactive` 在协议层长期保留为 compatibility field；弃用只发生在 SDK 和新 UI 的开发接口层。
8. 新 SDK 调旧 hub 时必须兼容：SDK 不能只发送旧 hub 不认识的 `execution_mode` 字段；迁移期应同时发送 `interactive`，或在探测到旧 hub 后只发送旧字段。

### Compatibility Checklist

- [ ] 旧 SDK -> 新 hub：只发 `interactive` 时，新 hub 仍可创建 `batch` / `pty` job。
- [ ] 新 SDK -> 旧 hub：SDK 发送请求时必须包含旧 hub 能识别的 `interactive`；旧 hub 忽略或拒绝未知字段时仍不影响 `batch` / `pty`。
- [ ] 新 SDK -> 旧 hub 请求 `pipe_stream`：必须在 SDK 或 hub 能力探测阶段失败，不能伪装成 `batch`。
- [ ] 新 hub -> 旧 daemon：hub 必须填充 `interactive`；旧 daemon 忽略 `execution_mode` 后仍可执行 `batch` / `pty`。
- [ ] 新 hub -> 旧 daemon 请求 `pipe_stream`：hub 必须通过设备 capability 拒绝，不投递。
- [ ] 旧数据库行：没有 `execution_mode` 的历史 job 在 API 和 dashboard 中显示为推导模式。
- [ ] 数据库迁移：新增列必须可空或有默认值，迁移过程中旧 hub 版本仍能读写未升级字段。
- [ ] 数据库回滚：如果回滚到旧 hub，旧 hub 不依赖新列；新列存在不应破坏旧查询。
- [ ] protobuf fixtures：所有旧 fixtures 仍可 decode，新 fixtures 不复用旧 tag。
- [ ] OpenClaw 路径：不依赖新版 SDK 的调用仍保留原行为。

---

## 过程数据要求

每个 job 必须形成完整的数据链：

```text
job create
-> dispatch envelope
-> local policy / approval
-> process spawn
-> stdout/stderr/pty/progress stream
-> optional stdin/resize input
-> agent logs / transcript / artifacts
-> finish / fail / cancel / timeout / disconnect
-> audit
```

必须保证：

1. 所有实时输出进入 hub `OutputStream`，生产路径使用 Redis-backed persistent output。
2. daemon 本地继续写 `~/.ahand/data/trace.jsonl` 和 `~/.ahand/data/runs/{job_id}`。
3. `batch` 和 `pipe_stream` 保留 stdout / stderr 通道差异。
4. `pty` 保留原始终端字节流，不按行解析，不裁剪 ANSI 控制序列。
5. `codex` / `claude` 等 agent 的 transcript、JSONL log、debug log 必须能通过 job metadata、env 或 artifact API 定位。
6. dashboard 展示 running job 时必须显示最近输出、当前状态、开始时间、持续时间、最近更新时间和终态占位。
7. 断线、超时、取消、审批拒绝都必须留下过程事件和审计记录。

推荐 daemon 注入环境变量：

```text
AHAND_JOB_ID={job_id}
AHAND_EXECUTION_MODE=batch|pty|pipe_stream
AHAND_RUN_DIR=~/.ahand/data/runs/{job_id}
```

推荐 agent 过程 metadata：

```text
agent_kind
agent_session_id
agent_log_path
agent_workspace
agent_model
agent_phase
agent_last_event_ms
```

---

## File Structure

### Protocol

| File | Action | Responsibility |
|---|---|---|
| `proto/ahand/v1/envelope.proto` | Modify | Add `ExecutionMode` enum and `JobRequest.execution_mode` |
| `crates/ahand-protocol/tests/golden_envelope.rs` | Modify | Add/refresh golden fixtures for execution modes |
| `crates/ahand-protocol/tests/fixtures/*` | Modify | Add frozen binary fixtures |
| `packages/proto-ts/src/generated/*` | Generate | TypeScript generated protobuf updates |
| `crates/ahand-protocol/src/lib.rs` | Generate | Rust generated protobuf updates if needed by build script |

### Daemon

| File | Action | Responsibility |
|---|---|---|
| `crates/ahandd/src/executor.rs` | Modify | Split execution into batch, pty, pipe_stream paths |
| `crates/ahandd/src/registry.rs` | Modify | Track stdin sender for PTY and pipe_stream jobs |
| `crates/ahandd/src/ahand_client.rs` | Modify | Resolve execution mode, route StdinChunk/TerminalResize correctly |
| `crates/ahandd/src/ipc.rs` | Modify | Support execution mode over local IPC if relevant |
| `crates/ahandd/src/store.rs` | Modify | Persist execution mode, process metadata, run artifacts |
| `crates/ahandd/src/openclaw/handler.rs` | Modify | Map OpenClaw requests into the new execution mode model |

### Hub

| File | Action | Responsibility |
|---|---|---|
| `crates/ahand-hub/src/http/jobs.rs` | Modify | Accept `execution_mode`, preserve `interactive` compatibility |
| `crates/ahand-hub-core/src/job.rs` | Modify | Add execution mode to job domain model |
| `crates/ahand-hub-store/src/job_store.rs` | Modify | Persist execution mode and process metadata |
| `crates/ahand-hub-store/migrations/*.sql` | Create | Add database columns for execution mode / metadata |
| `crates/ahand-hub/src/output_stream.rs` | Modify | Preserve ordered output, expose process events if added |
| `crates/ahand-hub/src/http/terminal.rs` | Modify | Bind terminal WS only to PTY jobs; reject unsupported modes |

### SDK / Dashboard

| File | Action | Responsibility |
|---|---|---|
| `packages/sdk/src/cloud-client.ts` | Modify | Add `executionMode` request option |
| `packages/sdk/src/connection.ts` | Modify | Send protobuf `execution_mode` |
| `apps/hub-dashboard/src/lib/api.ts` | Modify | Add execution mode to job API types |
| `apps/hub-dashboard/src/components/device-terminal.tsx` | Modify | Start jobs with `pty`; avoid terminal UI for batch-only jobs |
| `apps/hub-dashboard/src/components/device-jobs-panel.tsx` | Modify | Show execution mode, output, duration, recent activity |
| `apps/hub-dashboard/src/components/job-output-viewer.tsx` | Modify | Render batch/pipe output and link artifacts/logs |

---

## Task 1: Protocol model

**Goal:** Make execution mode explicit on the wire while keeping old clients compatible.

**Steps:**

- [ ] Add `ExecutionMode` enum to `proto/ahand/v1/envelope.proto`.

```proto
enum ExecutionMode {
  EXECUTION_MODE_UNSPECIFIED = 0;
  EXECUTION_MODE_BATCH = 1;
  EXECUTION_MODE_PTY = 2;
  EXECUTION_MODE_PIPE_STREAM = 3;
}
```

- [ ] Add `ExecutionMode execution_mode = <next_tag>;` to `JobRequest`.
- [ ] Keep `bool interactive = 7` and document it as compatibility-only. Do not delete it even if the SDK no longer exposes it in a future major version.
- [ ] Ensure new senders fill both fields during migration:

```text
execution_mode=batch       -> interactive=false
execution_mode=pty         -> interactive=true
execution_mode=pipe_stream -> interactive=false
```

- [ ] Ensure old receivers can ignore the unknown `execution_mode` field and still execute based on `interactive`.
- [ ] Ensure new receivers can decode old messages with no `execution_mode`.
- [ ] Regenerate Rust and TypeScript protobuf outputs.
- [ ] Add golden tests for:
  - unspecified + `interactive=false`
  - unspecified + `interactive=true`
  - explicit `batch`
  - explicit `pty`
  - explicit `pipe_stream`
  - explicit `pipe_stream` with `interactive=false`

**Acceptance Criteria:**

- [ ] Old fixtures still decode.
- [ ] New fixtures are wire-compatible and stable.
- [ ] Rust and TS generated types expose `ExecutionMode`.
- [ ] No tag reuse or field renumbering.
- [ ] Removing `interactive` is explicitly out of scope.

**Verify:**

```bash
pnpm generate
cargo test -p ahand-protocol
pnpm test:ts
```

---

## Task 2: Shared execution mode resolver

**Goal:** Centralize compatibility logic so every caller interprets jobs the same way.

**Steps:**

- [ ] Add a daemon-side helper:

```rust
fn resolve_execution_mode(req: &JobRequest) -> ExecutionMode {
    match ExecutionMode::try_from(req.execution_mode).unwrap_or(ExecutionMode::Unspecified) {
        ExecutionMode::Batch => ExecutionMode::Batch,
        ExecutionMode::Pty => ExecutionMode::Pty,
        ExecutionMode::PipeStream => ExecutionMode::PipeStream,
        ExecutionMode::Unspecified => {
            if req.interactive {
                ExecutionMode::Pty
            } else {
                ExecutionMode::Batch
            }
        }
    }
}
```

- [ ] Add equivalent hub-side resolver for API requests.
- [ ] Add equivalent SDK helper for outgoing requests.
- [ ] Ensure logs and run artifacts write the resolved mode, not just raw request fields.

**Acceptance Criteria:**

- [ ] All job execution paths call the same resolver.
- [ ] Compatibility behavior is covered by unit tests.
- [ ] Invalid/unknown enum values degrade to unspecified, then old `interactive` mapping.

**Verify:**

```bash
cargo test -p ahandd
cargo test -p ahand-hub
pnpm test:ts
```

---

## Task 3: Daemon batch mode hardening

**Goal:** Preserve current non-PTY behavior but make it an explicit `batch` runtime.

**Steps:**

- [ ] Rename or wrap the current `interactive=false` executor path as `run_job_batch`.
- [ ] Ensure batch mode:
  - spawns without PTY
  - reads stdout and stderr concurrently
  - forwards chunks immediately as `JobEvent`
  - writes local `stdout` and `stderr` run files
  - records `start_ms`, resolved tool, cwd, timeout, pid if available
  - does not register a stdin sender
  - rejects `StdinChunk` and `TerminalResize` with diagnostic logs
- [ ] Inject `AHAND_JOB_ID`, `AHAND_EXECUTION_MODE=batch`, `AHAND_RUN_DIR`.

**Acceptance Criteria:**

- [ ] Current non-interactive jobs behave the same externally.
- [ ] Output is visible before process exit.
- [ ] stdout and stderr remain separate.
- [ ] Run artifact includes execution mode and basic process metadata.

**Verify:**

```bash
cargo test -p ahandd job_request_tool
cargo test -p ahand-hub job_flow
```

---

## Task 4: Daemon PTY mode formalization

**Goal:** Preserve current `interactive=true` behavior but make it explicit `pty` mode.

**Steps:**

- [ ] Rename or wrap current PTY path as `run_job_pty`.
- [ ] Ensure PTY mode:
  - spawns with pseudo-terminal
  - registers stdin sender
  - accepts `StdinChunk`
  - accepts `TerminalResize`
  - forwards PTY master bytes without rewriting
  - records local run artifact with execution mode
  - writes terminal stream to a mode-appropriate artifact, e.g. `terminal` or `stdout`
- [ ] Inject `AHAND_JOB_ID`, `AHAND_EXECUTION_MODE=pty`, `AHAND_RUN_DIR`.
- [ ] Ensure hub terminal WebSocket rejects non-PTY jobs with a clear error.

**Acceptance Criteria:**

- [ ] Existing interactive terminal continues to work.
- [ ] Full-screen TUI output is not line-parsed or escaped.
- [ ] Resize still updates the PTY.
- [ ] Dashboard terminal only attaches to PTY jobs.

**Verify:**

```bash
cargo test -p ahandd
cargo test -p ahand-hub terminal
pnpm test:hub-dashboard
```

---

## Task 5: Implement pipe_stream mode

**Goal:** Add full-duplex non-TTY execution with persistent stdin and separated stdout/stderr.

**Steps:**

- [ ] Add `run_job_pipe_stream` in daemon executor.
- [ ] Spawn child with:

```rust
stdin(Stdio::piped())
stdout(Stdio::piped())
stderr(Stdio::piped())
```

- [ ] Register a stdin sender in `JobRegistry`.
- [ ] Route `StdinChunk` to child stdin.
- [ ] Decide EOF behavior:
  - empty stdin chunk is data, not EOF
  - add a future explicit stdin-close message if needed
  - for V1, closing the terminal/input bridge should not automatically kill the job unless requested
- [ ] Continue reading stdout and stderr concurrently.
- [ ] Reject or ignore `TerminalResize` with diagnostic logs.
- [ ] Inject `AHAND_JOB_ID`, `AHAND_EXECUTION_MODE=pipe_stream`, `AHAND_RUN_DIR`.

**Acceptance Criteria:**

- [ ] A process can receive multiple stdin chunks after startup.
- [ ] stdout and stderr remain separated.
- [ ] No PTY is allocated.
- [ ] Resize does not affect pipe_stream jobs.
- [ ] Cancel still terminates the child process.

**Verify:**

```bash
cargo test -p ahandd pipe_stream
cargo test -p ahand-hub
```

Recommended test command:

```bash
python -u -c "import sys; [print('echo:'+line.strip(), flush=True) for line in sys.stdin]"
```

---

## Task 6: Hub API and persistence

**Goal:** Make execution mode first-class in control plane APIs and durable job records.

**Steps:**

- [ ] Extend `CreateJobRequest` with `execution_mode?: "batch" | "pty" | "pipe_stream"`.
- [ ] Continue accepting `interactive?: boolean`.
- [ ] If both `execution_mode` and `interactive` are present, resolve by `execution_mode` and log/debug-track the deprecated `interactive` field if it conflicts.
- [ ] Resolve mode before creating the protobuf `JobRequest`.
- [ ] When dispatching to daemon, fill compatibility `interactive` from the resolved mode.
- [ ] Store resolved execution mode on the job record.
- [ ] Add DB migration for `jobs.execution_mode`.
- [ ] Prefer additive, backward-compatible migration:
  - add nullable `execution_mode` first, or add `NOT NULL DEFAULT 'batch'`
  - avoid dropping or renaming existing columns
  - keep old queries valid
  - make new code tolerate `NULL`
- [ ] Backfill or default existing rows:

```sql
execution_mode = CASE
  WHEN interactive = true THEN 'pty'
  ELSE 'batch'
END
```

- [ ] If the current `jobs` table does not persist `interactive`, backfill existing rows to `'batch'` and let currently-running interactive sessions remain governed by in-memory/runtime state.
- [ ] Ensure old hub binaries can run against the migrated database:
  - old `INSERT` statements should not need to provide `execution_mode`
  - old `SELECT` statements should not break because a new column exists
  - old update paths should not be required to maintain `execution_mode`
- [ ] Ensure new hub can read pre-migration rows where `execution_mode` is missing or null.
- [ ] Reject `pipe_stream` if the target device capability does not include pipe-stream support.
- [ ] Consider a JSONB `process_metadata` or `metadata` column for:
  - agent_kind
  - agent_session_id
  - agent_log_path
  - agent_workspace
  - agent_model
  - agent_phase
  - agent_last_event_ms
- [ ] Include execution mode and metadata in `GET /api/jobs` and `GET /api/jobs/{id}`.

**Acceptance Criteria:**

- [ ] API callers can request all three modes.
- [ ] Existing `interactive` API callers still work.
- [ ] Job list/detail responses show resolved mode.
- [ ] Migration is backward-compatible for existing rows.
- [ ] Old hub binaries can still write jobs after the migration if rollback is needed.
- [ ] New hub can read old rows and old hub-created rows.
- [ ] `pipe_stream` requests to unsupported old daemon/device fail clearly instead of silently running as batch.

**Verify:**

```bash
cargo test -p ahand-hub
cargo test -p ahand-hub-store
```

---

## Task 7: Output stream and process events

**Goal:** Ensure users can observe both output and process lifecycle while jobs run.

**Steps:**

- [ ] Keep current `stdout`, `stderr`, `progress`, `finished` SSE events.
- [ ] Confirm Redis-backed `JobOutputStore` preserves ordering and can replay from history.
- [ ] Add process lifecycle events if needed:
  - `process_started`
  - `stdin_received`
  - `resize_received`
  - `artifact_discovered`
  - `agent_phase`
- [ ] If adding events, decide whether to extend `JobEvent` protobuf or model process events as hub-side records.
- [ ] Ensure event sequence numbers are monotonic per job.
- [ ] Add max size / retention policy for output history.

**Acceptance Criteria:**

- [ ] Dashboard can recover output after reconnect.
- [ ] Long-running jobs expose recent activity timestamp.
- [ ] Process lifecycle errors are visible, not only written to server logs.
- [ ] Large outputs do not exhaust memory.

**Verify:**

```bash
cargo test -p ahand-hub job_output_resume
cargo test -p ahand-hub-store --features test-support --test store_roundtrip
```

---

## Task 8: Agent log and artifact discovery

**Goal:** Make Codex/Claude logs and transcripts discoverable as job-associated data.

**Steps:**

- [ ] Ensure daemon creates `AHAND_RUN_DIR` before spawn.
- [ ] Write `request.json`, output files, `result.json`, and `process.json`.
- [ ] Add optional `artifacts.json` in the run directory:

```json
{
  "job_id": "job-id",
  "artifacts": [
    {
      "kind": "agent_transcript",
      "path": "/home/user/.ahand/data/runs/job-id/transcript.jsonl",
      "mime": "application/jsonl",
      "size_bytes": 1234,
      "updated_at_ms": 1770000000000
    }
  ]
}
```

- [ ] Add configurable env for known agent tools:
  - `CODEX_LOG_DIR` or supported Codex env if available in deployment
  - `CLAUDE_LOG_DIR` or supported Claude env if available in deployment
  - fallback to `AHAND_RUN_DIR`
- [ ] If agent writes logs outside `AHAND_RUN_DIR`, record explicit path in metadata.
- [ ] Expose artifact read path through existing file API first.
- [ ] Design follow-up `GET /api/jobs/{id}/artifacts` only after file API path is validated.

**Acceptance Criteria:**

- [ ] Every job has a run directory.
- [ ] Agent logs can be located from job detail data.
- [ ] Missing logs are represented explicitly, not silently ignored.
- [ ] Sensitive artifact access respects file policy and dashboard auth.

**Verify:**

```bash
cargo test -p ahandd
cargo test -p ahand-hub http_files
```

---

## Task 9: Dashboard UX

**Goal:** Show execution mode and process data clearly without mixing terminal and log semantics.

**Steps:**

- [ ] Job list:
  - show mode badge: batch / pty / pipe_stream
  - show status, device, created_at, started_at, duration
  - show last output time or last event time
- [ ] Job detail:
  - show realtime output viewer for batch / pipe_stream
  - show terminal viewer for pty
  - show stdout/stderr channel filters where meaningful
  - show process metadata
  - show artifact/log links
  - show audit events for the job
- [ ] Terminal tab:
  - create PTY jobs by default
  - prevent attaching terminal WS to batch jobs
  - decide whether pipe_stream gets a separate input panel or a compact stdin composer
- [ ] Reconnect behavior:
  - resume SSE with `Last-Event-ID`
  - keep running state visible while output is quiet

**Acceptance Criteria:**

- [ ] Operators can see live output before job finishes.
- [ ] Operators can distinguish stdout, stderr, PTY output, and artifacts.
- [ ] No UI path implies batch output is an interactive terminal.
- [ ] Job detail is useful for failed Codex/Claude runs.

**Verify:**

```bash
pnpm test:hub-dashboard
```

---

## Task 10: SDK and compatibility

**Goal:** Let cloud-side callers use new modes without breaking existing SDK users.

**Steps:**

- [ ] Add `executionMode?: "batch" | "pty" | "pipe_stream"` to SDK request types.
- [ ] Keep `interactive?: boolean` as deprecated compatibility input.
- [ ] Resolve and send both fields during migration when talking to a hub/API version that accepts both.
- [ ] For compatibility with old hub versions, the SDK request body must include `interactive` for all requests even when callers use only `executionMode`; `pipe_stream` uses `interactive=false` as the compatibility bool value, but correct stream semantics still require explicit `executionMode`.
- [ ] Add hub capability/version detection if available:
  - prefer `GET /api/system` or equivalent if it exposes supported execution modes
  - cache capability result per client instance
  - if capability endpoint is missing, assume old hub
- [ ] Old hub fallback behavior:
  - `executionMode=batch` -> send `interactive=false`
  - `executionMode=pty` -> send `interactive=true`
  - `executionMode=pipe_stream` -> send `executionMode=pipe_stream` and `interactive=false`; mixed-version deployments should gate this by hub/device capability to avoid old hubs treating it as plain batch
- [ ] Avoid requiring users to know hub version manually.
- [ ] In the next SDK major version, it is acceptable to remove `interactive` from SDK public types, but not from protobuf or hub HTTP compatibility handling.
- [ ] Document that `executionMode: "pipe_stream"` requires a hub and daemon that implement pipe-stream. Do not rely on the compatibility `interactive=false` fallback for stream workloads.
- [ ] Add callback support for process events if Task 7 adds them.
- [ ] Update SDK docs and tests.

**Acceptance Criteria:**

- [ ] Existing SDK callers compile unchanged.
- [ ] New callers can request `pipe_stream`.
- [ ] New SDK can run `batch` and `pty` jobs against old hub versions.
- [ ] New SDK can request `pipe_stream`; production callers should use capability/version gating when mixed hub or daemon versions are present.
- [ ] `interactive=true` still maps to PTY.
- [ ] TypeScript tests cover mode serialization.
- [ ] SDK versioning policy is documented: SDK API may drop `interactive` in a major release; wire protocol keeps it.

**Verify:**

```bash
pnpm test:ts
```

---

## Task 11: OpenClaw gateway compatibility

**Goal:** Keep OpenClaw integration working while using the new daemon execution internals.

**Steps:**

- [ ] Map existing OpenClaw `system.run` requests to `batch` by default.
- [ ] Preserve current behavior for requests that need local approval.
- [ ] Decide how OpenClaw can request PTY or pipe_stream, if supported by its protocol.
- [ ] Ensure OpenClaw execution output remains observable through its existing response/event shape.
- [ ] Record AHand run artifacts even when invoked through OpenClaw.

**Acceptance Criteria:**

- [ ] Existing Team9/OpenClaw flow is unchanged externally.
- [ ] AHand local run artifacts are still created.
- [ ] Session mode and approval behavior remain consistent.

**Verify:**

```bash
cargo test -p ahandd openclaw
```

---

## Task 12: End-to-end test matrix

**Goal:** Prove the three modes and process data work across daemon, hub, SDK, and dashboard.

### Mode behavior

- [ ] `batch`: run `printf hello`, observe stdout before finish.
- [ ] `batch`: run stderr-producing command, observe stderr separately.
- [ ] `pty`: start shell, send input, receive terminal output.
- [ ] `pty`: run full-screen-ish command or ANSI output, verify raw bytes render in terminal.
- [ ] `pipe_stream`: start stdin-consuming process, send multiple input chunks, observe stdout.
- [ ] `pipe_stream`: verify `TerminalResize` is rejected/ignored.

### Process data

- [ ] Every job has `request.json`.
- [ ] Every completed job has `result.json`.
- [ ] stdout/stderr artifacts are written for batch and pipe_stream.
- [ ] PTY stream artifact is written for PTY mode.
- [ ] `trace.jsonl` includes inbound/outbound envelopes.
- [ ] hub job record includes execution mode.
- [ ] dashboard shows mode and live output.
- [ ] Redis-backed output can replay after reconnect.

### Failure cases

- [ ] command not found
- [ ] timeout
- [ ] cancel running job
- [ ] daemon disconnect mid-job
- [ ] session inactive denial
- [ ] strict approval denial
- [ ] large output
- [ ] invalid execution mode

**Verify:**

```bash
cargo test -p ahand-protocol
cargo test -p ahandd
cargo test -p ahand-hub-core
cargo test -p ahand-hub-store
cargo test -p ahand-hub
pnpm test:ts
pnpm test:hub-dashboard
pnpm test:e2e:scripts
```

---

## Rollout Plan

### Phase 1: Protocol and compatibility

- Add `ExecutionMode`.
- Keep `interactive` compatibility.
- Add resolvers and tests.
- No behavior change intended.

### Phase 2: Daemon runtime split

- Make current paths explicit `batch` and `pty`.
- Add `AHAND_*` env and richer run artifacts.
- Preserve current dashboard/API behavior.

### Phase 3: Hub/API persistence

- Add `execution_mode` to job API and database.
- Expose mode in job list/detail.
- Confirm output replay and audit behavior.

### Phase 4: pipe_stream

- Add stdin-piped runtime.
- Reuse the existing job stdin route for pipe_stream jobs, and keep terminal resize meaningful only for PTY.
- Add tests for full-duplex non-TTY commands.

### Phase 5: Process data and artifacts

- Add process metadata.
- Add agent log/artifact discovery.
- Add dashboard job detail panels.

### Phase 6: Cleanup

- Mark `interactive` deprecated in docs and SDK.
- Stop new UI code from using `interactive` directly.
- Keep wire compatibility indefinitely unless a v2 protocol is introduced.

---

## Open Questions

1. Should `pipe_stream` input reuse `/ws/terminal`, or should it get a separate `/ws/jobs/{id}/input` channel?
2. Do we need a protobuf `ProcessEvent` message, or can process events live purely in hub-side output/artifact records?
3. Should PTY output be persisted as stdout for compatibility, or as a distinct terminal stream artifact?
4. What is the authoritative way to configure Codex and Claude transcript/log output paths in the deployment environment?
5. Should `agent_phase` be inferred from logs, sent explicitly by wrappers, or left to future agent-specific adapters?
6. What retention policy should apply to Redis output streams and local run artifacts?
7. Which fields must be redacted from args/env/transcripts before dashboard display?

---

## Definition of Done

- [ ] `batch`, `pty`, and `pipe_stream` are explicit protocol/API concepts.
- [ ] Old `interactive` clients still work.
- [ ] All three modes produce live observable output.
- [ ] Job records persist resolved execution mode.
- [ ] Daemon writes per-job run artifacts and envelope trace.
- [ ] Hub output can be replayed after dashboard reconnect.
- [ ] Dashboard shows mode, live output, process metadata, artifacts, and audit context.
- [ ] Codex/Claude-style long-running jobs can be diagnosed from job output plus local transcript/log artifacts.
- [ ] Unit, integration, SDK, dashboard, and E2E tests cover success and failure paths.
