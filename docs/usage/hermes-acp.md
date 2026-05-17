# Run Hermes ACP Through AHand

This guide describes the current Hermes integration path in AHand. Hermes is run through its ACP mode:

```text
ahandd -> hermes acp stdin   JSON-RPC requests
ahandd <- hermes acp stdout  JSON-RPC responses and notifications
ahandd <- hermes acp stderr  raw diagnostics
```

Unlike Codex `--json`, Hermes ACP stdout is not user-facing output. It is protocol traffic. AHand acts as the ACP client, sends `initialize`, creates or resumes a session, sends the prompt, consumes `session/update` events, and exposes normalized observation JSONL as caller-facing stdout.

## Format Boundary

Hermes uses the same three stable knobs as Codex and Claude Code, but both stdin and stdout use Hermes ACP JSON-RPC:

```text
executionMode=pipe_stream
  process transport

inputFormat=hermes-acp-json-rpc
  spawn "hermes acp"
  initialize
  session/new or session/resume
  session/set_model
  session/prompt

outputFormat=hermes-acp-json-rpc
  ACP responses/notifications -> AgentObservationRecord JSONL
```

`executionMode: "pipe_stream"` remains the process transport. It is not `mode=acp`; ACP is selected by `inputFormat=hermes-acp-json-rpc` and `outputFormat=hermes-acp-json-rpc`.

## Current Contract

The current implementation uses `inputFormat` / `outputFormat` for routing and env fields for executable, prompt, and optional session metadata.

Required:

| Env | Meaning |
|---|---|
| `AHAND_INPUT_FORMAT=hermes-acp-json-rpc` | Selects Hermes ACP stdin handling. |
| `AHAND_OUTPUT_FORMAT=hermes-acp-json-rpc` | Selects Hermes ACP stdout parsing. |
| `AHAND_AGENT_EXECUTABLE=/path/to/hermes` | Explicit Hermes binary path. If omitted, `JobRequest.tool` is used. |
| `AHAND_AGENT_PROMPT=...` | Prompt sent via `session/prompt`. |

Optional:

| Env | Meaning |
|---|---|
| `AHAND_AGENT_MODEL=provider:model` | Sent in `session/new` and `session/set_model`. Model set failure fails the job. |
| `AHAND_AGENT_SESSION_ID=ses_...` | Uses `session/resume` instead of `session/new`. |
| `AHAND_AGENT_INSTRUCTIONS=...` | Writes one non-overwriting context file in `cwd`: `AGENTS.md` when absent, otherwise `AGENTS.ahand.md`. |

AHand intentionally does not discover Hermes from PATH. If you pass `hermes` instead of an absolute path, you must also pass a `PATH` env that makes it resolvable by the daemon process.

The stable shape should use `executionMode`, `inputFormat`, `outputFormat`, `executable`, `prompt`, `model`, `sessionId`, `cwd`, and `env`.
## Stdout Contract

Caller-facing stdout is one `AgentObservationRecord` JSON object per line.

Expected observation kinds include:

```text
status
agent_session
llm_call_delta
llm_call_end
tool_call_start
tool_call_output
tool_call_end
error
parse_error
raw
```

Raw ACP frames are debug artifacts, not the caller-facing stream.

## Local Sidecar Usage

Start a local daemon with IPC enabled:

```bash
export AHAND_IPC=/tmp/ahand-local-debug.sock
export AHAND_DATA=/tmp/ahand-local-debug-data

cargo run -p ahandd -- \
  --mode local \
  --debug-ipc \
  --ipc-socket "$AHAND_IPC" \
  --data-dir "$AHAND_DATA"
```

In another terminal, run Hermes ACP:

```bash
HERMES="$(command -v hermes)"

cargo run -p ahandctl -- \
  --ipc "$AHAND_IPC" \
  hermes "$HERMES" \
  --cwd "$PWD" \
  --timeout-ms 1800000 \
  --prompt "Inspect this repository and summarize the test strategy." \
  --env PATH="$PATH" \
  --env HOME="$HOME"
```

The lower-level `exec` equivalent is:

```bash
cargo run -p ahandctl -- \
  --ipc "$AHAND_IPC" \
  exec \
  --execution-mode pipe_stream \
  --input-format hermes-acp-json-rpc \
  --output-format hermes-acp-json-rpc \
  --cwd "$PWD" \
  --timeout-ms 1800000 \
  --env AHAND_AGENT_EXECUTABLE="$HERMES" \
  --env AHAND_AGENT_PROMPT="Inspect this repository and summarize the test strategy." \
  --env PATH="$PATH" \
  --env HOME="$HOME" \
  "$HERMES"
```

