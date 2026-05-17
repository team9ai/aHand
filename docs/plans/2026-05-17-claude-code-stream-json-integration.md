# Claude Code Stream JSON Integration 开发计划

**Goal:** 为 AHand 增加 Claude Code 接入能力：调用方显式提供 `claude` 可执行路径、cwd、env、prompt、model/session 等参数，daemon 启动 Claude Code 的非交互 `stream-json` 模式，向 stdin 写入一条 user message，读取 stdout JSONL 事件，并输出统一 `AgentObservationRecord` JSONL。AHand 不做 Claude CLI 探测、不复制 Multica 的 runtime/agent/task 表，只提供一个明确、可审计、可回放的 Claude Code 执行通道。

**Architecture:** Claude Code 不是 ACP JSON-RPC。它使用本地进程 stdio 上的 `stream-json` 行协议：stdin 写一条 `type=user` JSON，stdout 输出一行一个 `system` / `assistant` / `user` / `result` / `log` 事件，stderr 输出 CLI 诊断。AHand 应复用现有 `pipe_stream` 进程 transport，并用 `inputFormat=claude-stream-json` 构造固定 CLI 参数、把 prompt 转成 Claude `type=user` stdin JSON、关闭 stdin、收集 stderr tail、处理 timeout/cancel；用 `outputFormat=claude-stream-json` 把 Claude Code stdout stream-json 映射成 AHand observation records。

**Tech Stack:** Rust, tokio process/io, serde_json, ahandd RunStore, local IPC, hub control-plane, Claude Code `stream-json`

**Related Docs:**
- `docs/CLAUDE_CODE_INTEGRATION.md`
- `docs/CLAUDE_CODE_DATA_EXCHANGE.md`
- `docs/usage/claude-codex-pipe-stream.md`
- `docs/plans/2026-05-12-result-parser-for-agent-output.md`
- `docs/plans/2026-05-13-agent-formatter-observation-dimensions.md`
- `docs/plans/2026-05-16-hermes-acp-integration.md`

---

## 背景

AHand 当前已经有三块相关基础：

- `execution_mode = pipe_stream`：可启动非 TTY 子进程，并保持 stdin/stdout/stderr 管道。
- `result_parser = claude-stream-json`：协议层已经允许该 parser hint。
- `outputFormat = claude-stream-json`：目标上替代旧 `format`。

但目前 Claude Code 仍然只是保留位。`docs/usage/claude-codex-pipe-stream.md` 也明确写过：Claude Code `stream-json` 解析还没有实现。

根据 `docs/CLAUDE_CODE_INTEGRATION.md` 和 `docs/CLAUDE_CODE_DATA_EXCHANGE.md`，Multica 接入 Claude Code 的方式是：

```text
daemon
  -> spawn claude -p --output-format stream-json --input-format stream-json ...
  -> stdin write one {"type":"user",...}\n
  -> close stdin
  -> stdout read stream-json events
  -> stderr collect diagnostics/tail
```

这和 Hermes ACP 不同：

- Claude Code 没有 JSON-RPC request id。
- session 创建由 CLI 内部处理，恢复通过 `--resume <session_id>` 参数。
- prompt 不是 RPC，而是 stdin 上的一条 user message。
- 权限默认通过 CLI 参数 `--permission-mode` 控制，不是运行中 ACP `session/request_permission`。

因此 AHand 的 Claude Code 接入应当是 **`inputFormat=claude-stream-json` + `outputFormat=claude-stream-json`**，不是 ACP，也不是 backend 开关。

## 目标

第一版 Claude Code 接入需要做到：

- 调用方显式传入 Claude 可执行路径，或显式提供能解析 `claude` 的 PATH env。
- AHand 固定启动 Claude Code 非交互 stream-json 模式。
- AHand 向 stdin 写入一条 user message，并关闭 stdin。
- AHand 能可选传 `--model`、`--resume`、`--max-turns`、`--append-system-prompt`、`--mcp-config`。
- AHand 能把 `system` / `assistant` / `user` / `result` / `log` 事件映射成统一 observation records。
- AHand 能保存 Claude session id，用于后续 resume。
- AHand 能保存 raw stdout/stderr、normalized observations、stderr tail、MCP temp config metadata。
- AHand 不默认使用 `bypassPermissions`，除非调用方或 session policy 明确要求。
- AHand 能防止用户 custom args 覆盖关键协议参数。

