# Agent Formatter 统一观测维度计划

**Goal:** 让 AHand 后续所有 agent formatter / result parser 都输出同一套观测维度，保证 Codex、Claude Code 以及未来 agent CLI 的中间输入、输出、工具过程和基础配置都能被统一记录、查询和展示。

**Architecture:** 不同 formatter 负责解析各自 agent 的原始 stdout/stderr/log；解析结果统一写成 AHand normalized observation records。所有 formatter 必须保留 raw fallback，同时尽量抽取 agent id、时间、agent 配置、LLM 输入、LLM 输出、工具调用和 usage 等维度。

**Related Docs:**
- `docs/plans/2026-05-12-result-parser-for-agent-output.md`
- `docs/plans/2026-05-13-codex-jsonl-result-parser.md`
- `docs/plans/2026-05-11-ahand-execution-modes-observability.md`

---

## 背景

AHand 当前已经能通过 `pipe_stream` 启动 Codex / Claude Code，并保存原始 stdout/stderr。

下一步 parser / formatter 不能只做“展示日志”，而是要把 agent 过程数据规整成稳定结构。尤其是 LLM 调用的中间输入输出，需要能回答：

- 是哪个 agent 发起的？
- 什么时候发起的？
- 使用了什么模型和基础配置？
- 这次真正送进模型的 messages 是什么？
- 这次模型可见的 tools 是什么？
- 模型返回了什么 response？
- 这次调用产生了哪些 usage / stop reason / 错误？

因此所有 formatter 都应该面向同一套观测维度，而不是每个 agent 各自输出一套不可比的数据。

## 统一维度

所有 formatter 最终都尽量归一到这些维度：

| 维度 | 说明 | 必填 |
|---|---|---|
| `agent` | agent 身份和基础配置 | 是 |
| `time` | 事件时间、开始时间、结束时间、持续时间 | 是 |
| `llm_request` | 模型调用输入，包括 messages 和 tools | 条件必填 |
| `llm_response` | 模型输出，包括 response、usage、stop reason | 条件必填 |
| `tool_call` | agent 发起的工具调用过程 | 条件必填 |
| `runtime` | AHand job / process / execution mode 信息 | 是 |
| `raw` | 原始事件和无法识别的数据 | 是 |

其中最核心的“中间输入输出”可以理解为：

```ts
{
  input: {
    messages: ConversationEntry[];
    tools: ToolDefinition[];
  };
  output: {
    response?: ConversationEntry;
  };
}
```

完整记录还需要附带 `systemPrompt`、`contextInjections`、`model`、`usage`、`stopReason` 等元信息。

## 标准记录模型

建议新增统一结构，先落地为 JSONL，不急于进 protobuf：

```ts
type AgentObservationRecord = {
  schemaVersion: 1;
  jobId: string;
  seq: number;
  kind:
    | "agent_session"
    | "llm_call_start"
    | "llm_call_delta"
    | "llm_call_end"
    | "tool_call_start"
    | "tool_call_output"
    | "tool_call_end"
    | "error"
    | "raw"
    | "parse_error";

  agent: AgentIdentity;
  time: ObservationTime;
  runtime: RuntimeContext;

  llmRequest?: NormalizedLlmRequest;
  llmResponse?: NormalizedLlmResponse;
  toolCall?: NormalizedToolCall;
  error?: NormalizedError;

  raw: RawObservation;
};
```

### agent

每条记录都必须带 agent 维度：

```ts
type AgentIdentity = {
  agentId: string;
  agentKind: "codex" | "claude-code" | "unknown";
  agentSessionId?: string;
  agentThreadId?: string;
  agentVersion?: string;
  model?: {
    provider: string;
    id: string;
  };
  thinkingLevel?: string;
  permissionMode?: string;
};
```

规则：

- `agentId` 是 AHand 侧稳定 id，可以先用 `{job_id}:{agent_kind}`。
- Codex 的 `thread_id` 进入 `agentThreadId`。
- Claude Code 的 `session_id` 进入 `agentSessionId`。
- formatter 不能拿到的字段留空，不伪造。

### time

每条记录都必须带时间维度：

```ts
type ObservationTime = {
  observedAtMs: number;
  startedAtMs?: number;
  finishedAtMs?: number;
  durationMs?: number;
};
```

