# Run Claude Code or Codex Through AHand Pipe Stream

This guide describes the full path from configuring AHand to calling Codex and process-style agent commands without PTY. The process transport is **single-turn CLI launch**: AHand starts the agent command with `pipe_stream`, forwards the prompt for this run, and streams stdout/stderr back.

AHand now separates three stable knobs. See `docs/agent-stdio-formats.md` for the long-term contract:

```text
executionMode=pipe_stream
  process transport: child stdin/stdout/stderr pipes

inputFormat
  AHand prompt/stdin -> child stdin format

outputFormat
  child stdout format -> raw bytes or AgentObservationRecord JSONL
```

For Codex, `inputFormat=text` writes the prompt to stdin, while `outputFormat=codex-jsonl` parses Codex JSONL stdout into normalized observation JSONL. For Claude Code and Hermes ACP, use their dedicated usage guides because their stdin and stdout formats are different. **Caller-facing stdout is still the user-facing data source.** With `outputFormat=raw`, stdout/stderr are raw child-process streams. With an agent-specific `outputFormat`, stdout returned to the caller is normalized observation JSONL. Raw child stdout is still preserved in run artifacts for debugging and replay.

## Target Path

```text
caller
-> @ahandai/sdk CloudClient.spawn(...)
-> ahand-hub /api/control/jobs
-> hub WebSocket gateway
-> ahandd
-> child process with stdin/stdout/stderr pipes
-> stdout/stderr chunks back to SDK callbacks or SSE
```

## Output Contract

AHand uses stdout as the single user-facing data source. The long-term `--output-format` option selects the stdout schema:

| Output format | Caller-facing stdout |
|---|---|
| `raw` | Child process stdout bytes, unchanged. |
| `codex-jsonl` | Codex stdout parsed and formatted as one `AgentObservationRecord` JSON object per line. |
| `claude-stream-json` | Claude Code stdout parsed and formatted as observation JSONL. |
| `hermes-acp-json-rpc` | Hermes ACP stdout parsed and formatted as observation JSONL. |

The old `--format` flag is deprecated. Current implementations may still accept it as a compatibility alias, but new docs and APIs should use `--output-format` / `outputFormat`.

Run artifacts such as `runs/<job_id>/stdout` and `observations.jsonl` are debug and replay artifacts. They are not a second user query path. SDK callbacks, `ahandctl`, and hub SSE should all expose the same selected stdout schema.

Use `executionMode: "pipe_stream"` only when the target hub and daemon support it. The compatibility field `interactive=false` is still sent, but it is not enough to express stream semantics by itself.

## Format Boundary

`executionMode: "pipe_stream"` does not mean Codex, Claude, or ACP. It only means AHand uses non-PTY process pipes.

Use `inputFormat` and `outputFormat` for agent protocol differences:

| Agent | `inputFormat` | `outputFormat` |
|---|---|---|
| Raw process | `raw` | `raw` |
| Codex | `text` | `codex-jsonl` |
| Claude Code | `claude-stream-json` | `claude-stream-json` |
| Hermes ACP | `hermes-acp-json-rpc` | `hermes-acp-json-rpc` |

## Single-Turn Agent Command Shape

For low-level debugging, Codex can still be launched as an ordinary CLI command that AHand runs once per job.

### Codex

Codex is naturally single-turn. Pass `-` so Codex reads the prompt from stdin:

```bash
printf 'Run tests and explain failures\n' | cargo run -p ahandctl -- \
  --ipc /tmp/ahand-local-debug.sock \
  exec \
  --execution-mode pipe_stream \
  --input-format text \
  --result-parser codex-jsonl \
  --output-format codex-jsonl \
  --cwd "$PWD" \
  codex -- exec --skip-git-repo-check --json --cd "$PWD" -
```

`--input-format text` writes the prompt as plain text to Codex stdin. `--output-format codex-jsonl` decodes Codex JSONL stdout, maps decoded Codex events into AHand's normalized observation dimensions, and sends those observation records on stdout.

Resume an existing Codex thread the same way:

```bash
printf 'Continue from the previous result\n' | cargo run -p ahandctl -- \
  --ipc /tmp/ahand-local-debug.sock \
  exec \
  --execution-mode pipe_stream \
  --input-format text \
  --result-parser codex-jsonl \
  --output-format codex-jsonl \
  --cwd "$PWD" \
  codex -- exec resume --skip-git-repo-check <thread_id> --json -
```

