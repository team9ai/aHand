# Hermes ACP Integration Status

**Last updated:** 2026-05-17  
**Document type:** long-lived status reference, not an implementation plan

This document records the current AHand integration state for Hermes ACP. It should be updated whenever the Hermes ACP data contract, formatter behavior, or production readiness changes.

## Summary

Hermes is currently integrated as an explicit daemon-side ACP stdio format.

AHand does not discover Hermes, register Hermes as a runtime, or treat Hermes as a plain stdout parser. A caller explicitly supplies the Hermes executable path, environment, cwd, and prompt. AHand then starts:

```text
{hermes_path} acp
```

and communicates with the process over newline-delimited JSON-RPC on stdin/stdout.

The current supported shape is job-scoped ACP launch:

```text
caller
-> AHand job with inputFormat=hermes-acp-json-rpc and outputFormat=hermes-acp-json-rpc
-> ahandd Hermes ACP stdio runner
-> hermes acp
-> JSON-RPC initialize/session/prompt
-> Hermes session/update events
-> AHand observation JSONL
```

## Current Status

| Area | Status |
|---|---|
| Explicit Hermes format selection | Implemented via `inputFormat=hermes-acp-json-rpc` and `outputFormat=hermes-acp-json-rpc` |
| Explicit Hermes executable | Implemented via `AHAND_AGENT_EXECUTABLE` or `JobRequest.tool` |
| Prompt input | Implemented via `AHAND_AGENT_PROMPT` |
| Model input | Implemented via `AHAND_AGENT_MODEL` |
| Session resume input | Implemented via `AHAND_AGENT_SESSION_ID` |
| Context file injection | Implemented for non-overwriting `AGENTS.md` / `AGENTS.ahand.md` via `AHAND_AGENT_INSTRUCTIONS` |
| JSONL JSON-RPC framing | Implemented |
| `initialize` ready check | Implemented |
| `session/new` / `session/resume` | Implemented |
| `session/set_model` | Implemented when model is provided |
| `session/prompt` | Implemented |
| `session/update` / `session/notification` formatter | Implemented for main update types |
| Hermes permission request reply | Implemented for `session/request_permission` with session approval |
| Permission/policy observations | Implemented for Hermes permission requests and unknown reverse RPC methods |
| Unknown Hermes request reply | Implemented with JSON-RPC `-32601` |
| Provider stderr error promotion | Implemented for common provider auth/quota/rate-limit diagnostics |
| Run artifacts for ACP frames | Implemented |
| SDK typed Hermes helper | Implemented as `CloudClient.spawnAgent()` |
| Hub typed Hermes fields | Implemented on `/api/control/jobs`, converted to daemon env contract |
| Proto typed Hermes fields | Not implemented |
| `ahandctl hermes` command | Implemented |
| Production-grade policy integration | Not implemented |

## Execution Contract

Hermes jobs currently use existing `JobRequest.env` fields.

Required:

```text
AHAND_INPUT_FORMAT=hermes-acp-json-rpc
AHAND_OUTPUT_FORMAT=hermes-acp-json-rpc
AHAND_AGENT_EXECUTABLE=/absolute/path/to/hermes
AHAND_AGENT_PROMPT=<prompt>
```

Optional:

```text
AHAND_AGENT_MODEL=provider:model
AHAND_AGENT_SESSION_ID=ses_...
AHAND_AGENT_INSTRUCTIONS=<context text>
```

`executionMode` should still be `pipe_stream` because ACP is a stdio protocol. ACP is not a separate process attach mode.

Example local IPC call:

```bash
HERMES="$(command -v hermes)"

cargo run -p ahandctl -- \
  --ipc /tmp/ahand-local-debug.sock \
  hermes "$HERMES" \
  --cwd "$PWD" \
  --timeout-ms 1800000 \
  --prompt "Inspect this repository and summarize the test strategy." \
  --env PATH="$PATH" \
  --env HOME="$HOME"
```

## ACP Request Sequence

AHand currently sends:

```text
initialize
session/new or session/resume
session/set_model       only when AHAND_AGENT_MODEL is set
session/prompt
```

`initialize` is the ready check. If Hermes starts but does not complete `initialize`, the job fails.

The protocol uses one JSON-RPC message per line:

```text
{"jsonrpc":"2.0",...}\n
```

## Formatter Contract

Caller-facing stdout is AHand observation JSONL. Raw ACP stdout is not exposed as the user-facing output stream.

The current Hermes formatter maps:

| Hermes ACP update | AHand observation |
|---|---|
| `agent_message_chunk` | `llm_call_delta` |
| `agent_thought_chunk` | `llm_call_delta` with `channel=thinking` |
| `tool_call` | `tool_call_start` |
| `tool_call_update` | `tool_call_output` or `tool_call_end` |
| `usage_update` | `llm_call_end` usage snapshot |
| `permission_request` | `permission_request` |
| `policy_decision` | `policy_decision` |
| `turn_end` / `end_turn` | `llm_call_end` |
| unknown update | `raw` |
| malformed JSONL line | `parse_error` |

Supported Hermes update shapes:

```text
update.sessionUpdate = "agent_message_chunk"
update.type = "AgentMessageChunk"
update = { "agentMessageChunk": { ... } }
```

Tool name inference currently uses the Hermes `title` field first, for example:

```text
terminal: ls -la -> terminal
execute code      -> execute_code
```

## Permission Requests

Hermes can send JSON-RPC requests to AHand. The current implementation handles:

```text
session/request_permission
```

The current response is:

```json
{
  "outcome": {
    "outcome": "selected",
    "optionId": "approve_for_session"
  }
}
```

This is an early integration behavior and is not the final security model. Before broad remote use, Hermes permission handling must be integrated with AHand policy, approval, and audit semantics.

The current backend records both the inbound permission request and the selected session approval as observation/audit-style records in the job stream.

## Provider Errors

Raw Hermes stderr is preserved. Common provider failures are also promoted into structured `error` observations and fail the job:

```text
provider_rate_limited
provider_quota_exceeded
provider_auth_failed
provider_error
```

## Run Artifacts

When `ahandd` is started with a data directory, Hermes jobs write artifacts under:

```text
runs/<job_id>/
  request.json
  parser.json
  stdout
  stderr
  observations.jsonl
  hermes-session.json
  acp-requests.jsonl
  acp-events.jsonl
  context.jsonl
  result.json
```

Meaning:

| File | Meaning |
|---|---|
| `stdout` | Caller-facing AHand observation JSONL |
| `stderr` | Raw Hermes stderr diagnostics |
| `observations.jsonl` | Debug/replay copy of normalized observations |
| `hermes-session.json` | Captured Hermes session id/model/raw session response |
| `acp-requests.jsonl` | AHand -> Hermes requests and responses to Hermes requests |
| `acp-events.jsonl` | Hermes -> AHand responses and notifications |
| `context.jsonl` | Context files written by `AHAND_AGENT_INSTRUCTIONS`, when used |

## Known Limitations

- Protobuf now has `input_format` and `output_format`; executable, prompt, model, and session metadata are still bridged through typed HTTP/SDK fields and daemon env.
- Context injection currently supports a single non-overwriting `AGENTS.md` or `AGENTS.ahand.md` file. `.agent_context/skills/` is not implemented yet.
- Permission handling currently auto-approves `session/request_permission`.
- The backend is job-scoped; it does not maintain a long-lived Hermes process pool.
- The implementation assumes Hermes ACP uses newline-delimited JSON-RPC, as documented in `docs/HERMES_DATA_EXCHANGE.md`.

## Related Documents

- `docs/usage/hermes-acp.md`
- `docs/HERMES_DATA_EXCHANGE.md`
- `docs/HERMES_INTEGRATION.md`
- `docs/plans/2026-05-16-hermes-acp-integration.md`
- `docs/plans/2026-05-13-agent-formatter-observation-dimensions.md`