规则：

- `observedAtMs` 是 AHand 解析到该事件的时间。
- 如果原始事件自带时间，放到 `raw.originalTimestamp` 或 metadata，不覆盖 AHand 观测时间。
- 对 start/end 类事件，formatter 可以补 `startedAtMs` / `finishedAtMs`。

### runtime

每条记录都必须带 AHand runtime 维度：

```ts
type RuntimeContext = {
  jobId: string;
  executionMode: "batch" | "pty" | "pipe_stream";
  resultParser: string;
  cwd?: string;
  tool?: string;
  args?: string[];
  exitCode?: number;
};
```

规则：

- 这些字段来自 `JobRequest` 和 `JobFinished`。
- formatter 不直接决定 job 成败，只记录它观察到的过程。

## LLM Request

LLM request 是所有 formatter 的核心目标之一：

```ts
type NormalizedLlmRequest = {
  model: {
    provider: string;
    id: string;
  };
  thinkingLevel?: string;
  systemPrompt?: string;
  messages: ConversationEntry[];
  tools: ToolDefinition[];
  contextInjections?: LlmCallContextInjection[];
  componentSnapshots?: ComponentSnapshot[];
};
```

语义：

- `messages` 是这次真正送进模型的上下文消息数组。
- `tools` 是这次模型可见或可调用的工具定义数组。
- `systemPrompt`、`contextInjections`、`componentSnapshots` 是完整保存时的扩展上下文。

兼容规则：

- 如果 Codex / Claude 原始输出没有暴露完整 messages/tools，则不要伪造。
- 可以记录 `messagesAvailable=false`、`toolsAvailable=false` 到 metadata。
- 如果只能从 prompt/stdin 推导用户消息，可以标记 `source="inferred_from_stdin"`。
- formatter 要区分“确实为空”和“未观测到”。

## LLM Response

```ts
type NormalizedLlmResponse = {
  response?: ConversationEntry;
  responseText?: string;
  usage?: {
    inputTokens?: number;
    cachedInputTokens?: number;
    outputTokens?: number;
    reasoningOutputTokens?: number;
    totalTokens?: number;
  };
  stopReason?: string;
  isError?: boolean;
};
```

规则：

- Codex `agent_message` 可以映射为 `responseText` 或 assistant `ConversationEntry`。
- Claude Code `assistant` message 可以映射为 `response`。
- `turn.completed.usage`、Claude `result.usage` 映射到 `usage`。
- 失败型 result 仍然写 `llmResponse.isError=true`，不要丢。

## Tool Call

```ts
type NormalizedToolCall = {
  toolCallId: string;
  toolName: string;
  toolKind?: "shell" | "file" | "search" | "web" | "mcp" | "unknown";
  input?: unknown;
  outputText?: string;
  exitCode?: number;
  status: "started" | "completed" | "failed";
};
```

Codex 映射：

- `item.type = command_execution`
- `item.id` -> `toolCallId`
- `command` -> `toolName` 或 `input.command`
- `aggregated_output` -> `outputText`
- `exit_code` -> `exitCode`

Claude Code 映射：

- `tool_use` / `tool_result` 事件映射到 `tool_call_*`。
- 权限拒绝或工具失败映射为 `status=failed`。

## Raw Fallback

所有 formatter 必须保存 raw：

```ts
type RawObservation = {
  source: "stdout" | "stderr" | "file" | "ipc" | "parser";
  parser: string;
  parserVersion: number;
  line?: string;
  json?: unknown;
  parseError?: string;
};
```

规则：

- 原始 stdout/stderr 文件永远保留。
- formatter 识别不了的事件写 `kind="raw"`。
- JSON parse 失败写 `kind="parse_error"`。
- raw fallback 不能影响 job exit code。

## 文件落地

每个 run 目录建议包含：

```text
runs/<job_id>/
  request.json
  stdout
  stderr
  result.json
  parser.json
  parsed_events.jsonl
  llm_calls.jsonl
```

职责：

| 文件 | 内容 |
|---|---|
| `stdout` | 原始 stdout |
| `stderr` | 原始 stderr |
| `parsed_events.jsonl` | 所有 normalized observation records |
| `llm_calls.jsonl` | 可选，专门保存 LLM call start/end 聚合记录 |
| `parser.json` | parser 状态、事件数、错误数、agent/session/thread id |

