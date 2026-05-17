# Codex JSONL Result Parser 开发计划

**Goal:** 为 AHand 增加 Codex formatter：stdout 是用户唯一需要处理的数据源；当调用方显式指定 `outputFormat=codex-jsonl` 时，用 Codex formatter 过滤 Codex CLI `--json` 输出的 JSONL 过程事件，并把 stdout 改写为统一维度的 `AgentObservationRecord` JSONL；未指定 formatter 时 stdout 继续保持 raw/兼容输出。这样既能观测 Codex 的 agent 身份、时间、LLM 输入输出、工具调用、错误和最终结果，也不破坏默认调试链路。

**Architecture:** `pipe_stream` runtime 继续负责进程级 stdin/stdout/stderr transport；`inputFormat=text` 负责把 AHand prompt 写成 Codex CLI 需要的 stdin plain text，并配合固定 `codex exec --json ... -` 启动形态；`outputFormat=codex-jsonl` 负责按行读取、JSON decode、把 decoded Codex events 过滤/映射成统一 `AgentObservationRecord`，并把这些 records 作为 caller-facing stdout 发给 `ahandctl`、SDK callback 或 hub SSE。`inputFormat=raw` 和 `outputFormat=raw` 都必须保留，用于普通 pipe_stream 调试和未归一化输出。raw child stdout 仍写入 run artifact，`observations.jsonl` 只是 formatted stdout 的 debug copy。

**Tech Stack:** Rust, serde_json, protobuf/prost, ahandd RunStore, local IPC, hub output stream

**Related Docs:**
- `docs/plans/2026-05-12-result-parser-for-agent-output.md`
- `docs/plans/2026-05-13-agent-formatter-observation-dimensions.md`
- `docs/usage/claude-codex-pipe-stream.md`
- `docs/plans/2026-05-11-ahand-execution-modes-observability.md`

---

## 背景

当前 AHand 已经可以用 `pipe_stream` 启动 Codex：

```bash
printf 'Run tests\n' | cargo run -p ahandctl -- \
  --ipc /tmp/ahand-local-debug.sock \
  exec \
  --execution-mode pipe_stream \
  --result-parser codex-jsonl \
  --cwd "$ARTICLES" \
  --env PATH="$PATH" \
  "$CODEX" -- exec --skip-git-repo-check --json --cd "$ARTICLES" -
```

Codex 会输出 JSONL，每一行是一个独立事件。真实输出示例：

```json
{"type":"thread.started","thread_id":"019e202b-3c86-7753-b018-0348eb9b1feb"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"I’ll inspect the repo scripts first..."}}
{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"/bin/bash -lc 'git status --short'","aggregated_output":"","exit_code":null,"status":"in_progress"}}
{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"/bin/bash -lc 'git status --short'","aggregated_output":"","exit_code":0,"status":"completed"}}
{"type":"turn.completed","usage":{"input_tokens":312593,"cached_input_tokens":283904,"output_tokens":2155,"reasoning_output_tokens":346}}
```

现在 AHand 只把这些行当 stdout 原样透传和保存。下一步需要把它们解析成 AHand 统一观测记录，而不是 Codex 专属日志格式。

## 目标

`codex-jsonl` parser 负责识别 Codex 事件；当 `outputFormat=codex-jsonl` 时，Codex formatter 需要把这些事件归一到这些维度：

- `agent`：Codex agent id、thread id、agent kind、模型信息。
- `time`：AHand observed time、turn/tool start/end 时间。
- `runtime`：AHand job id、execution mode、cwd、tool、args。
- `llm_request`：messages/tools 可用性、从 AHand stdin 推导的 user prompt。
- `llm_response`：assistant message、usage、stop reason、错误状态。
- `tool_call`：command execution 的 start/output/end/failure。
- `raw`：原始 Codex JSON、未知事件、parse error。

这些 observation records 应能用于：

- 本地 debug 查看一次 AHand 执行过程
- run artifact 回放
- hub job 详情页展示过程步骤
- SDK callback / SSE 后续扩展
- 统计 Codex 执行中的工具调用、失败点和 token 使用

同时，Codex 接入需要和 Claude Code、Hermes ACP 对齐到同一组三个开关：

