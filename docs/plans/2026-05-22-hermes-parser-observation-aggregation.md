# Hermes Parser Observation Aggregation 开发计划

**Goal:** 为 AHand 增加专门的 `parser=hermes`，与 `parser=codex-jsonl` 平级。Hermes ACP 原始事件仍保留在 raw artifacts 中，但 `observations.jsonl` 只输出聚合后的统一 observation records。Hermes token 级 `agent_thought_chunk` / `agent_message_chunk` 不再直接暴露为大量 `llm_call_delta`，而是聚合为保留 `thinking` 的 `llm_message`，使 Hermes 和 Codex 在 observation 层可直接比较。

**Architecture:** Hermes runtime 继续通过 `pipe_stream` 启动 `hermes acp`。输入侧仍由 Hermes ACP runner 写入 `initialize`、`session/new`、`session/prompt` 等 JSON-RPC request；输出侧新增 `parser=hermes`，消费 Hermes stdout 上的 ACP JSON-RPC response / notification，保存 raw ACP 流，并把语义事件写成 AHand `AgentObservationRecord` JSONL。Codex 仍由 `parser=codex-jsonl` 消费 Codex 原生 JSONL；两者在 parser 层之后对齐到同一组 observation kinds。

**Tech Stack:** Rust, serde_json, tokio process/io, AHand RunStore, Hermes ACP JSON-RPC, AgentObservationRecord

**Related Docs:**
- `docs/plans/2026-05-12-result-parser-for-agent-output.md`
- `docs/plans/2026-05-13-agent-formatter-observation-dimensions.md`
- `docs/plans/2026-05-13-codex-jsonl-result-parser.md`
- `docs/plans/2026-05-16-hermes-acp-integration.md`
- `docs/HERMES_DATA_EXCHANGE.md`
- `docs/usage/hermes-acp.md`

---

## 背景

当前 Hermes ACP 接入已经能通过 AHand 启动 Hermes、注入 MCP config、发送 prompt，并保存运行产物：

```text
runs/<job_id>/
  request.json
  stdout
  stderr
  acp-requests.jsonl
  acp-events.jsonl
  observations.jsonl
  result.json
  parser.json
```

但现有 Hermes 输出仍更接近 raw ACP stream。一次真实长任务中，Hermes observation 层出现了大量 token 级 delta：

```text
3139 llm_call_delta
113  tool_call_start
104  tool_call_end
```

其中 `llm_call_delta.responseText` 大多是单词或 token 片段：

```text
' need'
' to'
' collect'
' accepted'
','
```

相比之下，Codex `codex-jsonl` parser 输出的 `llm_call_delta` 已经接近用户可读消息块：

```text
11 llm_call_delta
平均长度约 140 字符
```

这导致同样读取 `observations.jsonl` 时，Hermes 和 Codex 的粒度不一致：

- Codex observation 层适合直接展示、统计和比较。
- Hermes observation 层仍暴露 token streaming 细节。
- 下游 consumer 必须自行聚合 Hermes delta，才能得到和 Codex 类似的语义消息。

Observation 层应该表达 AHand 的统一 agent 语义，而不是泄露各 runtime 的 streaming chunk 粒度。因此需要把 Hermes ACP 解析升级为专门 parser，并在 parser 层完成聚合。

## 目标

新增 `parser=hermes` 后，AHand 应做到：

- Hermes ACP raw events 继续完整保存，方便协议 debug。
- `observations.jsonl` 输出聚合后的语义 records。
- Hermes thinking 必须保留，但以 `llm_message(channel="thinking")` 形式出现。
- Hermes 可见回复以 `llm_message(channel="message")` 形式出现。
- Hermes token 级 delta 不再写入 `observations.jsonl`。
- Tool call、plan、usage、error、session 等事件映射到统一 observation kinds。
- Hermes 和 Codex 的 observation 层可直接比较。
- Parser 失败不影响 raw artifacts 和 child process lifecycle。
- 错误识别不再从普通 stderr INFO/WARNING 日志中猜测 provider error。

## 非目标

第一阶段不做：

- 不改变 `executionMode=pipe_stream`。
- 不改变 Hermes ACP 启动和 request 序列。
- 不移除 `stdout` / `stderr` / `acp-events.jsonl`。
- 不要求 Hermes 停止保存自己的 `~/.hermes/sessions/session_*.json`。
- 不在 observation 层保存完整 provider HTTP request / response body。
- 不要求 Codex 原始协议和 Hermes ACP 原始协议一致。
- 不把 token 级 replay 放进 `observations.jsonl`。
- 不因为 parser 聚合失败改变 job exit code。

## Parser 命名

建议把 Hermes parser 作为正式 parser 名称，而不是继续使用 `raw`：

```json
{
  "parser": "hermes",
  "format": "hermes-acp-json-rpc",
  "parser_version": 1,
  "status": "configured"
}
```

`parser=hermes` 与已有 parser 平级：

```text
raw
codex-jsonl
claude-stream-json
hermes
```

如果后续需要更明确的协议名，也可以把外部 format 保持为 `hermes-acp-json-rpc`，但 run artifact 中的 parser 名称应稳定表达实现：