Notes:

- `execution-mode pipe_stream` is the correct transport shape, while `input-format` / `output-format` select ACP JSON-RPC handling.
- Do not send ACP frames from the caller. AHand owns the ACP request sequence.
- `ahandctl` will print observation JSONL to stdout.
- The final line from `ahandctl` still reports `[finished] exit_code=...`.

## Resume a Hermes Session

If a previous run emitted an `agent.agentSessionId`, pass it back:

```bash
cargo run -p ahandctl -- \
  --ipc "$AHAND_IPC" \
  exec \
  --execution-mode pipe_stream \
  --input-format hermes-acp-json-rpc \
  --output-format hermes-acp-json-rpc \
  --cwd "$PWD" \
  --timeout-ms 1800000 \
  --env AHAND_AGENT_EXECUTABLE="$HERMES" \
  --env AHAND_AGENT_SESSION_ID="ses_abc123" \
  --env AHAND_AGENT_PROMPT="Continue from the previous result and run the focused tests." \
  --env PATH="$PATH" \
  --env HOME="$HOME" \
  "$HERMES"
```

## Select a Model

Pass the model explicitly:

```bash
cargo run -p ahandctl -- \
  --ipc "$AHAND_IPC" \
  exec \
  --execution-mode pipe_stream \
  --input-format hermes-acp-json-rpc \
  --output-format hermes-acp-json-rpc \
  --cwd "$PWD" \
  --timeout-ms 1800000 \
  --env AHAND_AGENT_EXECUTABLE="$HERMES" \
  --env AHAND_AGENT_MODEL="provider:model" \
  --env AHAND_AGENT_PROMPT="Review the latest diff." \
  --env PATH="$PATH" \
  --env HOME="$HOME" \
  "$HERMES"
```

AHand sends the model during `session/new` and then calls `session/set_model`. If Hermes rejects `session/set_model`, the job fails instead of silently falling back.

## SDK Shape

Use `CloudClient.spawnAgent()` for Hermes ACP:

```ts
import { CloudClient } from "@ahandai/sdk";

const client = new CloudClient({
  hubUrl: process.env.AHAND_HUB_URL!,
  getAuthToken: async () => process.env.AHAND_CONTROL_TOKEN!,
});

await client.spawnAgent({
  deviceId: "device-123",
  executionMode: "pipe_stream",
  inputFormat: "hermes-acp-json-rpc",
  outputFormat: "hermes-acp-json-rpc",
  executable: "/absolute/path/to/hermes",
  cwd: "/home/me/project",
  prompt: "Run tests and explain failures.",
  model: "provider:model",
  timeoutMs: 30 * 60 * 1000,
  env: {
    PATH: process.env.PATH ?? "",
    HOME: process.env.HOME ?? "",
  },
  onObservation: (record) => console.log(record),
  onStderr: (chunk) => process.stderr.write(chunk),
});
```

The lower-level `spawn()` path still works when you need to pass the env contract directly:

```ts
await client.spawn({
  deviceId: "device-123",
  tool: "/absolute/path/to/hermes",
  cwd: "/home/me/project",
  timeoutMs: 30 * 60 * 1000,
  executionMode: "pipe_stream",
  inputFormat: "hermes-acp-json-rpc",
  outputFormat: "hermes-acp-json-rpc",
  env: {
    : "hermes-acp",
    AHAND_AGENT_EXECUTABLE: "/absolute/path/to/hermes",
    AHAND_AGENT_PROMPT: "Run tests and explain failures.",
    PATH: process.env.PATH ?? "",
    HOME: process.env.HOME ?? "",
  },
  onStdout: (chunk) => process.stdout.write(chunk),
  onStderr: (chunk) => process.stderr.write(chunk),
});
```

## Direct HTTP Shape

Create a job:

```bash
curl -sS -X POST "$HUB_URL/api/control/jobs" \
  -H "Authorization: Bearer $CONTROL_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{
    \"deviceId\": \"$DEVICE_ID\",
    \"tool\": \"$HERMES\",
    \"cwd\": \"$PWD\",
    \"timeoutMs\": 1800000,
    \"executionMode\": \"pipe_stream\",
    \"inputFormat\": \"hermes-acp-json-rpc\",
    \"outputFormat\": \"hermes-acp-json-rpc\",
    \"executable\": \"$HERMES\",
    \"prompt\": \"Inspect this repository.\",
    \"env\": {
      \"\": \"hermes-acp\",
      \"AHAND_AGENT_EXECUTABLE\": \"$HERMES\",
      \"AHAND_AGENT_PROMPT\": \"Inspect this repository.\",
      \"PATH\": \"$PATH\",
      \"HOME\": \"$HOME\"
    }
  }"
```

