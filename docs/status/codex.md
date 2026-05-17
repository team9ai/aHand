# Codex Integration Status

**Last updated:** 2026-05-17  
**Document type:** long-lived status reference, not an implementation plan

This document records the current AHand integration state for Codex. It should be updated whenever the Codex execution contract, formatter schema, or production readiness changes.

## Summary

Codex is currently integrated as a process-style CLI agent.

AHand starts Codex with `execution_mode = pipe_stream`, writes the prompt as text stdin, reads Codex JSONL from stdout, and converts it into AHand `AgentObservationRecord` JSONL when `outputFormat = codex-jsonl` is selected.

The current supported shape is single-turn CLI launch:

```text
caller
-> AHand job
-> ahandd pipe_stream executor
-> codex exec --json ...
-> stdout JSONL
-> Codex formatter
-> AHand observation JSONL
```

Codex is not currently implemented as a daemon-side protocol backend. AHand does not speak a Codex-specific bidirectional protocol beyond stdin/stdout process pipes.

## Current Status

| Area | Status |
|---|---|
| `pipe_stream` execution | Implemented |
| Codex JSONL parser / formatter | Implemented |
| Caller-facing observation JSONL | Implemented with `outputFormat=codex-jsonl` |
| Raw stdout preservation | Implemented via run artifacts |
| Hub control-plane field forwarding | Implemented for `executionMode`, `inputFormat`, `outputFormat`, and compatibility `resultParser` / `format` |
| SDK field support | Implemented for `executionMode`, `inputFormat`, `outputFormat`, and compatibility `resultParser` / `format` |
| Local IPC debug path | Implemented |
| Codex thread resume | Supported by launching `codex exec resume ...`; AHand does not manage it as a first-class session yet |
| Typed high-level agent API | Not implemented |
| Dashboard-specific Codex view | Not implemented |

## Execution Contract

Codex should be launched through `pipe_stream`:

```text
executionMode = "pipe_stream"
inputFormat   = "text"
outputFormat  = "codex-jsonl"
resultParser  = "codex-jsonl"
```

Typical command:

```bash
printf 'Run tests and explain failures\n' | cargo run -p ahandctl -- \
  --ipc /tmp/ahand-local-debug.sock \
  exec \
  --execution-mode pipe_stream \
  --input-format text \
  --output-format codex-jsonl \
  --result-parser codex-jsonl \
  --cwd "$PWD" \
  codex -- exec --skip-git-repo-check --json --cd "$PWD" -
```

Resume is handled by passing the Codex thread id to the Codex CLI:

```bash
printf 'Continue from the previous result\n' | cargo run -p ahandctl -- \
  --ipc /tmp/ahand-local-debug.sock \
  exec \
  --execution-mode pipe_stream \
  --input-format text \
  --output-format codex-jsonl \
  --result-parser codex-jsonl \
  --cwd "$PWD" \
  codex -- exec resume --skip-git-repo-check <thread_id> --json -
```

## Stdout Contract

AHand treats stdout as the user-facing stream.

| `outputFormat` | Caller-facing stdout |
|---|---|
| `raw` | Codex raw JSONL stdout |
| `codex-jsonl` | AHand `AgentObservationRecord` JSONL |

When `outputFormat=codex-jsonl`, raw Codex stdout is still preserved in run artifacts for debugging and replay.

## Observation Mapping

The current Codex formatter emits these observation kinds:

| Codex event | AHand observation |
|---|---|
| `thread.started` | `agent_session` |
| `turn.started` | `llm_call_start` |
| `item.completed` with `agent_message` | `llm_call_delta` |
| `item.started` with `command_execution` | `tool_call_start` |
| `item.completed` with `command_execution` output | `tool_call_output` |
| `item.completed` with `command_execution` exit status | `tool_call_end` |
| `turn.completed` | `llm_call_end` |
| `error` | `error` |
| unknown event | `raw` |
| malformed JSONL line | `parse_error` |

The formatter preserves the raw Codex JSON event in each observation under `raw`.

## Run Artifacts

When `ahandd` is started with a data directory, Codex jobs write artifacts under:

```text
runs/<job_id>/
  request.json
  parser.json
  stdout
  stderr
  observations.jsonl
  result.json
```

For `outputFormat=codex-jsonl`:

- `stdout` is the caller-facing observation JSONL.
- raw child stdout is still preserved by the executor path before formatting.
- `observations.jsonl` is a debug/replay copy of formatted observations.

## Known Limitations

- AHand does not currently own Codex session state as a first-class field.
- Resume works by explicitly launching `codex exec resume <thread_id>`.
- Codex formatter depends on the current Codex JSONL event names and shapes.
- Codex JSONL does not always expose full LLM request payloads; missing inputs are represented as unobserved rather than empty.
- There is no typed `spawnAgent()` SDK helper yet.
- There is no dashboard-specific Codex timeline view yet.

## Related Documents

- `docs/usage/claude-codex-pipe-stream.md`
- `docs/plans/2026-05-13-codex-jsonl-result-parser.md`
- `docs/plans/2026-05-13-agent-formatter-observation-dimensions.md`
- `docs/plans/2026-05-12-result-parser-for-agent-output.md`
