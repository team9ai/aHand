# Run Claude Code Through AHand

This guide describes the current Claude Code integration path in AHand. Claude Code runs through its `stream-json` mode:

```text
ahandd -> claude stdin   one JSON user message
ahandd <- claude stdout  stream-json events
ahandd <- claude stderr  raw diagnostics
```

AHand starts Claude Code with fixed protocol arguments, writes the prompt as a JSON user message, closes stdin, consumes stdout events, and exposes normalized observation JSONL as caller-facing stdout.

## Format Boundary

Claude Code uses the same three stable knobs as Codex and Hermes:

```text
executionMode=pipe_stream
  process transport

inputFormat=claude-stream-json
  spawn claude -p --output-format stream-json --input-format stream-json
  write one {"type":"user","message":...} JSONL line to stdin
  close stdin

outputFormat=claude-stream-json
  stream-json stdout events -> AgentObservationRecord JSONL
```

`executionMode: "pipe_stream"` remains the process transport. It does not mean Claude, Codex, or ACP; Claude Code is selected by `inputFormat=claude-stream-json` and `outputFormat=claude-stream-json`.

## Current Contract

Required:

| Env | Meaning |
|---|---|
| `AHAND_INPUT_FORMAT=claude-stream-json` | Selects Claude Code stream-json stdin handling. |
| `AHAND_OUTPUT_FORMAT=claude-stream-json` | Selects Claude Code stream-json stdout parsing. |
| `AHAND_AGENT_EXECUTABLE=/path/to/claude` | Explicit Claude binary path. If omitted, `JobRequest.tool` is used. |
| `AHAND_AGENT_PROMPT=...` | Prompt written to Claude stdin as `type=user`. |

Optional:

| Env | Meaning |
|---|---|
| `AHAND_AGENT_MODEL=...` | Sent as `--model`. |
| `AHAND_AGENT_SESSION_ID=...` | Sent as `--resume`. |
| `AHAND_AGENT_MAX_TURNS=...` | Sent as `--max-turns`. |
| `AHAND_AGENT_SYSTEM_PROMPT=...` | Sent as `--append-system-prompt`. |
| `AHAND_AGENT_PERMISSION_MODE=...` | Sent as `--permission-mode`. |
| `AHAND_AGENT_INSTRUCTIONS=...` | Writes one non-overwriting context file in `cwd`: `CLAUDE.md` when absent, otherwise `CLAUDE.ahand.md`. |

The stable shape should use `executionMode`, `inputFormat`, `outputFormat`, `executable`, `prompt`, `model`, `sessionId`, `maxTurns`, `systemPrompt`, `permissionMode`, `cwd`, and `env`.
## Local Usage

```bash
CLAUDE="$(command -v claude)"

cargo run -p ahandctl -- \
  --ipc "$AHAND_IPC" \
  claude-code "$CLAUDE" \
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
  --input-format claude-stream-json \
  --output-format claude-stream-json \
  --cwd "$PWD" \
  --timeout-ms 1800000 \
  --result-parser claude-stream-json \
  --env AHAND_AGENT_EXECUTABLE="$CLAUDE" \
  --env AHAND_AGENT_PROMPT="Inspect this repository." \
  --env PATH="$PATH" \
  --env HOME="$HOME" \
  "$CLAUDE"
```

Do not write Codex-style plain prompt text to Claude stdin when using `inputFormat=claude-stream-json`. AHand writes Claude's required stream-json user message internally.

## SDK Shape

```ts
await client.spawnAgent({
  deviceId: "device-123",
  inputFormat: "claude-stream-json",
  outputFormat: "claude-stream-json",
  executable: "/absolute/path/to/claude",
  cwd: "/repo",
  prompt: "Run tests and explain failures.",
  model: "claude-sonnet",
  sessionId: "claude-session-123",
  maxTurns: 4,
  systemPrompt: "You are running under AHand.",
  onObservation: (record) => console.log(record),
});
```

## Event Mapping

| Claude stream-json event | AHand observation |
|---|---|
| `system` | `agent_session`, `status` |
| `assistant.content[].text` | `llm_call_delta` |
| `assistant.content[].thinking` | `llm_call_delta` with `channel=thinking` |
| `assistant.content[].tool_use` | `tool_call_start` |
| `user.content[].tool_result` | `tool_call_output` |
| `assistant.message.usage` | `llm_call_end.usage` |
| `result` | final `llm_call_delta` / `llm_call_end`; error observation when `is_error=true` |
| `log` | `status` |
| malformed JSONL line | `parse_error` |
| unknown event | `raw` |

## Run Artifacts

When `--data-dir` is enabled:

```text
runs/<job_id>/
  request.json
  parser.json
  stdout
  stderr
  observations.jsonl
  claude-stdin.jsonl
  claude-events.jsonl
  claude-result.json
  context.jsonl
  result.json
```

`stdout` is caller-facing observation JSONL. Raw Claude stdout frames are preserved in `claude-events.jsonl`.

## Limitations

- MCP config temp-file support is not implemented yet.
- `.claude/skills/` injection is not implemented yet.
- Permission/policy is currently limited to passing explicit `AHAND_AGENT_PERMISSION_MODE`; richer audit records are still planned.
- Claude Code control-request permission loops are not implemented; this path writes one user message and closes stdin.