- `executionMode=pipe_stream` 表达进程 transport。
- `inputFormat=text` 把 prompt 转成 Codex stdin plain text；`inputFormat=raw` 保留原始 stdin。
- `outputFormat=codex-jsonl` 把 Codex 原生 stdout JSONL 转成统一 observation JSONL；`outputFormat=raw` 保留原始 stdout。
- `ExecutionMode::PipeStream` 只表示进程 transport，不表达 agent 协议类型。

## 非目标

第一阶段不做：

- 不改变 Codex 启动方式。
- 不改变 `executionMode = pipe_stream`。
- 不把 Codex formatter 设为强制默认输出。
- 不实现 Codex 交互式多轮 attach。
- 不向 Codex stdin 注入权限回复。
- 不要求 Codex JSON schema 完全稳定。
- 不因为 parser 失败改变 job exit code。
- 不移除 stdout/stderr 原始文件。

## Format 开关

为兼容现有调试路径，长期接口使用 `inputFormat` 和 `outputFormat`。旧 `format` 字段废弃；旧 `resultParser` 只作为兼容 parser hint。

建议命名：

```bash
--input-format raw
--input-format text
--output-format raw
--output-format codex-jsonl
```

或者在 SDK / control-plane 中使用：

```ts
{
  executionMode: "pipe_stream",
  inputFormat: "raw" | "text" | "claude-stream-json" | "hermes-acp-json-rpc",
  outputFormat: "raw" | "codex-jsonl" | "claude-stream-json" | "hermes-acp-json-rpc"
}
```

语义：

| 开关 | 行为 |
|---|---|
| `inputFormat=raw` | 不转换输入，保留 caller-provided stdin。 |
| `inputFormat=text` | 把 prompt 作为 plain text 写入 stdin。 |
| `outputFormat=raw` | 默认值。caller-facing stdout 是 child raw stdout。 |
| `outputFormat=codex-jsonl` | caller-facing stdout 是 Codex stdout 转换后的 `AgentObservationRecord` JSONL。 |

兼容规则：

- 不传 `inputFormat` 时默认为 `raw`。
- 不传 `outputFormat` 时默认为 `raw`。
- `raw` 行为不能因为新增 formatter 被破坏。
- `outputFormat=codex-jsonl` 改变 stdout schema，但不改变 Codex 子进程 exit code。
- Codex formatter 失败时可以写 formatter error，但不能影响 raw 输出和 job finish。
- 旧 SDK / 旧 hub 不认识该开关时，应自然降级为 `raw`。
- 新 SDK 调旧 hub 时，如果用户显式请求 `inputFormat` 或 `outputFormat`，需要返回明确 capability error 或降级提示，不能静默假装成功。
- `outputFormat=codex-jsonl` 必须和 Codex JSONL stdout 匹配；如果旧 `resultParser` 不是 `codex-jsonl`，hub/daemon 应拒绝或降级为 `raw`，避免错误 formatter 误解析。

## 输入契约

Codex 接入分成三层契约：process transport、stdin format、stdout format。

### AHand 输入格式

建议和 Hermes / Claude Code 共用长期 format fields：

```text
executionMode = "pipe_stream"
inputFormat = "text"
outputFormat = "codex-jsonl"
executable = "/absolute/path/to/codex"
prompt = "Run tests and explain failures."
agentModel = optional
agentSessionId = optional thread id for resume
cwd = "/repo"
env = explicit PATH/HOME/auth environment
```

兼容当前实现时，也可以继续由调用方手写 `exec` 命令并通过 stdin 传 prompt。后续 `inputFormat=text` 应把上述统一输入自动转成：

```text
codex exec --skip-git-repo-check --json --cd <cwd> -
codex exec resume --skip-git-repo-check <thread_id> --json -
```

并把 `prompt` 写入 child stdin 后关闭 stdin。`inputFormat=raw` 必须继续保留，表示 AHand 不转换 stdin。

### Codex 原生输出输入

Codex parser 只解析 stdout。

```text
child stdout bytes
-> line buffer
-> one JSON object per newline
-> codex-jsonl parser
```

stderr 继续作为原始错误流保存。除非后续确认 Codex 会把结构化 JSON 写到 stderr，否则第一阶段不解析 stderr。

parser 必须支持 chunk 边界不等于行边界：

```text
stdout chunk 1: {"type":"item.sta
stdout chunk 2: rted","item":...}\n{"type":"item.completed"
stdout chunk 3: ,...}\n
```

因此实现上需要 per-job line buffer，不能直接对每个 stdout chunk 做 JSON parse。

