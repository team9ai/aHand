# Agent Stdio Format Architecture

This document defines AHand's long-term stdio contract for local agent processes. It replaces the older target design that used the single `format` field for stdout parsing.

## Stable Knobs

AHand should expose three independent choices:

```text
executionMode
inputFormat
outputFormat
```

### `executionMode`

`executionMode` selects the process I/O transport.

```text
executionMode=pipe_stream
```

For agent integrations, `pipe_stream` means AHand runs the child process with piped stdin, stdout, and stderr. It does not mean Codex, Claude Code, Hermes, ACP, JSONL, or any other agent protocol.

### `inputFormat`

`inputFormat` selects how AHand converts its unified task input into the child process stdin protocol.

Supported target values:

| Value | Meaning |
|---|---|
| `raw` | Do not transform input. Forward caller-provided stdin chunks as-is. |
| `text` | Write the prompt as plain text to stdin, then close stdin for single-turn runs. |
| `claude-stream-json` | Write one Claude Code stream-json `type=user` message to stdin, then close stdin. |
| `hermes-acp-json-rpc` | Drive Hermes ACP JSON-RPC requests on stdin, including `initialize`, session setup, and `session/prompt`. |

### `outputFormat`

`outputFormat` selects how AHand handles child stdout before exposing caller-facing stdout.

Supported target values:

| Value | Meaning |
|---|---|
| `raw` | Do not parse or normalize stdout. Caller-facing stdout is child stdout bytes. |
| `codex-jsonl` | Parse Codex JSONL stdout and emit unified `AgentObservationRecord` JSONL. |
| `claude-stream-json` | Parse Claude Code stream-json stdout and emit unified `AgentObservationRecord` JSONL. |
| `hermes-acp-json-rpc` | Parse Hermes ACP JSON-RPC stdout and emit unified `AgentObservationRecord` JSONL. |

`outputFormat` replaces the older `format` field. The old `format` field is deprecated and should not be used in new API design.

## Agent Recipes

These are presets made from the three stable knobs:

| Agent | `executionMode` | `inputFormat` | `outputFormat` |
|---|---|---|---|
| Raw process | `pipe_stream` | `raw` | `raw` |
| Codex | `pipe_stream` | `text` | `codex-jsonl` |
| Claude Code | `pipe_stream` | `claude-stream-json` | `claude-stream-json` |
| Hermes ACP | `pipe_stream` | `hermes-acp-json-rpc` | `hermes-acp-json-rpc` |

## Compatibility Fields

Existing implementation fields may remain temporarily for compatibility, but they are not the long-term API:

| Existing field | Status | Long-term replacement |
|---|---|---|
| `format` | Deprecated | `outputFormat` |
| `resultParser` | Compatibility parser hint | `outputFormat` |

New plans and APIs should not add behavior that depends on `format`; use `outputFormat` instead. Input conversion must use `inputFormat`.

## Examples

Codex:

```bash
ahandctl agent run \
  --execution-mode pipe_stream \
  --input-format text \
  --output-format codex-jsonl \
  --executable /path/to/codex \
  --cwd "$PWD" \
  --prompt "Run tests"
```

Claude Code:

```bash
ahandctl agent run \
  --execution-mode pipe_stream \
  --input-format claude-stream-json \
  --output-format claude-stream-json \
  --executable /path/to/claude \
  --cwd "$PWD" \
  --prompt "Review this repo"
```

Hermes ACP:

```bash
ahandctl agent run \
  --execution-mode pipe_stream \
  --input-format hermes-acp-json-rpc \
  --output-format hermes-acp-json-rpc \
  --executable /path/to/hermes \
  --cwd "$PWD" \
  --prompt "Inspect this repository"
```

Raw process:

```bash
ahandctl exec \
  --execution-mode pipe_stream \
  --input-format raw \
  --output-format raw \
  --cwd "$PWD" \
  sh -- -c 'cat'
```