第一阶段可以只写 `parsed_events.jsonl`，`llm_calls.jsonl` 后续再由聚合器生成。

## Formatter 职责边界

formatter 负责：

- 解析原始事件。
- 提取统一维度。
- 标记字段来源。
- 输出 normalized records。
- 记录 parse error。

formatter 不负责：

- 改变子进程生命周期。
- 改变 stdout/stderr 原始流。
- 判断 job 是否成功。
- 补全没有观测到的完整 prompt。
- 直接调用模型或工具。

## Codex Formatter 要求

Codex JSONL formatter 至少输出：

- `agent_session`：来自 `thread.started`
- `llm_call_start`：来自 `turn.started`
- `llm_call_delta`：来自 `item.completed agent_message`
- `tool_call_start`：来自 `item.started command_execution`
- `tool_call_output`：来自 `item.completed command_execution aggregated_output`
- `tool_call_end`：来自 `item.completed command_execution`
- `llm_call_end`：来自 `turn.completed`
- `error`：来自 `error`

Codex 当前 `--json` 输出通常不暴露完整 `messages` 和 `tools`。因此：

- `llmRequest.messages` 默认标记为未观测。
- 如果 prompt 由 AHand stdin 注入，后续可从 AHand request sidecar 记录中补一个 user message，并标记 `source="ahand_stdin"`.
- `llmRequest.tools` 默认标记为未观测，除非 Codex 后续输出 tool definition。

## Claude Code Formatter 要求

Claude Code stream-json formatter 至少输出：

- `agent_session`：来自 `system.init.session_id`
- `llm_call_start`：来自 init 或第一条 assistant 前的 turn 边界
- `llm_call_delta` / `llm_call_end`：来自 assistant message / result
- `tool_call_start` / `tool_call_output` / `tool_call_end`：来自 tool_use / tool_result
- `error`：来自 assistant error 或 result is_error

Claude Code 如果暴露 system/model/permissionMode/tools，需要填入 `agent` 和 `llmRequest` 对应字段。

## 兼容策略

- 所有字段新增，不改变现有 stdout/stderr。
- formatter 缺字段时留空并记录 availability，不写假数据。
- parser 失败不影响 job exit code。
- 旧 run 没有 `parsed_events.jsonl` 时仍可只看 raw。
- 不同 agent 的 formatter 可以逐步补齐维度。
- UI 和 SDK 应允许字段部分缺失。

## 开发阶段

### Phase 1: 统一 schema

- 定义 `AgentObservationRecord`。
- 定义 `AgentIdentity`、`RuntimeContext`、`NormalizedLlmRequest`、`NormalizedLlmResponse`、`NormalizedToolCall`。
- RunStore 支持 append `parsed_events.jsonl`。

### Phase 2: Codex formatter

- 把 Codex JSONL 映射到统一维度。
- 对 `messages/tools` 做 availability 标记。
- 保存 `thread_id`、usage、command execution。

### Phase 3: Claude Code formatter

- 把 Claude stream-json 映射到统一维度。
- 保存 `session_id`、model、permissionMode、assistant response、tool events。

### Phase 4: LLM call 聚合

- 从 `llm_call_start` / delta / end 聚合出 `llm_calls.jsonl`。
- 最小结构包含：

```ts
{
  agentId: string;
  startedAtMs: number;
  finishedAtMs?: number;
  model: { provider: string; id: string };
  messages: ConversationEntry[];
  tools: ToolDefinition[];
  response?: ConversationEntry;
}
```

### Phase 5: Hub / SDK / UI

- hub SSE 增加 normalized observation event。
- SDK 增加 `onObservation` 或 `onParsedEvent` callback。
- dashboard job 页面展示 agent、LLM call、tool call、raw stream。

## 验收

- Codex 和 Claude formatter 都输出同一 record envelope。
- 每条 record 都有 `agent`、`time`、`runtime`、`raw`。
- 能从 records 中还原一次 agent 执行的工具步骤。
- 能从 records 中看到 LLM 输入是否可用、输出是什么。
- 缺失 messages/tools 时有明确 availability 标记。
- raw stdout/stderr 与当前行为一致。
- formatter 错误不会导致 job 失败。