## 输出模型

Codex parser / formatter 分两层输出，但用户只消费 stdout。

### Raw 输出

默认 `outputFormat=raw` 时保持当前兼容行为：

```text
caller-facing stdout = child raw stdout
```

run artifact 中的 `stdout` 文件也保存 child raw stdout。

### Codex Formatter 输出

只有当 `outputFormat=codex-jsonl` 时，Codex formatter 才启用。此时 caller-facing stdout 是统一的 `AgentObservationRecord` JSONL：

```json
{
  "schemaVersion": 1,
  "jobId": "ctl-job-376799",
  "seq": 12,
  "kind": "tool_call_end",
  "agent": {
    "agentId": "ctl-job-376799:codex",
    "agentKind": "codex",
    "agentThreadId": "019e202b-3c86-7753-b018-0348eb9b1feb"
  },
  "time": {
    "observedAtMs": 1778600000000
  },
  "runtime": {
    "jobId": "ctl-job-376799",
    "executionMode": "pipe_stream",
    "resultParser": "codex-jsonl",
    "cwd": "/home/mew/workspace/articles",
    "tool": "codex"
  },
  "toolCall": {
    "toolCallId": "item_1",
    "toolName": "/bin/bash -lc 'git status --short'",
    "toolKind": "shell",
    "input": {
      "command": "/bin/bash -lc 'git status --short'"
    },
    "exitCode": 0,
    "status": "completed"
  },
  "raw": {
    "source": "stdout",
    "parser": "codex-jsonl",
    "parserVersion": 1,
    "json": {}
  }
}
```

字段说明：

| 字段 | 含义 |
|---|---|
| `schemaVersion` | 统一观测记录 schema 版本 |
| `jobId` | AHand job id |
| `seq` | parser 内单调递增序号 |
| `kind` | 统一事件类型，例如 `llm_call_start` / `tool_call_end` |
| `agent` | agent 身份、Codex thread id、模型信息 |
| `time` | AHand 观测时间和可推导的 start/end/duration |
| `runtime` | AHand job/process 上下文 |
| `llmRequest` | 本次 LLM 输入，能观测则填，不能观测则标记 unavailable |
| `llmResponse` | 模型输出、usage、stop reason、错误状态 |
| `toolCall` | 工具调用过程 |
| `raw` | 原始 Codex JSON / line / parse error |

## 事件类型

`outputFormat=codex-jsonl` 时，第一阶段支持这些统一 `kind`：

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

后续如需要 UI 更细粒度展示，可以再拆：

```text
file_read
file_changed
plan_updated
```

第一阶段先统一落到 `tool_call_*`，避免和 Codex 内部 item type 过度绑定。

## Codex 事件映射

以下映射只对 `outputFormat=codex-jsonl` 生效。`outputFormat=raw` 模式继续使用原有 raw/parser event 结构。

| Codex `type` | 条件 | AHand `kind` | 提取维度 |
|---|---|---|---|
| `thread.started` | 有 `thread_id` | `agent_session` | `agent.agentThreadId` |
| `turn.started` | - | `llm_call_start` | `time.startedAtMs` |
| `turn.completed` | - | `llm_call_end` | `llmResponse.usage` |
| `item.started` | `item.type = command_execution` | `tool_call_start` | `toolCall.toolCallId`, `toolCall.input.command` |
| `item.completed` | `item.type = command_execution`, `exit_code = 0` | `tool_call_output`, `tool_call_end` | `toolCall.outputText`, `toolCall.exitCode` |
| `item.completed` | `item.type = command_execution`, `exit_code != 0` | `tool_call_output`, `tool_call_end` | `toolCall.status=failed`, `toolCall.exitCode` |
| `item.completed` | `item.type = agent_message` | `llm_call_delta` | `llmResponse.responseText` |
| `error` | 有 `message` | `error` | `error.message`, `llmResponse.isError=true` |
| unknown | 任意未知结构 | `raw` | full JSON |

## LLM Request 处理

以下规则只对 `outputFormat=codex-jsonl` 生效。

Codex 当前 `exec --json` 输出通常不包含完整的 LLM request payload，因此 formatter 必须明确区分“未观测到”和“空数组”。

默认记录：