```text
inputFormat=hermes-acp-json-rpc
outputFormat=hermes
parser=hermes
```

## 存储模型

Hermes run artifact 建议保持三层：

```text
runs/<job_id>/
  stdout                 # child raw stdout bytes
  stderr                 # child raw stderr bytes
  acp-requests.jsonl     # AHand -> Hermes ACP requests
  acp-events.jsonl       # Hermes -> AHand ACP responses/notifications
  observations.jsonl     # parser=hermes 聚合后的统一 observations
  parser.json
  result.json
```

语义边界：

| 文件 | 语义 | 是否聚合 |
|---|---|---|
| `stdout` | Hermes child 原始 stdout | 否 |
| `stderr` | Hermes child 原始 stderr | 否 |
| `acp-requests.jsonl` | AHand 发给 Hermes 的 ACP requests | 否 |
| `acp-events.jsonl` | Hermes 回复给 AHand 的 ACP events | 否 |
| `observations.jsonl` | AHand 统一 observation records | 是 |

这样 raw 层仍可用于 debug MCP handshake、ACP protocol、token streaming；observation 层用于 UI、统计、任务审计和跨 runtime 对齐。

## Observation 输出模型

### LLM Message

Hermes `agent_thought_chunk` 聚合为：

```json
{
  "schemaVersion": 1,
  "jobId": "ctl-job-84021",
  "seq": 19,
  "kind": "llm_message",
  "agent": {
    "agentId": "ctl-job-84021:hermes",
    "agentKind": "hermes",
    "agentSessionId": "d56cf528-0730-4b80-9928-a9a0697f1c04"
  },
  "llmResponse": {
    "channel": "thinking",
    "responseText": "完整 thinking 文本..."
  },
  "stream": {
    "sourceKind": "agent_thought_chunk",
    "chunkCount": 2838,
    "startSeq": 19,
    "endSeq": 3050
  }
}
```

Hermes `agent_message_chunk` 聚合为：

```json
{
  "schemaVersion": 1,
  "jobId": "ctl-job-84021",
  "seq": 3358,
  "kind": "llm_message",
  "llmResponse": {
    "channel": "message",
    "responseText": "完整可见回复..."
  },
  "stream": {
    "sourceKind": "agent_message_chunk",
    "chunkCount": 301,
    "startSeq": 3358,
    "endSeq": 3366
  }
}
```

`observations.jsonl` 不再写 Hermes token 级 `llm_call_delta`。如需 token 级回放，读取 `acp-events.jsonl` 或 `stdout`。

### Tool Calls

Hermes ACP `tool_call` 转为：

```json
{
  "kind": "tool_call_start",
  "toolCall": {
    "toolCallId": "tc-...",
    "toolKind": "mcp",
    "toolName": "mcp_capability_hub_youtube_search",
    "input": {
      "q": "home gym equipment review channel"
    },
    "status": "started"
  }
}
```

Hermes ACP `tool_call_update(status=completed)` 转为：

```json
{
  "kind": "tool_call_end",
  "toolCall": {
    "toolCallId": "tc-...",
    "toolKind": "mcp",
    "toolName": "mcp_capability_hub_youtube_search",
    "outputText": "...",
    "status": "completed"
  }
}
```

第一阶段可以不新增 `tool_call_output`。Codex 有 `tool_call_output` 是因为 Codex 原生 JSONL 提供 command output 事件；Hermes ACP 大多数工具以 update 的 content 返回，统一映射为 `tool_call_end.outputText` 即可。

### Plan Updates

Hermes ACP `sessionUpdate=plan` 不应混入 tool events。建议输出：

```json
{
  "kind": "plan_update",
  "plan": {
    "entries": [
      {
        "content": "读取 youtube-bd skill 与参考流程",
        "status": "completed",
        "priority": "medium"
      }
    ]
  }
}
```

如果需要兼容旧 consumer，可以短期继续在 `raw` 中保留原始 plan event，但 normalized kind 应为 `plan_update`。

### Usage / LLM End

Hermes `usage_update` 或 turn end usage 转为：

```json
{
  "kind": "llm_call_end",
  "llmResponse": {
    "stopReason": "end_turn",
    "usage": {
      "inputTokens": 4295018,
      "cachedReadTokens": 3828736,
      "outputTokens": 38532,
      "thoughtTokens": 0,
      "totalTokens": 4333550
    }
  }
}
```

### Session

`session/new` response 转为 `agent_session`：

```json
{
  "kind": "agent_session",
  "agent": {
    "agentKind": "hermes",
    "agentSessionId": "d56cf528-0730-4b80-9928-a9a0697f1c04"
  }
}
```

## 聚合规则

`parser=hermes` 维护 per-job message buffer：

```text
current_channel: thinking | message | null
current_text: string
start_seq: number
end_seq: number
chunk_count: number
started_at_ms: number
ended_at_ms: number
```

收到 ACP chunk 时：

| ACP sessionUpdate | 聚合 channel |
|---|---|
| `agent_thought_chunk` | `thinking` |
| `agent_message_chunk` | `message` |

Flush 边界：