## 非目标

第一阶段不做：

- 不自动探测 PATH 中的 `claude`。
- 不复制 Multica 的 `agent_runtime` / `agent` / `agent_task_queue` 表。
- 不把 Claude Code 设为默认 agent。
- 不做 Claude Code 模型动态发现；模型 catalog 可后续单独补。
- 不默认开启 `--permission-mode bypassPermissions`。
- 不要求 dashboard 第一阶段有 Claude 专属 UI。
- 不实现长驻 Claude Code process pool。
- 不实现 Claude Code 的 `control_request` 交互授权闭环；第一版按一次性 prompt + close stdin 模式。

## 设计原则

1. **Claude Code 是 stream-json input/output format，不是 ACP，也不是 execution mode。**  
   `ExecutionMode::PipeStream` 能承载 stdio，但 `inputFormat=claude-stream-json` 必须负责固定 CLI 参数、stdin user message、stdin close、stderr tail 和最终状态。

2. **raw 两侧都必须保留。**  
   `inputFormat=raw` 表示不转换 stdin；`outputFormat=raw` 表示 caller-facing stdout 仍是 child raw stdout。Claude Code 正式接入不使用 raw 作为完成标准，但 raw 是 pipe_stream 的基础能力。

3. **formatter 输出统一 observation。**  
   Claude 专属事件必须映射到 AHand 已有 observation 维度：`agent`、`runtime`、`llmResponse`、`toolCall`、`error`、`status`、`raw`、`usage`。

4. **raw 可回放。**  
   原始 stdout JSONL、stderr、stdin prompt JSON、MCP config metadata 都要保存到 RunStore。formatter 失败时不能丢失原始事件。

5. **协议参数由 AHand 控制。**  
   `-p`、`--output-format`、`--input-format`、`--mcp-config`、`--permission-mode` 等会影响通信安全和协议稳定性的参数不能被 custom args 覆盖。

6. **安全默认值保守。**  
   Multica 使用 `--permission-mode bypassPermissions` 适合无人值守任务。AHand 第一版不默认绕过权限；是否 bypass 必须由显式 job env/typed field/session policy 指定并输出 audit/observation。

## 运行模型

第一版采用 job-scoped Claude Code 进程：

```text
JobRequest
  -> validate claude_path/cwd/env/prompt
  -> write context files: CLAUDE.md and optional .claude/skills/
  -> write MCP config temp file if provided
  -> spawn claude -p --output-format stream-json --input-format stream-json ...
  -> write one user message JSON line to stdin
  -> close stdin
  -> read stdout stream-json events
  -> collect stderr raw + bounded tail
  -> finish job and terminate child if still alive
```

这意味着：

- 每个 AHand job 默认对应一个 Claude Code child process。
- Claude session id 来自 `system.session_id` 或 `result.session_id`。
- resume 通过 `--resume <session_id>` 参数完成。
- model 通过 `--model <model>` 参数完成。
- MCP 通过临时 JSON 文件 + `--mcp-config <path>` 完成。
- 后续如果需要长驻 Claude Code process，可以在同一 format runner 之上增加 session/process pool；第一版不做。

## 输入契约

建议复用 Hermes / Codex 的统一 format fields，由 `inputFormat=claude-stream-json` 转成 Claude 原生 stream-json 输入。当前实现可先使用 env/typed bridge：

```text
AHAND_INPUT_FORMAT=claude-stream-json
AHAND_OUTPUT_FORMAT=claude-stream-json
AHAND_AGENT_EXECUTABLE=/absolute/path/to/claude
AHAND_AGENT_PROMPT=<prompt>
AHAND_AGENT_MODEL=<model>                 # optional
AHAND_AGENT_SESSION_ID=<session_id>       # optional, maps to --resume
AHAND_AGENT_MAX_TURNS=<n>                 # optional
AHAND_AGENT_SYSTEM_PROMPT=<runtime brief> # optional, maps to --append-system-prompt
AHAND_AGENT_MCP_CONFIG=<json>             # optional, writes temp file
AHAND_AGENT_PERMISSION_MODE=<mode>        # optional, default conservative
AHAND_AGENT_INSTRUCTIONS=<CLAUDE.md text> # optional context injection
```

hub/SDK typed fields can mirror the same shape:

```text
inputFormat = "claude-stream-json"
outputFormat = "claude-stream-json"
executable
prompt
model
sessionId
maxTurns
systemPrompt
mcpConfig
permissionMode
instructions
```

兼容策略：

- 未设置 `inputFormat` / `outputFormat` 时默认为 `raw`，保持当前 process job 行为。
- `inputFormat = "claude-stream-json"` 时 daemon 写 Claude Code stream-json user message。
- `outputFormat = "claude-stream-json"` 时 daemon parse Claude Code stdout，并输出 observation JSONL。
- 旧 `result_parser = claude-stream-json` 只作为兼容 parser hint；旧 `format` 废弃，目标字段是 `outputFormat`。
- hub/SDK 对未知 format 做 400，不静默降级。

统一输入到 Claude 原生协议的映射：

| AHand field | Claude Code native action |
|---|---|
| `inputFormat=claude-stream-json` | stdin JSONL `{"type":"user","message":...}` |
| `outputFormat=claude-stream-json` | parse stdout stream-json events |
| `executable` | spawn Claude executable |
| `prompt` | user message text |
| `model` | `--model` |
| `sessionId` | `--resume` |
| `maxTurns` | `--max-turns` |
| `systemPrompt` | `--append-system-prompt` |
| `permissionMode` | `--permission-mode`, with audit/observation |
| `instructions` | non-overwriting `CLAUDE.md` / `CLAUDE.ahand.md` context file |

## 启动命令

固定协议参数：

```text
claude
  -p
  --output-format stream-json
  --input-format stream-json
  --verbose
  --strict-mcp-config
  --disallowedTools AskUserQuestion
```

可选参数：

```text
--model <model>
--max-turns <n>
--append-system-prompt <runtime brief>
--resume <session_id>
--mcp-config <temp-json-path>
--permission-mode <mode>
```

第一版建议：

- 默认不传 `--permission-mode bypassPermissions`。
- 如果显式传 `AHAND_AGENT_PERMISSION_MODE=bypassPermissions`，必须输出 `policy_decision` / `audit` observation，并写入 run metadata。
- 禁止 custom args 覆盖固定协议参数。

## Stdin User Message

adapter 启动 Claude 后写入一条 JSONL：

```json
{
  "type": "user",
  "message": {
    "role": "user",
    "content": [
      {
        "type": "text",
        "text": "具体任务 prompt"
      }
    ]
  }
}
```

写完后关闭 stdin。

RunStore 保存：

```text
claude-stdin.jsonl
```

## Daemon 模块设计

新增或扩展：

```text
crates/ahandd/src/agent/
  mod.rs
  input_format.rs
  output_format.rs
  claude_code.rs
```

职责：

| 文件 | 职责 |
|---|---|
| `input_format.rs` | `InputFormat` trait / factory：`raw`、`text`、`claude-stream-json`、`hermes-acp-json-rpc` |
| `output_format.rs` | `OutputFormat` trait / factory：`raw`、`codex-jsonl`、`claude-stream-json`、`hermes-acp-json-rpc` |
| `claude_code.rs` | Claude stream-json stdin/stdout format runner、构造 Claude CLI 参数、写 stdin user message、读取 stdout/stderr、session/result 状态、stderr tail、MCP temp file |
| `result_parser.rs` | 可拆出或扩展 Claude Code stdout parser/formatter |
| `mod.rs` | format runner dispatch |

第一阶段可以不抽 trait，沿用 Hermes 当前形态：

```rust
pub async fn run_claude_code<T>(
    device_id: String,
    req: JobRequest,
    tx: T,
    cancel_rx: mpsc::Receiver<()>,
    store: Option<Arc<RunStore>>,
) -> (i32, String)
where
    T: EnvelopeSink;
```

## Formatter 映射

Claude stream-json 顶层 envelope：

```text
type
message
subtype
session_id
result
is_error
duration_ms
num_turns
log
request_id
request
```

### `system`

输入：

```json
{ "type": "system", "subtype": "init", "session_id": "claude-session-123" }
```

输出：

```text
agent_session
status running
```

记录 `agent.agentSessionId`。

### `assistant`

遍历 `message.content[]`：

| Claude block | AHand observation |
|---|---|
| `text` | `llm_call_delta.responseText` |
| `thinking` | `llm_call_delta.channel = thinking` |
| `tool_use` | `tool_call_start` |