```json
{
  "kind": "llm_call_start",
  "llmRequest": {
    "model": {
      "provider": "openai",
      "id": "unknown"
    },
    "messages": [],
    "tools": [],
    "availability": {
      "messages": "unobserved",
      "tools": "unobserved"
    }
  }
}
```

如果 prompt 是 AHand 通过 stdin 注入的，后续可以在 daemon 侧保留一份 request input snapshot，并在 `llm_call_start` 中补一个 inferred user message：

```json
{
  "llmRequest": {
    "messages": [
      {
        "role": "user",
        "content": "Run tests"
      }
    ],
    "tools": [],
    "availability": {
      "messages": "inferred_from_ahand_stdin",
      "tools": "unobserved"
    }
  }
}
```

规则：

- 不从 Codex stdout 中没有的信息硬造完整 messages/tools。
- 如果只知道用户 prompt，就只记录 user message，并标记来源。
- 如果后续 Codex 输出 tool definitions，再填 `tools`。
- model id 如果无法从 Codex event 识别，先写 `unknown`，不要从环境猜测。

### 示例: thread.started

输入：

```json
{"type":"thread.started","thread_id":"019e202b-3c86-7753-b018-0348eb9b1feb"}
```

输出：

```json
{
  "schemaVersion": 1,
  "kind": "agent_session",
  "agent": {
    "agentId": "ctl-job-376799:codex",
    "agentKind": "codex",
    "agentThreadId": "019e202b-3c86-7753-b018-0348eb9b1feb"
  },
  "raw": {
    "source": "stdout",
    "parser": "codex-jsonl",
    "parserVersion": 1,
    "json": {
      "type": "thread.started",
      "thread_id": "019e202b-3c86-7753-b018-0348eb9b1feb"
    }
  }
}
```

同时更新 `parser.json`：

```json
{
  "parser": "codex-jsonl",
  "parser_version": 1,
  "status": "running",
  "thread_id": "019e202b-3c86-7753-b018-0348eb9b1feb",
  "events": 1,
  "parse_errors": 0
}
```

### 示例: assistant message

输入：

```json
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"I’ll inspect the repo scripts first..."}}
```

输出：

```json
{
  "schemaVersion": 1,
  "kind": "llm_call_delta",
  "llmResponse": {
    "responseText": "I’ll inspect the repo scripts first..."
  },
  "raw": {
    "source": "stdout",
    "parser": "codex-jsonl",
    "parserVersion": 1
  }
}
```

### 示例: command started

输入：

```json
{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"/bin/bash -lc 'git status --short'","aggregated_output":"","exit_code":null,"status":"in_progress"}}
```

输出：

```json
{
  "schemaVersion": 1,
  "kind": "tool_call_start",
  "toolCall": {
    "toolCallId": "item_1",
    "toolName": "/bin/bash -lc 'git status --short'",
    "toolKind": "shell",
    "input": {
      "command": "/bin/bash -lc 'git status --short'"
    },
    "status": "in_progress"
  }
}
```

### 示例: command completed

输入：

```json
{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"/bin/bash -lc 'git status --short'","aggregated_output":"","exit_code":0,"status":"completed"}}
```

输出：

```json
{
  "schemaVersion": 1,
  "kind": "tool_call_end",
  "toolCall": {
    "toolCallId": "item_1",
    "toolName": "/bin/bash -lc 'git status --short'",
    "toolKind": "shell",
    "input": {
      "command": "/bin/bash -lc 'git status --short'"
    },
    "exitCode": 0,
    "status": "completed"
  }
}
```

如果 `aggregated_output` 非空，额外输出一个 `tool_call_output`：

```json
{
  "schemaVersion": 1,
  "kind": "tool_call_output",
  "toolCall": {
    "toolCallId": "item_2",
    "toolName": "/bin/bash -lc \"pwd && rg --files\"",
    "toolKind": "shell",
    "outputText": "/home/mew/workspace/articles\npackage.json\nREADME.md\n",
    "status": "completed"
  }
}
```

## 存储

继续保留现有文件：

```text
runs/<job_id>/
  request.json
  parser.json
  stdout
  stderr
  result.json
```

新增：

```text
runs/<job_id>/
  parsed_events.jsonl
  observations.jsonl
```

`stdout` 永远保存 Codex 原始 JSONL。

`parsed_events.jsonl` 保存兼容 parser events。

`observations.jsonl` 只在 `outputFormat=codex-jsonl` 时写入，保存 caller-facing formatted stdout 的 debug copy。