Then stream:

```bash
curl -N "$HUB_URL/api/control/jobs/$JOB_ID/stream" \
  -H "Authorization: Bearer $CONTROL_TOKEN"
```

## ACP Request Sequence

AHand sends these JSON-RPC requests to Hermes:

```text
initialize
session/new or session/resume
session/set_model       only when AHAND_AGENT_MODEL is set
session/prompt
```

`initialize` is the health check. If Hermes starts but does not complete `initialize`, the job fails.

`session/prompt` sends prompt blocks:

```json
[
  {
    "type": "text",
    "text": "..."
  }
]
```

## Hermes Notifications

AHand currently maps these Hermes ACP updates:

| Hermes update | AHand observation |
|---|---|
| `agent_message_chunk` | `llm_call_delta` |
| `agent_thought_chunk` | `llm_call_delta` with `channel=thinking` |
| `tool_call` | `tool_call_start` |
| `tool_call_update` | `tool_call_output` or `tool_call_end` |
| `usage_update` | `llm_call_end` usage snapshot |
| `turn_end` / `end_turn` | `llm_call_end` |

Supported update shapes:

```text
update.sessionUpdate = "agent_message_chunk"
update.type = "AgentMessageChunk"
update = { "agentMessageChunk": { ... } }
```

## Permission Requests

Hermes may send daemon-bound JSON-RPC requests. The current implementation handles:

```text
session/request_permission
```

It emits `permission_request` and `policy_decision` observations, then replies with:

```json
{
  "outcome": {
    "outcome": "selected",
    "optionId": "approve_for_session"
  }
}
```

Unknown Hermes -> AHand methods receive JSON-RPC `-32601 method not found`.

This is an early integration behavior. Do not treat it as a final policy model. AHand still needs a stricter policy/approval story for Hermes tool execution before broad remote use.

## Provider Stderr Errors

Hermes stderr is still streamed as raw stderr and saved in `stderr`. AHand also promotes common provider failures into structured error observations and a failed job result:

```text
provider_rate_limited
provider_quota_exceeded
provider_auth_failed
provider_error
```

## Run Artifacts

When `--data-dir` is enabled, inspect:

```text
$AHAND_DATA/runs/<job_id>/
  request.json
  stdout
  stderr
  observations.jsonl
  hermes-session.json
  acp-requests.jsonl
  acp-events.jsonl
  result.json
```

Meaning:

| File | Meaning |
|---|---|
| `stdout` | Caller-facing observation JSONL. |
| `stderr` | Raw Hermes stderr diagnostics. |
| `observations.jsonl` | Debug copy of normalized observations. |
| `hermes-session.json` | Captured Hermes session id/model/raw session response. |
| `acp-requests.jsonl` | AHand -> Hermes JSON-RPC requests and replies to Hermes requests. |
| `acp-events.jsonl` | Hermes -> AHand JSON-RPC responses and notifications. |
| `context.jsonl` | Context files written by `AHAND_AGENT_INSTRUCTIONS`, when used. |

## Current Limitations

- The stable typed API is not added yet; use env fields for now.
- Context injection currently supports a single non-overwriting `AGENTS.md` or `AGENTS.ahand.md` file. `.agent_context/skills/` is not implemented yet.
- The permission response currently auto-approves `session/request_permission`; tighten this before production exposure.
- The implementation assumes Hermes ACP uses newline-delimited JSON-RPC, as documented in `docs/HERMES_DATA_EXCHANGE.md`.

## Troubleshooting

`failed to spawn Hermes ACP`:

- `AHAND_AGENT_EXECUTABLE` is wrong or not executable.
- `cwd` does not exist.
- If using `tool: "hermes"`, the daemon `PATH` cannot resolve it.

`hermes-acp requires AHAND_AGENT_PROMPT`:

- Pass `--env AHAND_AGENT_PROMPT="..."`.

No observation output:

- Confirm `inputFormat=hermes-acp-json-rpc` and `outputFormat=hermes-acp-json-rpc`.
- Confirm Hermes completed `initialize`.
- Inspect `acp-events.jsonl` for malformed or unexpected frames.

Model selection fails:

- Check `AHAND_AGENT_MODEL`.
- Hermes may require a provider prefix such as `provider:model`.