字段优先级：

```text
message.model -> agent.model.id
content[].id -> toolCall.toolCallId
content[].name -> toolCall.toolName
content[].input -> toolCall.input
message.usage -> llm_call_end.usage snapshot/additive usage
```

Claude usage 映射：

```text
input_tokens -> inputTokens
output_tokens -> outputTokens
cache_read_input_tokens -> cachedReadTokens
cache_creation_input_tokens -> cachedWriteTokens
```

Claude usage 是 assistant event 中的增量/分段数据，formatter 需要按 model 累加，最终 `llm_call_end` 输出累计 usage。

### `user`

遍历 `message.content[]`：

| Claude block | AHand observation |
|---|---|
| `tool_result` | `tool_call_output` 或 `tool_call_end` |

字段优先级：

```text
content[].tool_use_id -> toolCall.toolCallId
content[].content -> toolCall.outputText
content[].is_error -> toolCall.status failed/completed
```

如果 `content` 是数组或 object，使用结构化 JSON 保留到 `toolCall.output`，并输出压缩 `outputText`。

### `result`

输入：

```json
{
  "type": "result",
  "subtype": "success",
  "session_id": "claude-session-123",
  "result": "最终回答内容",
  "is_error": false,
  "duration_ms": 12345,
  "num_turns": 3
}
```

输出：

- `is_error = false`：`llm_call_end` + final status completed。
- `is_error = true`：`error` observation，并让 job failed。
- `result` 非空时输出最终 `llm_call_delta.responseText` 或 `llm_call_end.finalText`。
- 保存 `claude-session.json`。

### `log`

输出：

```text
status/log observation
```

字段：

```text
log.level
log.message
```

### unknown / malformed

- unknown type -> `raw`
- malformed line -> `parse_error`
- raw JSON 始终保存。

## stderr 处理

Claude stderr 不走 stream-json。

实现要求：

- stderr 原样发送为 `JobEvent.stderr`。
- stderr 原样写入 RunStore `stderr`。
- 保存 bounded `stderr_tail`，用于最终错误。
- 识别常见 provider/CLI 错误：
  - auth / invalid api key / login required
  - rate limit / 429
  - quota / billing / credits
  - CLI crash / command not found / config error
- 如果 stdout `result` 看似成功但 stderr tail 有明确 provider failure，输出 `error` observation，并按失败处理或至少输出 terminal error observation。

## Context Injection

Claude Code 使用原生上下文路径：

```text
{cwd}/CLAUDE.md
{cwd}/.claude/skills/{skill-name}/SKILL.md
```

第一版实现：

- `AHAND_AGENT_INSTRUCTIONS` 写入 `CLAUDE.md`。
- 如果 `CLAUDE.md` 已存在，不覆盖；改写 `CLAUDE.ahand.md`，并在 prompt/system prompt 中引用。
- RunStore 写 `context.jsonl` 记录写入路径。

第二版：

- 支持 `.claude/skills/` 注入。
- 支持技能名冲突策略：不覆盖用户已有 skill，写 `.claude/skills/ahand-{name}` 或失败。

## MCP Config

Claude MCP 配置通过临时文件：

```text
--mcp-config /tmp/ahand-claude-mcp-xxxx.json
```

要求：

- `AHAND_AGENT_MCP_CONFIG` 必须是 JSON object。
- 写入 temp file 后传给 Claude。
- 始终启用 `--strict-mcp-config`。
- RunStore 记录 mcp config hash/path，不把敏感完整内容默认写入 stdout。
- 进程结束后删除 temp file。

## Policy and Safety

第一版安全策略：

- 不默认 `bypassPermissions`。
- 显式 `bypassPermissions` 必须输出 `policy_decision` observation：

```text
policy.decision = "allow"
policy.reason = "explicit Claude permission mode bypassPermissions"
```

- 禁止 custom args 覆盖关键协议参数：

```text
-p
--output-format
--input-format
--permission-mode
--mcp-config
--permission-prompt-tool
```

- 过滤父进程中容易污染子 Claude Code 的 env：

```text
CLAUDECODE
CLAUDECODE_*
CLAUDE_CODE_*
```

- 用户 env 不能覆盖 AHand 内部任务上下文 env。

## Hub / SDK / CLI Surface

### ahandctl

新增：