`parsed_events.jsonl` 不建议在 `outputFormat=codex-jsonl` 时改变 envelope；它继续作为 parser-level 兼容输出。用户不需要读取 artifact 文件，正常消费路径是 selected stdout。

`parser.json` 从当前的静态配置文件升级为解析状态文件：

```json
{
  "job_id": "ctl-job-376799",
  "parser": "codex-jsonl",
  "parser_version": 1,
  "outputFormat": "codex-jsonl",
  "formatter": "codex-jsonl",
  "formatter_version": 1,
  "status": "finished",
  "thread_id": "019e202b-3c86-7753-b018-0348eb9b1feb",
  "agent_id": "ctl-job-376799:codex",
  "agent_kind": "codex",
  "events": 28,
  "parse_errors": 0,
  "last_event_kind": "llm_call_end",
  "start_ms": 1778600000000,
  "last_event_ms": 1778600002000,
  "finish_ms": 1778600003000
}
```

## 模块设计

建议新增或整理 daemon 内部模块：

```text
crates/ahandd/src/result_parser/
  mod.rs
  input_format.rs
  output_format.rs
  event.rs
  raw.rs
  codex_jsonl.rs
```

职责：

| 文件 | 职责 |
|---|---|
| `input_format.rs` | `InputFormat`：`raw` / `text` / `claude-stream-json` / `hermes-acp-json-rpc` |
| `output_format.rs` | `OutputFormat`：`raw` / `codex-jsonl` / `claude-stream-json` / `hermes-acp-json-rpc` |
| `mod.rs` | parser / formatter trait、factory、stream dispatch |
| `event.rs` | `AgentObservationRecord`、`ParserState`、`FormatterState`、统一维度类型 |
| `raw.rs` | raw parser |
| `codex_jsonl.rs` | Codex JSONL line buffer |
| `codex_formatter.rs` | Codex event 到 AgentObservationRecord 的过滤和映射 |

建议 input format trait：

```rust
trait InputFormatHandler {
    fn input_format(&self) -> &'static str;
    async fn write_initial_input(&self, stdin: ChildStdin, input: AgentInput) -> Result<()>;
}
```

建议 trait：

```rust
trait ResultParser {
    fn parser_name(&self) -> &'static str;
    fn push_stdout(&mut self, chunk: &[u8]) -> Vec<DecodedParserEvent>;
    fn push_stderr(&mut self, chunk: &[u8]) -> Vec<DecodedParserEvent>;
    fn finish(&mut self) -> Vec<DecodedParserEvent>;
}
```

formatter 独立于 parser：

```rust
trait AgentFormatter {
    fn formatter_name(&self) -> &'static str;
    fn push_event(&mut self, event: DecodedParserEvent) -> Vec<AgentObservationRecord>;
    fn finish(&mut self) -> Vec<AgentObservationRecord>;
}
```

第一阶段可以不把 trait 暴露到 hub，只在 daemon 内部使用。

## 执行流程

```text
JobRequest(result_parser = codex-jsonl)
-> ahandd run_job_stream
-> spawn codex child
-> stdout chunk
-> RunStore append stdout
-> parser.push_stdout(chunk)
-> RunStore append parsed_events.jsonl
-> if outputFormat=codex-jsonl: codex_formatter.push_event(decoded_event)
-> if outputFormat=codex-jsonl: RunStore append observations.jsonl
-> if outputFormat=codex-jsonl: emit observation JSONL as JobEvent stdout
-> if outputFormat=raw: emit raw chunk as JobEvent stdout
-> update parser.json
-> child exit
-> parser.finish()
-> RunStore write result.json
-> update parser.json status=finished
```

关键约束：

1. raw stdout/stderr artifact 写入优先，保证可回放。
2. caller-facing stdout 由 `outputFormat` 决定：`raw` 转发 raw chunk，`codex-jsonl` 转发 formatter records。
3. parser 解析失败在 `outputFormat=codex-jsonl` 时作为 `parse_error` record 输出到 stdout，不能阻断 job。
4. parser 不拥有 child process。
5. parser 不影响 job exit code。
6. parser / formatter 的文件写入失败不能导致 Codex 被 kill，但需要写 daemon warn log。

## 兼容策略

