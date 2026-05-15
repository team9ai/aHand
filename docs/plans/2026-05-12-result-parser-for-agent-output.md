# AHand Result Parser 能力计划

## 背景

当前 AHand 已支持通过 `executionMode = pipe_stream` 启动 Codex、Claude Code 这类 CLI，并把 child process 的 stdin/stdout/stderr 作为原始字节流转发和持久化。

这已经足够完成第一阶段目标：

- AHand 能启动 Codex / Claude Code。
- 调用方能看到原始 stdout/stderr。
- 本地 sidecar 能用于调试一次执行结果。

但这还不是完整的 agent 结果理解能力。Codex 和 Claude Code 的输出格式不同：

- Codex 通常输出 JSON lines。
- Claude Code `--output-format stream-json` 输出 stream-json。
- 两者都可能包含文本增量、工具调用、错误、session/thread id、权限请求或最终结果。

因此后续应新增 **result parser**，专门负责把原始进程输出解析成结构化过程数据。

## 设计边界

保持现有执行接口不变：

```text
executionMode = batch | pty | pipe_stream
```

不新增 `transport` 概念。`executionMode` 已经是协议、SDK、hub、daemon 之间的稳定字段，继续作为 AHand 和 child process 的通信/执行方式。

新增概念只应该是：

```text
resultParser = raw | codex-jsonl | claude-stream-json
```

职责划分：

| 概念 | 作用 | 当前状态 |
|---|---|---|
| `executionMode` | AHand 如何 attach 子进程 | 已实现 |
| `tool + args` | 实际启动哪个 CLI | 已实现 |
| `resultParser` | 如何解释 stdout/stderr | Phase 1 已透传；具体解析待开发 |

## 当前实现状态

已实现：

- proto `JobRequest.result_parser` 字段。
- TypeScript proto 生成。
- SDK `CloudClient.spawn({ resultParser })` 透传。
- hub control-plane 接收 `resultParser`，校验 `raw` / `codex-jsonl` / `claude-stream-json` 并转发给 daemon。
- `ahandctl exec --result-parser <parser>`。
- daemon RunStore 在 `request.json` 中记录 `result_parser`。
- daemon RunStore 写入 `parser.json`，记录 parser 名称、版本、状态和错误计数。

未实现：

- Codex JSON lines 解析。
- Claude Code stream-json 解析。
- parsed events 流式输出。
- `parsed_events.jsonl`。
- SDK parsed event callbacks。

## 目标

新增 result parser 后，AHand 应能从原始输出中产生结构化过程数据：

- assistant 文本增量
- tool start / tool output / tool finish
- permission request
- error
- final result
- Codex thread id
- Claude session id
- raw stdout/stderr fallback

这些数据应能用于：

- 本地调试展示
- run store 持久化
- hub SSE / webhook / SDK 回调
- 后续 UI 或平台消息渲染

## 非目标

第一阶段不做以下事情：

- 不改变 `executionMode` 字段。
- 不引入 `transport` 参数。
- 不要求 Codex / Claude Code 长驻会话。
- 不实现完整权限交互闭环。
- 不让 parser 影响 child process 生命周期。
- 不阻塞 raw stdout/stderr 原始观测。

## 建议接口

### SDK / control-plane

在 `JobRequest` 上新增可选字段：

```text
result_parser = "raw" | "codex-jsonl" | "claude-stream-json"
```

兼容策略：

- 旧 SDK 不传该字段时默认为 `raw`。
- 旧 hub 收到新 SDK 请求时，如果不认识该字段，应忽略或由 SDK 降级。
- 新 daemon 收到旧 hub 请求时默认为 `raw`。

### ahandctl

保留当前可用命令：

```bash
ahandctl --ipc <socket> exec \
  --execution-mode pipe_stream \
  --cwd <work_dir> \
  codex -- exec --skip-git-repo-check --json --cd <work_dir> -
```

后续只新增 parser 参数：

```bash
ahandctl --ipc <socket> exec \
  --execution-mode pipe_stream \
  --result-parser codex-jsonl \
  --cwd <work_dir> \
  codex -- exec --skip-git-repo-check --json --cd <work_dir> -
```

Claude Code：

```bash
ahandctl --ipc <socket> exec \
  --execution-mode pipe_stream \
  --result-parser claude-stream-json \
  --cwd <work_dir> \
  claude -- -p "Review this repo" --output-format stream-json
```

`--result-parser raw` 应为默认值。

## 数据模型

建议新增统一事件模型，例如：

```text
ParsedResultEvent
  job_id
  seq
  parser
  ts_ms
  kind
  text
  raw_json
  metadata
```

建议事件类型：

