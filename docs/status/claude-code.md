# Claude Code Integration Status

**Last updated:** 2026-05-17  
**Document type:** long-lived status reference, not an implementation plan

Claude Code is currently integrated as an explicit daemon-side `stream-json` stdio format.

AHand does not discover Claude Code or treat it as an ACP server. A caller explicitly supplies the executable path, environment, cwd, and prompt. AHand starts:

```text
claude -p --output-format stream-json --input-format stream-json --verbose --strict-mcp-config --disallowedTools AskUserQuestion
```

then writes one `type=user` JSON message to stdin and closes stdin.

## Current Status

| Area | Status |
|---|---|
| Explicit format selection | Implemented via `inputFormat=claude-stream-json` and `outputFormat=claude-stream-json` |
| Explicit executable | Implemented via `AHAND_AGENT_EXECUTABLE` or `JobRequest.tool` |
| Prompt input | Implemented via `AHAND_AGENT_PROMPT` |
| Model input | Implemented via `AHAND_AGENT_MODEL` -> `--model` |
| Session resume | Implemented via `AHAND_AGENT_SESSION_ID` -> `--resume` |
| Max turns | Implemented via `AHAND_AGENT_MAX_TURNS` -> `--max-turns` |
| System prompt | Implemented via `AHAND_AGENT_SYSTEM_PROMPT` -> `--append-system-prompt` |
| Permission mode | Implemented via `AHAND_AGENT_PERMISSION_MODE` -> `--permission-mode` |
| Context file injection | Implemented for non-overwriting `CLAUDE.md` / `CLAUDE.ahand.md` |
| Stream-json stdout formatter | Implemented |
| Stderr raw capture and tail errors | Implemented |
| `ahandctl claude-code` | Implemented |
| Hub typed bridge | Implemented on `/api/control/jobs` |
| SDK typed helper | Implemented as `CloudClient.spawnAgent({ inputFormat: "claude-stream-json", outputFormat: "claude-stream-json" })` |
| MCP config temp file | Not implemented |
| `.claude/skills/` injection | Not implemented |
| Control-request permission loop | Not implemented |

## Formatter Contract

Caller-facing stdout is AHand observation JSONL. Raw Claude stdout is saved separately.

| Claude event | AHand observation |
|---|---|
| `system` | `agent_session`, `status` |
| `assistant.content[].text` | `llm_call_delta` |
| `assistant.content[].thinking` | `llm_call_delta` with `channel=thinking` |
| `assistant.content[].tool_use` | `tool_call_start` |
| `user.content[].tool_result` | `tool_call_output` |
| `assistant.message.usage` | `llm_call_end` usage snapshot |
| `result` | `llm_call_delta`, `llm_call_end`, optional `error` |
| `log` | `status` |
| malformed line | `parse_error` |
| unknown event | `raw` |

## Artifacts

```text
runs/<job_id>/
  stdout
  stderr
  observations.jsonl
  claude-stdin.jsonl
  claude-events.jsonl
  claude-result.json
  context.jsonl
```

## Related Documents

- `docs/usage/claude-code.md`
- `docs/CLAUDE_CODE_INTEGRATION.md`
- `docs/CLAUDE_CODE_DATA_EXCHANGE.md`
- `docs/plans/2026-05-17-claude-code-stream-json-integration.md`