- `result_parser` 缺失时默认为 `raw`。
- `outputFormat` 缺失时默认为 `raw`，不启用 agent formatter。
- `result_parser = raw` 时行为和旧版本一致。
- 旧 run 目录没有 `parsed_events.jsonl` 时，读取层展示 raw stdout/stderr。
- 旧 run 目录没有 `observations.jsonl` 时，读取层展示 raw 或 parser events。
- Codex 新版本增加字段时，未知字段保留在 `raw.json`。
- Codex 新版本增加事件类型时，先输出 `raw`，不报错。
- 单行 JSON parse 失败时输出 `parse_error`，并保留原始 line。
- 最后一行没有 newline 时，`finish()` 尝试解析剩余 buffer。

## 开发阶段

### Phase 0: Codex Input/Output Format Boundary

- [ ] 定义 `inputFormat` / `outputFormat` typed fields。
- [ ] 保留 `inputFormat=raw` 和 `outputFormat=raw`。
- [ ] 实现 `inputFormat=text`：构造 `codex exec --json ... -` 并写 prompt stdin。
- [ ] 实现 `outputFormat=codex-jsonl`：解析 Codex JSONL 并输出 observation JSONL。
- [ ] 支持 resume：`codex exec resume <thread_id> --json -`。
- [ ] 将 `prompt` 写入 stdin 并关闭 stdin。
- [ ] 明确 `ExecutionMode::PipeStream` 只是 transport，不作为 Codex/Hermes/Claude 的协议选择。

验收：

- SDK 用户不需要手写 Codex CLI 参数即可启动 Codex。
- 旧 `exec --execution-mode pipe_stream ... codex -- exec ... -` 路径继续可用。
- `inputFormat=text` 不改变 `outputFormat=codex-jsonl` 的 stdout schema。

### Phase 1: Parser 基础设施

- 新增 `AgentObservationRecord` 数据结构。
- 新增 `AgentIdentity`、`ObservationTime`、`RuntimeContext`、`NormalizedLlmRequest`、`NormalizedLlmResponse`、`NormalizedToolCall`。
- 新增 `InputFormat`：`raw` / `text` / `claude-stream-json` / `hermes-acp-json-rpc`。
- 新增 `OutputFormat`：`raw` / `codex-jsonl` / `claude-stream-json` / `hermes-acp-json-rpc`。
- 在 CLI / SDK / hub / daemon 透传 `outputFormat`，默认 `raw`。
- 新增 `ResultParser` trait。
- 新增 `AgentFormatter` trait。
- 新增 parser factory：`raw` / `codex-jsonl`。
- 新增 formatter factory：`raw` / `codex-jsonl` / `claude-stream-json` / `hermes-acp-json-rpc`。
- `RunStore` 支持 append `parsed_events.jsonl`。
- `RunStore` 支持 append `observations.jsonl`。
- `RunStore` 支持更新 `parser.json` 状态。

### Phase 2: Codex JSONL 行解析

- 实现 stdout line buffer。
- 每行 `serde_json::from_slice`。
- 支持 chunk 拆行。
- 支持 `finish()` 解析剩余 buffer。
- 解析失败写 `parse_error`。

### Phase 3: Raw format 行为保持

- 保持当前 raw stdout/stderr。
- 保持当前 `parser.json`。
- 如果已有 `parsed_events.jsonl` 兼容结构，则继续输出。
- `outputFormat=raw` 时，不输出 `AgentObservationRecord`。

### Phase 4: Codex formatter 映射到统一维度

- 仅在 `outputFormat=codex-jsonl` 时启用。
- `thread.started` -> `agent_session`。
- `turn.started` -> `llm_call_start`。
- `turn.completed` -> `llm_call_end`。
- `item.completed agent_message` -> `llm_call_delta`。
- `item.started command_execution` -> `tool_call_start`。
- `item.completed command_execution` -> `tool_call_output` + `tool_call_end`。
- `error` -> `error`。
- 未知事件写 `raw`。

### Phase 5: LLM request 可用性标记

- 仅在 `outputFormat=codex-jsonl` 时启用。
- `llmRequest.messages` 默认不伪造，标记 `availability.messages=unobserved`。
- `llmRequest.tools` 默认不伪造，标记 `availability.tools=unobserved`。
- 如果 AHand 保存了 stdin prompt snapshot，则补 inferred user message。
- model provider 可填 `openai`，model id 无法识别时填 `unknown`。

### Phase 6: 本地可观测验证