```bash
ahandctl claude-code /absolute/path/to/claude \
  --cwd "$PWD" \
  --prompt "Review this repo" \
  --model "claude-sonnet-4-6" \
  --session-id "claude-session-123" \
  --env PATH="$PATH" \
  --env HOME="$HOME"
```

底层发送：

```text
AHAND_INPUT_FORMAT=claude-stream-json
AHAND_OUTPUT_FORMAT=claude-stream-json
AHAND_AGENT_EXECUTABLE=/absolute/path/to/claude
AHAND_AGENT_PROMPT=...
executionMode=pipe_stream
resultParser=claude-stream-json
outputFormat=claude-stream-json
```

### Hub

扩展 `/api/control/jobs` typed format bridge：

```json
{
  "executionMode": "pipe_stream",
  "inputFormat": "claude-stream-json",
  "outputFormat": "claude-stream-json",
  "executable": "/absolute/path/to/claude",
  "prompt": "Review this repo",
  "model": "claude-sonnet-4-6",
  "sessionId": "claude-session-123",
  "maxTurns": 8,
  "permissionMode": "default"
}
```

hub 转成 daemon env，并强制：

```text
executionMode = pipe_stream
resultParser = claude-stream-json
outputFormat = claude-stream-json
```

### SDK

扩展 `CloudClient.spawnAgent()`：

```ts
await client.spawnAgent({
  deviceId: "device-123",
  executionMode: "pipe_stream",
  inputFormat: "claude-stream-json",
  outputFormat: "claude-stream-json",
  executable: "/absolute/path/to/claude",
  cwd: "/repo",
  prompt: "Run tests and explain failures.",
  model: "claude-sonnet-4-6",
  sessionId: "claude-session-123",
  onObservation: (record) => {},
});
```

## Run Artifacts

Claude jobs 写入：

```text
runs/<job_id>/
  request.json
  parser.json
  stdout
  stderr
  observations.jsonl
  claude-stdin.jsonl
  claude-session.json
  claude-result.json
  context.jsonl
  mcp-config.jsonl
  result.json
```

含义：

| File | Meaning |
|---|---|
| `stdout` | caller-facing observation JSONL |
| `stderr` | raw Claude stderr |
| `observations.jsonl` | normalized observation copy |
| `claude-stdin.jsonl` | AHand -> Claude user message |
| `claude-session.json` | captured session id/model/result session metadata |
| `claude-result.json` | final raw result event and accumulated usage |
| `context.jsonl` | context files written by AHand |
| `mcp-config.jsonl` | temp MCP config metadata |

## 实施阶段

### Phase 0: Fixture and Protocol Confirmation

- [ ] 用真实 `claude` 采集 `system` / `assistant` / `user tool_result` / `result` / `log` 样例。
- [ ] 覆盖 text、thinking、tool_use、tool_result、usage、result error。
- [ ] 保存 fixture 到 `crates/ahandd/tests/fixtures/claude-code/`。
- [ ] 记录 Claude Code version 和必要 CLI 参数。

验收：

- formatter 单元测试不依赖真实 Claude CLI。
- fixture 能覆盖文档中的全部主要事件类型。

### Phase 1: Explicit Launch Adapter

- [x] 新增 `inputFormat=claude-stream-json` / `outputFormat=claude-stream-json` 识别。
- [ ] 将 Claude Code env/typed bridge 对齐到统一 `AgentInput`，由 ClaudeCodeInputAdapter 消费。
- [x] 新增 daemon-side `run_claude_code`。
- [x] 构造固定 Claude CLI 协议参数。
- [x] 支持 model/session/max-turns/system-prompt。
- [x] 写 stdin user message 并关闭 stdin。
- [x] timeout/cancel 时 kill child。
- [x] stdout/stderr 分离捕获。

验收：

- 缺 executable/prompt 明确失败。
- fake Claude script 可模拟 stdin/stdout 并跑通 end-to-end。

### Phase 2: Stream JSON Formatter

- [x] 新增 `ClaudeCodeFormatter` 或扩展 `result_parser.rs` factory。
- [x] `system` -> `agent_session` / running status。
- [x] `assistant.content[].text` -> `llm_call_delta`。
- [x] `assistant.content[].thinking` -> thinking channel。
- [x] `assistant.content[].tool_use` -> `tool_call_start`。
- [x] `user.content[].tool_result` -> `tool_call_output/end`。
- [x] `assistant.message.usage` -> 累计 usage。
- [x] `result` -> final output/error/session/result metadata。
- [x] `log` -> status/log observation。
- [x] unknown -> `raw`。
- [x] malformed -> `parse_error`。