### Claude Code

Prefer the dedicated Claude Code adapter path in `docs/usage/claude-code.md`. The older low-level command form is only useful for raw process debugging:

```bash
cargo run -p ahandctl -- \
  --ipc /tmp/ahand-local-debug.sock \
  exec \
  --execution-mode pipe_stream \
  --result-parser claude-stream-json \
  --cwd "$PWD" \
  claude -- -p "Review this repo" --output-format stream-json
```

The supported Claude Code integration does not share Codex's stdin text format. AHand writes Claude's native stream-json user message internally and then exposes normalized observation JSONL to callers.

## Quick Run Order

For a local development run, the full order is:

```text
1. pnpm install
2. Start Postgres and Redis
3. Start ahand-hub on http://127.0.0.1:8080
4. Register or claim a device for externalUserId=user-123
5. Start ahandd and connect it to ws://127.0.0.1:8080/ws
6. Mint a control-plane token for user-123
7. Call CloudClient.spawn({ executionMode: "pipe_stream" })
8. Observe stdout/stderr through SDK callbacks or /api/control/jobs/{id}/stream
```

Step 4 can be done manually with `ahandctl identity show`, or automated by the product/pairing flow.

## Current Debug Status

`pipe_stream` is a mainline AHand feature, not a sidecar-only experiment. The current debug path is the normal control-plane path:

```text
SDK / HTTP
-> ahand-hub control-plane
-> daemon WebSocket
-> ahandd pipe_stream runtime
-> stdout/stderr callbacks or SSE
```

Local sidecar debugging is also available for daemon-only development. It does not start `ahand-hub`, PostgreSQL, Redis, dashboard, device registration, or control-plane JWT:

```bash
cargo run -p ahandd -- \
  --mode local \
  --debug-ipc \
  --ipc-socket /tmp/ahand-local-debug.sock \
  --data-dir /tmp/ahand-local-debug-data
```

In another terminal:

```bash
cargo run -p ahandctl -- \
  --ipc /tmp/ahand-local-debug.sock \
  exec \
  --execution-mode pipe_stream \
  --cwd "$PWD" \
  --env AHAND_LOCAL_DEBUG=1 \
  sh -- -c 'printf "stdout:%s\n" "$AHAND_LOCAL_DEBUG"; printf "stderr\n" >&2'
```

Expected result:

```text
stdout:1
stderr
[finished] exit_code=0
```

Use the same single-turn launch shape for Claude Code or Codex:

```bash
cargo run -p ahandctl -- \
  --ipc /tmp/ahand-local-debug.sock \
  exec --execution-mode pipe_stream --cwd "$PWD" \
  --result-parser claude-stream-json \
  claude -- -p "Review this repo" --output-format stream-json

printf 'Run tests and explain failures\n' | \
cargo run -p ahandctl -- \
  --ipc /tmp/ahand-local-debug.sock \
  exec --execution-mode pipe_stream --cwd "$PWD" \
  --result-parser codex-jsonl \
  --output-format codex-jsonl \
  codex -- exec --skip-git-repo-check --json --cd "$PWD" -
```

The `--` after the tool is required when tool arguments begin with `-`, for example `sh -- -c ...` or `claude -- -p ...`.

What is currently verified:

- SDK serializes `executionMode: "pipe_stream"` and sends compatibility `interactive: false`.
- hub control-plane resolves `executionMode: "pipe_stream"` to protobuf `ExecutionMode::PipeStream`.
- daemon has a `pipe_stream` runtime using child `stdin/stdout/stderr` pipes.
- local sidecar mode can run IPC-injected `pipe_stream` jobs without hub/control-plane.
- single-turn CLI launch is enough to start Codex and Claude Code through AHand.
- Codex JSONL parsing and `outputFormat=codex-jsonl` observation output are implemented. With `outputFormat=codex-jsonl`, live stdout contains observation JSONL instead of raw Codex JSONL.
- Claude Code `stream-json` parsing is implemented through `inputFormat=claude-stream-json` and `outputFormat=claude-stream-json`. See `docs/usage/claude-code.md` for the supported path.
- control-plane integration test for actual daemon WebSocket dispatch compiles; in restricted sandboxes it may not run because it needs to bind a local test port.