```text
assistant_delta
assistant_message
tool_started
tool_output
tool_finished
permission_requested
session_observed
thread_observed
error
final_result
raw
parse_error
```

解析失败时不要中断 job：

- 写入 `parse_error`。
- 保留原始 stdout/stderr。
- job 生命周期仍由 child process exit 决定。

## 存储

RunStore 建议保留现有文件，并增量新增：

```text
runs/<job_id>/
  request.json
  stdout
  stderr
  finished.json
  events.jsonl
  parsed_events.jsonl
  parser.json
```

其中：

- `stdout` / `stderr` 继续保存原始输出。
- `parsed_events.jsonl` 保存 result parser 输出。
- `parser.json` 保存 parser 类型、版本、错误计数、最终识别出的 session/thread id。

## 处理流程

```text
JobRequest(result_parser)
-> ahandd spawn child
-> stdout/stderr raw chunks
-> persist raw chunks
-> parser consumes selected stream
-> emit ParsedResultEvent
-> persist parsed events
-> forward parsed events to hub / local IPC observer
```

建议第一阶段只解析 stdout。stderr 作为 raw error stream 保留，除非某个 CLI 明确把结构化事件写到 stderr。

## Parser 设计

所有 parser / formatter 的统一观测维度见：

- `docs/plans/2026-05-13-agent-formatter-observation-dimensions.md`

Codex parser 的详细开发计划见：

- `docs/plans/2026-05-13-codex-jsonl-result-parser.md`

### raw

行为：

- 不解析 JSON。
- stdout/stderr 只作为原始 chunk。
- 可选地把每个 chunk 包装成 `raw` parsed event，方便统一展示。

### codex-jsonl

行为：

- 按行读取 stdout。
- 每行尝试 JSON parse。
- 识别 Codex thread id、assistant 文本、工具事件、错误和最终结果。
- 未识别字段保留在 `raw_json`。

风险：

- Codex CLI JSON schema 可能随版本变化。
- parser 必须宽松解析，不能因字段变化导致 job 失败。

### claude-stream-json

行为：

- 按 Claude Code `stream-json` 事件解析 stdout。
- 识别文本增量、工具调用、权限请求、错误、最终结果。
- 未识别事件保留在 `raw_json`。

风险：

- 单轮 `claude -p ... --output-format stream-json` 与长驻 stdio 模式事件可能不同。
- 第一阶段只覆盖单轮 print mode。

## 观测

新增 parser 后，需要能回答：

- 当前 job 使用了哪个 parser？
- parser 解析了多少事件？
- parser 出现了多少错误？
- 最终识别出的 thread/session id 是什么？
- 原始输出和结构化输出是否都可回放？

本地调试建议：

```bash
ahandctl runs show <job_id>
ahandctl runs parsed <job_id>
ahandctl runs tail <job_id> --parsed
```

这些命令属于后续开发范围，不在本计划当前实现。

## 兼容策略

- 默认 `result_parser = raw`，旧行为不变。
- parser 是附加观测能力，不改变 stdout/stderr 原始事件。
- parser 失败不改变 job exit code。
- 旧 RunStore 目录继续可读。
- 新字段进协议时必须使用新字段号，不能复用已有字段。
- SDK 新版本必须能对旧 hub 降级：不传 parser 或只用 raw。

## 开发阶段

### Phase 1: 字段与存储

- proto 增加 `result_parser` 字段。
- SDK / hub / daemon 透传该字段。
- RunStore 写入 `parser.json`，默认 raw。
- 不做实际解析。

### Phase 2: 本地 raw parser

- daemon 内部建立 parser pipeline。
- raw parser 输出统一 `ParsedResultEvent`。
- 本地 sidecar 能查看 parsed events。

### Phase 3: Codex parser

- 实现 `codex-jsonl`。
- 捕获 thread id、assistant 输出、工具事件、错误和 final result。
- 增加 fixture 测试。

### Phase 4: Claude Code parser

- 实现 `claude-stream-json`。
- 覆盖单轮 `claude -p ... --output-format stream-json`。
- 增加 fixture 测试。

### Phase 5: Hub / SDK 可观测输出

- SSE 增加 parsed event 类型，或新增 parsed stream API。
- SDK 增加 parsed event callbacks。
- 保留 raw stdout/stderr callbacks。

## 验收

- 不传 parser 时，所有现有 job 行为不变。
- `pipe_stream` + `raw` 继续能启动 Codex / Claude Code。
- parser 失败不会导致 job 失败。
- stdout/stderr 原始文件始终存在。
- Codex fixture 能解析出 assistant text 和 thread id。
- Claude fixture 能解析出 assistant text 和 final result。
- 旧 SDK + 新 daemon、新 SDK + 旧 hub 都有明确降级行为。