- 用 local sidecar 跑 Codex。
- 验证 run artifact `stdout` 保留完整 raw Codex JSONL。
- 验证默认不传 `--output-format` 时 caller-facing stdout 是 raw 输出。
- 验证传 `--output-format codex-jsonl` 时 caller-facing stdout 是统一 `AgentObservationRecord`。
- 验证 `observations.jsonl` 是 formatted stdout 的 debug copy。
- 验证 `parser.json` 有 thread id、事件数、parse error 数。
- 验证 Codex 失败时仍能写 `error` 和 `result.json`。

### Phase 7: 测试

- 增加 fixture：真实 Codex 成功输出。
- 增加 fixture：command failed。
- 增加 fixture：error/reconnect。
- 增加 fixture：unknown event。
- 增加 fixture：chunk 被拆成半行。
- 增加 fixture：最后一行无 newline。
- 增加 fixture：无 messages/tools 时 availability 标记正确。
- 增加 fixture：AHand stdin prompt 可推导为 user message。
- 增加 fixture：默认 `outputFormat=raw` 不输出 observation envelope。
- 增加 fixture：`outputFormat=codex-jsonl` 输出统一 observation record。

### Phase 8: Hub / SDK 输出

- hub output stream 后续增加 observation event 类型。
- SDK 后续增加 `onObservation` 或 `onParsedEvent` callback。
- dashboard 后续用 observation record 渲染 Codex 执行步骤、LLM 调用和工具调用。

## 验收

- `pipe_stream + codex-jsonl` 仍能正常启动 Codex。
- run artifact `stdout` 文件内容和当前版本一致，不被 parser 修改。
- 不传 `--output-format` 时保持 raw 输出。
- 传 `--output-format codex-jsonl` 时使用 Codex formatter，并通过 caller-facing stdout 输出统一 `AgentObservationRecord`。
- `outputFormat=codex-jsonl` 模式下，每条 observation 都包含：
  - `agent`
  - `time`
  - `runtime`
  - `raw`
- `outputFormat=codex-jsonl` 模式下，observation 至少包含：
  - `agent_session`
  - `llm_call_start`
  - `llm_call_delta`
  - `tool_call_start`
  - `tool_call_end`
  - `llm_call_end`
- `llm_call_start` 明确记录 `messages/tools` 是否可用。
- 如果 AHand 保存 stdin prompt，能把 prompt 映射为 inferred user message。
- `parser.json` 记录：
  - parser 名称
  - parser version
  - status
  - thread id
  - agent id
  - events count
  - parse errors count
- parser 遇到未知事件不会失败。
- parser 遇到坏 JSON 不会影响 job exit code。
- 不传 `--result-parser` 时旧行为不变。

## Debug 命令

启动 local sidecar：

```bash
RUST_LOG=info cargo run -p ahandd -- \
  --mode local \
  --debug-ipc \
  --ipc-socket /tmp/ahand-local-debug.sock \
  --data-dir /tmp/ahand-local-debug-data
```

运行 Codex：

```bash
CODEX=$(command -v codex)
ARTICLES=/home/mew/workspace/articles

printf 'Run tests\n' | cargo run -p ahandctl -- \
  --ipc /tmp/ahand-local-debug.sock \
  exec \
  --execution-mode pipe_stream \
  --result-parser codex-jsonl \
  --cwd "$ARTICLES" \
  --env PATH="$PATH" \
  "$CODEX" -- exec --skip-git-repo-check --json --cd "$ARTICLES" -
```

运行 Codex 并启用 Codex formatter：

```bash
printf 'Run tests\n' | cargo run -p ahandctl -- \
  --ipc /tmp/ahand-local-debug.sock \
  exec \
  --execution-mode pipe_stream \
  --result-parser codex-jsonl \
  --output-format codex-jsonl \
  --cwd "$ARTICLES" \
  --env PATH="$PATH" \
  "$CODEX" -- exec --skip-git-repo-check --json --cd "$ARTICLES" -
```

查看结果：

```bash
LATEST=$(ls -td /tmp/ahand-local-debug-data/runs/* | head -1)

cat "$LATEST/parser.json"
sed -n '1,120p' "$LATEST/stdout"
sed -n '1,120p' "$LATEST/parsed_events.jsonl"
sed -n '1,120p' "$LATEST/observations.jsonl"
cat "$LATEST/result.json"
```