## Local Sidecar Debug Usage

Use this path when you only want to debug one AHand execution locally, without hub, dashboard, database, device registration, or control-plane token.

Start local sidecar:

```bash
export AHAND_IPC=/tmp/ahand-local-debug.sock
export AHAND_DATA=/tmp/ahand-local-debug-data

cargo run -p ahandd -- \
  --mode local \
  --debug-ipc \
  --ipc-socket "$AHAND_IPC" \
  --data-dir "$AHAND_DATA"
```

In another terminal, run a raw pipe-stream smoke test:

```bash
cargo run -p ahandctl -- \
  --ipc "$AHAND_IPC" \
  exec \
  --execution-mode pipe_stream \
  --result-parser raw \
  --cwd "$PWD" \
  sh -- -c 'printf "stdout\n"; printf "stderr\n" >&2'
```

Run Codex as a single-turn CLI:

```bash
printf 'Run tests and explain failures\n' | cargo run -p ahandctl -- \
  --ipc "$AHAND_IPC" \
  exec \
  --execution-mode pipe_stream \
  --result-parser codex-jsonl \
  --output-format codex-jsonl \
  --cwd "$PWD" \
  codex -- exec --skip-git-repo-check --json --cd "$PWD" -
```

If `codex` is not visible to the daemon's `PATH`, use an absolute path and pass the daemon environment explicitly:

```bash
CODEX=$(command -v codex)

printf 'Run tests and explain failures\n' | cargo run -p ahandctl -- \
  --ipc "$AHAND_IPC" \
  exec \
  --execution-mode pipe_stream \
  --result-parser codex-jsonl \
  --output-format codex-jsonl \
  --cwd "$PWD" \
  --env PATH="$PATH" \
  "$CODEX" -- exec --skip-git-repo-check --json --cd "$PWD" -
```

Run Claude Code as a single-turn CLI:

```bash
cargo run -p ahandctl -- \
  --ipc "$AHAND_IPC" \
  exec \
  --execution-mode pipe_stream \
  --result-parser claude-stream-json \
  --cwd "$PWD" \
  claude -- -p "Review this repo" --output-format stream-json
```

Inspect local run artifacts:

```bash
find "$AHAND_DATA/runs" -maxdepth 2 -type f | sort
```

Each run directory contains:

```text
$AHAND_DATA/runs/<job_id>/
  request.json
  parser.json
  observations.jsonl
  stdout
  stderr
  result.json
```

`request.json` records the submitted `JobRequest`, including `execution_mode`, `result_parser`, and `outputFormat`.

`parser.json` records parser and formatter configuration:

```json
{
  "job_id": "ctl-job-...",
  "parser": "codex-jsonl",
  "parser_version": 1,
  "outputFormat": "codex-jsonl",
  "status": "configured",
  "parse_errors": 0,
  "start_ms": 1778635825119
}
```

`stdout` and `stderr` files in the run directory are debug artifacts and remain raw child-process output.

With `--output-format codex-jsonl`, live stdout from `ahandctl`, SDK callbacks, or hub SSE contains the same normalized observation records that are written to `observations.jsonl`. This is intentional: the explicit formatter switch changes the caller-facing stdout format, while preserving raw process stdout on disk.

`observations.jsonl` is a debug copy of the formatted stdout when a formatter is enabled, for example `--output-format codex-jsonl`. It is useful for local inspection and replay, but the user-facing data source is still stdout. Each line is a normalized observation record. Current Codex records include:

```text
agent_session
llm_call_start
llm_call_delta
llm_call_end
tool_call_start
tool_call_output
tool_call_end
error
raw
parse_error
```

Quick inspection:

```bash
LATEST=$(ls -td "$AHAND_DATA/runs"/* | head -1)

cat "$LATEST/parser.json"
sed -n '1,80p' "$LATEST/stdout"
sed -n '1,80p' "$LATEST/observations.jsonl"
cat "$LATEST/result.json"
```

For normal consumers, prefer reading the stdout stream produced by `ahandctl`, SDK `onStdout`, or hub SSE. Read artifact files only when debugging or replaying a run.

## Prerequisites

On the machine running `ahandd`:

- `ahandd` and `ahandctl` are installed.
- Claude Code and/or Codex CLI are installed and already authenticated for the same OS user that runs `ahandd`.
- `claude` and/or `codex` are discoverable through `PATH`, or callers pass an absolute executable path as `tool`.
- The daemon session mode allows jobs, for example `trust` or `auto_accept`.

On the hub side:

- `ahand-hub` is running.
- PostgreSQL and Redis are configured for persistent hub state and output replay.
- The device is registered to an `externalUserId`.
- The caller can obtain a control-plane JWT with scope `jobs:execute`.

## 1. Start the Hub

Install repo dependencies if this is a fresh checkout:

```bash
pnpm install
```

For local development, start PostgreSQL and Redis first:

```bash
docker network create ahand-dev 2>/dev/null || true

docker run -d --rm --network ahand-dev --name ahand-dev-postgres \
  -p 5432:5432 \
  -e POSTGRES_DB=ahand_hub \
  -e POSTGRES_USER=ahand_hub \
  -e POSTGRES_PASSWORD=ahand_hub \
  postgres:16-alpine

docker run -d --rm --network ahand-dev --name ahand-dev-redis \
  -p 6379:6379 \
  redis:7-alpine
```

Wait until both dependencies are ready:

```bash
until docker exec ahand-dev-postgres pg_isready -U ahand_hub -d ahand_hub; do sleep 1; done
until docker exec ahand-dev-redis redis-cli ping; do sleep 1; done
```

Then start `ahand-hub` from this repository:

```bash
export AHAND_HUB_BIND_ADDR=0.0.0.0:8080
export AHAND_HUB_SERVICE_TOKEN=dev-service-token
export AHAND_HUB_DASHBOARD_PASSWORD=dev-dashboard-password
export AHAND_HUB_DEVICE_BOOTSTRAP_TOKEN=dev-bootstrap-token
export AHAND_HUB_DEVICE_BOOTSTRAP_DEVICE_ID=device-dev-1
export AHAND_HUB_JWT_SECRET=dev-jwt-secret
export AHAND_HUB_DATABASE_URL=postgres://ahand_hub:ahand_hub@127.0.0.1:5432/ahand_hub
export AHAND_HUB_REDIS_URL=redis://127.0.0.1:6379
export AHAND_HUB_OUTPUT_RETENTION_MS=3600000

cargo run -p ahand-hub
```

Verify the hub:

```bash
export HUB_URL=http://127.0.0.1:8080

curl -fsS "$HUB_URL/api/health"
```

Production should use the same environment contract through `deploy/hub`. The checked-in compose file starts `ahand-hub` and `ahand-hub-dashboard`, but PostgreSQL and Redis are still external dependencies:

```bash
export AHAND_HUB_SERVICE_TOKEN=dev-service-token
export AHAND_HUB_DASHBOARD_PASSWORD=dev-dashboard-password
export AHAND_HUB_DEVICE_BOOTSTRAP_TOKEN=dev-bootstrap-token
export AHAND_HUB_DEVICE_BOOTSTRAP_DEVICE_ID=device-dev-1
export AHAND_HUB_JWT_SECRET=dev-jwt-secret
export AHAND_HUB_DATABASE_URL=postgres://ahand_hub:ahand_hub@host.docker.internal:5432/ahand_hub
export AHAND_HUB_REDIS_URL=redis://host.docker.internal:6379

docker compose -f deploy/hub/docker-compose.yml up --build
```

## 2. Register or Claim the Device

The control-plane API authorizes by `externalUserId`, so the device row must be associated with the same external user used by the control-plane JWT.

Recommended production flow:

1. The product backend creates or loads the device identity.
2. The backend pre-registers the device with `externalUserId`.
3. The daemon connects with its signed Ed25519 hello.
4. The backend mints a control-plane token for that same `externalUserId`.

Admin API shape:

First export the daemon identity:

```bash
IDENTITY_JSON=$(ahandctl identity show)
DEVICE_ID=$(echo "$IDENTITY_JSON" | jq -r .deviceId)
PUBLIC_KEY=$(echo "$IDENTITY_JSON" | jq -r .publicKey)
echo "$IDENTITY_JSON" | jq
```

If the daemon uses a non-default config or identity path:

```bash
ahandctl identity show --config "$HOME/.ahand/config.toml"
ahandctl identity show --identity-path "$HOME/.ahand/hub-device-identity.json"
```