- channel 变化。
- `tool_call` start。
- `tool_call_update` end / failed。
- `plan` update。
- `usage_update`。
- `llm_call_end` / turn end。
- ACP error。
- child process exit。
- parser finalization。

Flush 后输出一条 `llm_message`。空文本 buffer 不输出。

这会把 Hermes 原本数千条 token delta 聚合为少量语义消息，同时保留 thinking 和 message 的顺序。

## 与 Codex 对齐

对齐目标不是让 Hermes 和 Codex 原始协议一致，而是让 parser 之后的 observation kind 一致。

目标 observation kinds：

```text
agent_session
llm_message
tool_call_start
tool_call_end
plan_update
llm_call_end
error
raw
```

Codex 现有 `llm_call_delta` 后续也可以升级为 `llm_message`，但不是 Hermes parser 的阻塞项。短期 consumer 可以同时支持：

```text
llm_message        # 新统一语义层
llm_call_delta     # 旧 Codex 兼容层
```

长期建议 consumer 默认读取 `llm_message`。

## 错误处理

`parser=hermes` 应收紧 error 识别，避免把普通 stderr 日志误判为 provider error。

只有以下来源可以生成 `kind=error`：

- ACP JSON-RPC error object。
- Hermes 明确结构化 provider error。
- child process 非零退出，并且没有 successful turn end。
- parser 自身 parse error。

普通 stderr INFO / WARNING 不应直接变成 provider error。例如以下日志不应被归类为 `provider_rate_limited`：

```text
2026-05-22 20:11:36 [INFO] run_agent: API call #8: model=openai/gpt-5.5 ...
```

如果 stderr 中确有 tool warning，例如 MCP tool 404 或 `openpyxl unavailable`，可以保留为 raw stderr，或输出 `tool_call_end(status=failed)`，但不应覆盖整个 job 的 final result。

## Backward Compatibility

兼容策略：

- `stdout` / `stderr` 原始文件不变。
- `acp-events.jsonl` / `acp-requests.jsonl` 保留。
- `observations.jsonl` schema 仍使用 `AgentObservationRecord` 外层字段。
- 新增 `kind=llm_message` 和 `kind=plan_update`。
- Hermes 不再向 observations 写 token 级 `llm_call_delta`。
- 需要 token 级流式 UI 的 consumer 应读取 raw ACP events，或后续新增单独 `stream_events.jsonl`，不要依赖 observations。

如果担心已有 consumer 依赖 Hermes `llm_call_delta`，可以增加短期开关：

```text
AHAND_HERMES_OBSERVATION_DELTA_COMPAT=1
```

但默认行为应是聚合后的方案 A。

## 验证计划

使用已有两次 run 做回放验证：

Hermes run：

```text
/var/folders/_3/wg5h7ydd6rl_vrwx_w3hldhw0000gn/T/ahand-with-mcp.mUa0TE
```

Codex run：

```text
/var/folders/_3/wg5h7ydd6rl_vrwx_w3hldhw0000gn/T/ahand-with-mcp.RPvrbo
```

验证项：

- Hermes parser 能读取 `acp-events.jsonl` 或 raw stdout 重建 observations。
- Hermes `3139` 条 token delta 聚合为少量 `llm_message`。
- `thinking` 文本完整保留，且 `channel=thinking`。
- 最终可见回复完整保留，且 `channel=message`。
- Tool call 数量和 MCP stdio log 的唯一 call 数基本对齐。
- `plan` event 输出为 `plan_update`。
- `usage_update` 输出为 `llm_call_end`。
- 普通 stderr INFO 不再触发 `provider_rate_limited`。
- `result.json` 不因 parser 误判覆盖 successful turn end。
- Codex / Hermes 在 observation 层可以按 kind 直接统计和比较。

验收标准：

```text
Hermes observations:
  llm_message(channel=thinking) >= 1
  llm_message(channel=message) >= 1
  llm_call_delta == 0
  tool_call_start > 0
  tool_call_end > 0
  plan_update >= 1
  error 不包含普通 API call INFO 日志
```

## 分阶段实施

### Phase 1: Parser 回放实现

- 新增 `parser=hermes`。
- 支持从已保存 `acp-events.jsonl` 回放生成 observations。
- 实现 message buffer 和 flush 边界。
- 实现 `llm_message`、`tool_call_start`、`tool_call_end`、`plan_update`、`llm_call_end`。

### Phase 2: Runtime 接入

- Hermes live run 默认使用 `parser=hermes`，不再使用 `parser=raw`。
- `parser.json` 正确记录 parser 名称和版本。
- live stdout parse 和 artifact replay 输出一致。

### Phase 3: Consumer 对齐

- UI / SDK / debug tools 默认读取 `llm_message`。
- Codex parser 后续可把现有大块 `llm_call_delta` 也归一为 `llm_message`。
- 统计工具按统一 observation kinds 比较 Codex / Hermes。

### Phase 4: Error 语义收紧

- 只从结构化 ACP error 或明确 provider error 生成 job error。
- child process 非零退出时结合 successful turn end 判断 job result。
- 为 stderr warning 增加 raw diagnostics，而不是直接提升为 provider error。