验收：

- caller-facing stdout 是 observation JSONL。
- `observations.jsonl` 和 stdout 内容一致。
- fixture 覆盖全部主事件类型。

### Phase 3: Final Result and Error Semantics

- [x] `result.is_error=true` 让 job failed。
- [x] 非零 exit 且无 result error 时，用 stderr tail 构造 error。
- [x] timeout -> failed with timeout message。
- [x] cancel -> cancelled/failed with cancelled message。
- [x] 保存 `claude-result.json`。
- [ ] 保存 `claude-session.json`。

验收：

- 失败不会被误标成功。
- stderr tail 出现在最终 error 中。

### Phase 4: Context and Skills

- [x] 写 `CLAUDE.md`，不覆盖用户文件。
- [x] 已存在时写 `CLAUDE.ahand.md`。
- [ ] 在 prompt/system prompt 中引用 `CLAUDE.ahand.md`。
- [x] RunStore 记录 context 写入。
- [ ] 支持 `.claude/skills/` 注入。
- [ ] skill 冲突策略明确且有测试。

验收：

- Claude 能在 cwd 读取 AHand 注入上下文。
- 用户已有上下文文件不被静默覆盖。

### Phase 5: MCP Config

- [ ] 支持 `AHAND_AGENT_MCP_CONFIG`。
- [ ] 校验 JSON object。
- [ ] 写 temp file。
- [ ] 追加 `--mcp-config`。
- [ ] 进程结束后删除 temp file。
- [ ] RunStore 保存 MCP metadata。

验收：

- invalid MCP JSON 明确失败。
- temp file 生命周期受控。

### Phase 6: Policy and Safety

- [x] 默认不传 `--permission-mode bypassPermissions`。
- [ ] 显式 bypass 输出 `policy_decision` observation。
- [ ] 禁止 custom args 覆盖关键协议参数。
- [x] 过滤 `CLAUDECODE*` / `CLAUDE_CODE*` 父环境污染。
- [ ] 用户 env 不能覆盖 AHand 内部 env。

验收：

- 安全相关行为可在 stdout observations 和 run artifacts 中追踪。
- custom args 无法破坏 stream-json 协议。

### Phase 7: Hub / SDK / CLI

- [x] `ahandctl claude-code` 调试入口。
- [x] hub `/api/control/jobs` 支持 `inputFormat=claude-stream-json` / `outputFormat=claude-stream-json` typed bridge。
- [x] SDK `spawnAgent({ inputFormat: "claude-stream-json", outputFormat: "claude-stream-json" })`。
- [x] usage 文档更新。
- [x] long-term status 文档更新。

验收：

- SDK 用户不需要手写 stream-json stdin。
- 旧 process jobs 行为不变。

## 测试计划

单元测试：

- formatter opt-in：`outputFormat=claude-stream-json` + `result_parser=claude-stream-json`。
- line buffering：chunk boundary、final line without newline。
- parse error -> observation。
- system session id capture。
- assistant text/thinking/tool_use。
- user tool_result string/object/array。
- usage accumulation by model。
- result success/error。
- log event。
- stderr provider/CLI error detector。
- CLI arg sanitizer。
- context file no-overwrite。
- MCP config validation/temp metadata。

集成测试：

- fake Claude script 读取 stdin user message，输出 system/assistant/result。
- fake Claude script 输出 `is_error=true`。
- fake Claude script 非零 exit + stderr tail。
- `ahandctl claude-code` local IPC path。
- hub typed bridge -> daemon env。
- SDK `spawnAgent({ inputFormat: "claude-stream-json", outputFormat: "claude-stream-json" })` request shape and observation parsing。

## 验收标准

- `cargo test -p ahandd claude_code --lib` 通过。
- `cargo check -p ahandd -p ahandctl -p ahand-hub` 通过。
- `pnpm --filter @ahandai/sdk lint` 通过。
- `pnpm --filter @ahandai/sdk test` 通过。
- `git diff --check` 通过。
- 使用 fake Claude CLI 可以端到端输出 observation JSONL。
- 使用真实 Claude Code 可以完成一个只读任务，并保存 session id / raw event / observations。