The command reads or creates the same identity file that `ahandd` uses for hub authentication. Its output is:

```json
{
  "deviceId": "sha256-public-key-hex",
  "publicKey": "base64-ed25519-public-key",
  "identityPath": "/home/me/.ahand/hub-device-identity.json"
}
```

Then pre-register the device:

```bash
curl -sS -X POST "$HUB_URL/api/admin/devices" \
  -H "Authorization: Bearer $AHAND_HUB_SERVICE_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{
    \"deviceId\": \"$DEVICE_ID\",
    \"publicKey\": \"$PUBLIC_KEY\",
    \"externalUserId\": \"user-123\"
  }"
```

Then mint a control-plane JWT:

```bash
CONTROL_TOKEN=$(
  curl -sS -X POST "$HUB_URL/api/admin/control-plane/token" \
    -H "Authorization: Bearer $AHAND_HUB_SERVICE_TOKEN" \
    -H "Content-Type: application/json" \
    -d "{
      \"externalUserId\": \"user-123\",
      \"deviceIds\": [\"$DEVICE_ID\"],
      \"scope\": \"jobs:execute\"
    }" | jq -r .token
)
```

For local-only smoke testing of hub/daemon connectivity, the fixed bootstrap device can connect without pre-registration:

```text
deviceId: device-dev-1
bootstrap token: dev-bootstrap-token
```

That proves the hub and daemon are connected, but it is not enough for SDK control-plane ownership checks unless the device is also associated with the caller's `externalUserId`.

## 3. Configure `ahandd`

Write `~/.ahand/config.toml` on the device:

```toml
server_url = "ws://127.0.0.1:8080/ws"
device_id = "<DEVICE_ID from ahandctl identity show>"
default_session_mode = "trust"
max_concurrent_jobs = 4

[hub]
# Optional. If omitted, ahandd uses its default identity path under ~/.ahand.
# Use an absolute path here; the daemon does not expand "~" inside TOML values.
private_key_path = "/home/me/.ahand/hub-device-identity.json"

[policy]
allowed_tools = ["claude", "codex"]
denied_paths = []
denied_tools = []
approval_timeout_secs = 86400
```

For local testing with the fixed bootstrap device from the hub env above, use:

```toml
server_url = "ws://127.0.0.1:8080/ws"
device_id = "device-dev-1"
default_session_mode = "trust"
max_concurrent_jobs = 4

[hub]
bootstrap_token = "dev-bootstrap-token"

[policy]
allowed_tools = ["claude", "codex"]
denied_paths = []
denied_tools = []
approval_timeout_secs = 86400
```

If the device is connecting for first-time bootstrap instead of pre-registration, include the bootstrap token:

```toml
[hub]
bootstrap_token = "dev-bootstrap-token"
private_key_path = "/home/me/.ahand/hub-device-identity.json"
```

Start the daemon:

```bash
ahandctl start
ahandctl status
```

During development you can also run it in the foreground:

```bash
cargo run -p ahandd -- --config "$HOME/.ahand/config.toml"
```

You should see the daemon connect to the hub. If running via `ahandctl`, check:

```bash
tail -f "$HOME/.ahand/data/daemon.log"
```

Logs are written to:

```text
~/.ahand/data/daemon.log
```

## 4. Verify CLI Availability on the Device

Run these as the same user that starts `ahandd`:

```bash
which claude
claude --version

which codex
codex --version
```

If a tool is installed through shell startup files, prefer running through a login shell or use an absolute path. For direct execution, `tool` must be resolvable by the daemon process environment.

Verify the hub sees the device as online:

```bash
curl -sS "$HUB_URL/api/admin/devices?externalUserId=user-123" \
  -H "Authorization: Bearer $AHAND_HUB_SERVICE_TOKEN" | jq
```

For the fixed local bootstrap device (`device-dev-1`), it may not be associated with an `externalUserId`. For SDK/control-plane testing, prefer the pre-registered device flow in step 2 so ownership checks pass.

## 5. Call Claude Code With SDK

Set the SDK environment:

```bash
export AHAND_HUB_URL=http://127.0.0.1:8080
export AHAND_CONTROL_TOKEN="$CONTROL_TOKEN"
```

Create a local script:

```bash
mkdir -p /tmp/ahand-pipe-stream-demo
cd /tmp/ahand-pipe-stream-demo
pnpm init
pnpm add @ahandai/sdk
```

```ts
// run-claude.ts
import { CloudClient } from "@ahandai/sdk";

const client = new CloudClient({
  hubUrl: process.env.AHAND_HUB_URL!,
  getAuthToken: async () => process.env.AHAND_CONTROL_TOKEN!,
});

const result = await client.spawn({
  deviceId: "device-123",
  tool: "claude",
  args: [
    "-p",
    "Inspect this repository and summarize failing tests.",
    "--output-format",
    "stream-json",
  ],
  cwd: "/home/me/workspace/project",
  timeoutMs: 30 * 60 * 1000,
  executionMode: "pipe_stream",
  onStdout: (chunk) => process.stdout.write(chunk),
  onStderr: (chunk) => process.stderr.write(chunk),
});

console.log("exit", result.exitCode);
```

Run it with your TypeScript runner of choice, or convert the snippet to plain JS for Node.

For Claude Code, prefer CLI arguments that produce machine-readable streaming output when the deployment supports them. Avoid full-screen interactive UI flags in `pipe_stream`; those belong in `pty`.

## 6. Call Codex With SDK

```ts
// run-codex.ts
import { CloudClient } from "@ahandai/sdk";

const client = new CloudClient({
  hubUrl: process.env.AHAND_HUB_URL!,
  getAuthToken: async () => process.env.AHAND_CONTROL_TOKEN!,
});

const result = await client.spawn({
  deviceId: "device-123",
  tool: "codex",
  args: ["exec", "Run tests and explain failures."],
  cwd: "/home/me/workspace/project",
  timeoutMs: 30 * 60 * 1000,
  executionMode: "pipe_stream",
  onStdout: (chunk) => process.stdout.write(chunk),
  onStderr: (chunk) => process.stderr.write(chunk),
});

console.log("exit", result.exitCode);
```

## 7. Call the HTTP API Directly

Create the job:

```bash
JOB_ID=$(
  curl -sS -X POST "$HUB_URL/api/control/jobs" \
    -H "Authorization: Bearer $CONTROL_TOKEN" \
    -H "Content-Type: application/json" \
    -d '{
      "deviceId": "device-123",
      "tool": "codex",
      "args": ["exec", "Run tests and explain failures."],
      "cwd": "/home/me/workspace/project",
      "timeoutMs": 1800000,
      "executionMode": "pipe_stream",
      "interactive": false
    }' | jq -r .jobId
)
```

Stream output:

```bash
curl -N "$HUB_URL/api/control/jobs/$JOB_ID/stream" \
  -H "Authorization: Bearer $CONTROL_TOKEN"
```

The stream emits stdout, stderr, finished, and error events.

## 8. Debug and Verify Pipe Stream

### Verify SDK Serialization

Run:

```bash
pnpm --filter @ahandai/sdk test
```

The SDK test suite should include a case that sends:

```json
{
  "executionMode": "pipe_stream",
  "interactive": false
}
```

This confirms SDK callers can request stream mode without using PTY.

### Verify Hub Mode Resolution

Run:

```bash
cargo test -p ahand-hub --lib http::control_plane::tests
```

Expected behavior:

- explicit `executionMode=pipe_stream` resolves to `ExecutionMode::PipeStream`
- explicit mode wins over the legacy `interactive` compatibility bool
- missing `executionMode` still falls back to old `interactive` behavior

### Verify Hub Dispatch Shape

Compile the integration test:

```bash
cargo check -p ahand-hub --test control_plane
```

There is an integration test that simulates a daemon WebSocket and asserts the dispatched `JobRequest` contains:

```text
execution_mode = PipeStream
interactive = false
```

Run the actual integration test on a normal development machine or CI runner that allows binding local ports:

```bash
cargo test -p ahand-hub --test control_plane create_job_pipe_stream_dispatches_explicit_execution_mode
```

In a restricted sandbox, this can fail before assertions with:

```text
Operation not permitted
```

at the test server bind step. That is an environment limitation, not a `pipe_stream` assertion failure.

### Verify Daemon Runtime Selection

When a `pipe_stream` job reaches `ahandd`, expected daemon behavior is:

- no PTY is allocated
- child stdin is piped
- child stdout and stderr are piped separately
- `StdinChunk` writes to child stdin
- terminal resize is ignored because there is no terminal

If local run storage is enabled, inspect:

```text
~/.ahand/data/runs/<job_id>/request.json
~/.ahand/data/runs/<job_id>/stdout
~/.ahand/data/runs/<job_id>/stderr
```

`request.json` should include `execution_mode` for the job.

### Verify End-to-End with HTTP

Use a command that behaves differently when it has separated stdout/stderr. For example:

```bash
JOB_ID=$(
  curl -sS -X POST "$HUB_URL/api/control/jobs" \
    -H "Authorization: Bearer $CONTROL_TOKEN" \
    -H "Content-Type: application/json" \
    -d "{
      \"deviceId\": \"$DEVICE_ID\",
      \"tool\": \"shell\",
      \"args\": [\"-lc\", \"printf out; printf err >&2\"],
      \"executionMode\": \"pipe_stream\",
      \"interactive\": false
    }" | jq -r .jobId
)

curl -N "$HUB_URL/api/control/jobs/$JOB_ID/stream" \
  -H "Authorization: Bearer $CONTROL_TOKEN"
```

Expected:

- stdout event contains `out`
- stderr event contains `err`
- finished event contains the exit code

### Current Non-Goals for Debugging

These are planned for the local sidecar work, not required for mainline `pipe_stream` validation:

- `ahandctl --ipc ... exec --execution-mode pipe_stream`
- `ahandctl attach <job_id>`
- `ahandctl stdin <job_id>`
- `ahandctl runs list/show/tail`

## 9. Sending Stdin

`pipe_stream` is designed for stdin/stdout/stderr pipes, and the daemon-side runtime can receive `StdinChunk` frames. The control-plane HTTP/SDK surface still needs a dedicated stdin endpoint before SDK callers can write to a running control-plane job after creation.

For the first Claude/Codex path, pass the prompt or instruction in CLI args:

```ts
await client.spawn({
  deviceId: "device-123",
  tool: "codex",
  args: ["exec", "Run tests and explain failures."],
  executionMode: "pipe_stream",
});
```

The follow-up API should be shaped like this:

```bash
printf 'continue\n' | curl -sS -X POST "$HUB_URL/api/control/jobs/$JOB_ID/stdin" \
  -H "Authorization: Bearer $CONTROL_TOKEN" \
  --data-binary @-
```

For SDK callers, add a typed helper before exposing this broadly, for example:

```ts
await client.writeStdin(jobId, Buffer.from("continue\n"));
```

That helper is not part of the current `CloudClient.spawn()` surface yet.

## 10. Observability

You should be able to observe the run at three layers:

| Layer | What to check |
|---|---|
| SDK / HTTP stream | `onStdout`, `onStderr`, `/api/control/jobs/{id}/stream` |
| hub output stream | output chunks retained in Redis for `AHAND_HUB_OUTPUT_RETENTION_MS` |
| daemon artifacts | run metadata and stdout/stderr files under `~/.ahand/data` |

Expected behavior for `pipe_stream`:

- No PTY is allocated.
- stdout and stderr stay separated.
- stdin bytes are written to child stdin.
- terminal resize messages are ignored or rejected because there is no terminal.
- `interactive=false` is sent only for backward compatibility.

## 11. Common Failures

`403` from `/api/control/jobs`:

- The control-plane JWT scope is not `jobs:execute`.
- The token `externalUserId` does not match the device owner.
- The token has a `deviceIds` allowlist that does not include the target device.

`404 DEVICE_OFFLINE`:

- `ahandd` is not connected to the hub.
- `server_url` points at the wrong hub.
- The daemon failed authentication.

`tool not found`:

- `claude` or `codex` is not on the daemon process `PATH`.
- Use an absolute executable path, or start the daemon with the same environment where the CLI is installed.

No live output:

- Confirm the job uses `executionMode: "pipe_stream"`.
- Confirm the target daemon includes pipe-stream support.
- Confirm the CLI command actually writes to stdout/stderr in non-TTY mode.

Unexpected TUI or ANSI screen redraw:

- The command is trying to run an interactive terminal UI. Use `pty` for true terminal UI, or switch the CLI to a non-interactive/headless mode.
