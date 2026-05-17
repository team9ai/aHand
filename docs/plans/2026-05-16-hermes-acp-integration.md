# Hermes ACP Integration 开发计划

**Goal:** 为 AHand 增加 Hermes Agent 接入能力：调用方显式提供 Hermes 可执行路径和运行环境，daemon 以 ACP 模式启动或复用 Hermes，确认 ACP 进程可用后发送 prompt，并把 Hermes 返回的结果和流式事件归一成 AHand `AgentObservationRecord` JSONL。这样 AHand 不做本机 runtime 探测，不维护 provider registry，只负责一个明确的 ACP 执行通道：start/health-check、send prompt、collect result。

**Architecture:** 现有 `pipe_stream` runtime 提供进程级 stdin/stdout/stderr transport，但 Hermes ACP 不是用户可直接消费的 stdout 格式，而是 stdin/stdout 上的 JSON-RPC 2.0 协议。因此正式接入应使用三开关模型：`executionMode=pipe_stream` 负责进程 transport，`inputFormat=hermes-acp-json-rpc` 负责向 stdin 写 `initialize`、session setup、`session/prompt` 等 ACP request，`outputFormat=hermes-acp-json-rpc` 负责解析 stdout 上的 ACP response/notification 并输出 AHand `AgentObservationRecord` JSONL。

**Tech Stack:** Rust, tokio process/io, serde_json, protobuf/prost, ahandd RunStore, local IPC, hub control-plane, ACP JSON-RPC

**Related Docs:**
- `docs/HERMES_INTEGRATION.md`
- `docs/plans/2026-05-13-codex-jsonl-result-parser.md`
- `docs/plans/2026-05-13-agent-formatter-observation-dimensions.md`
- `docs/plans/2026-05-12-result-parser-for-agent-output.md`
- `docs/usage/claude-codex-pipe-stream.md`

---

## 背景

当前 AHand 已经实现了 Codex 的第一阶段接入：

- `execution_mode = pipe_stream`：非 TTY、stdin 可写、stdout/stderr 分离。
- `result_parser = codex-jsonl`：识别 Codex JSONL 输出。
- `outputFormat = codex-jsonl`：把 Codex events 映射成统一 `AgentObservationRecord` JSONL。
- RunStore 保存 raw stdout/stderr、request metadata 和 `observations.jsonl`。

这套机制适合 Codex CLI 的 `exec --json` 模式，因为 Codex 把结构化过程事件直接写到 stdout，AHand 只需要按行解析 stdout。

Hermes 的接入模型不同。根据 `docs/HERMES_INTEGRATION.md`，Multica 不是直接调用某个 LLM HTTP API，也不是消费 Hermes 的展示型 stdout，而是：

```text
daemon
  -> spawn "hermes acp"
  -> stdin/stdout JSON-RPC 2.0
  -> initialize
  -> session/new 或 session/resume
  -> session/set_model 可选
  -> session/prompt
  -> consume ACP events
```

因此 Hermes 在 AHand 中不能只建成一个被动 stdout parser。它需要 `inputFormat=hermes-acp-json-rpc` 和 `outputFormat=hermes-acp-json-rpc` 配合：输入侧驱动 ACP request 序列，输出侧解析 stdout 上的 ACP response/notification。

## 目标

第一版 Hermes 接入需要做到：

- 调用方显式传入 Hermes 可执行路径，或显式传入能解析该路径的环境。
- AHand 能以 ACP 模式启动 Hermes，即固定执行 `{hermes_path} acp`。
- AHand 能完成 ACP `initialize`，把它作为 Hermes alive/ready 检查。
- AHand 能创建新 Hermes session，发送 prompt，等待任务完成。
- AHand 能可选设置模型，失败时明确失败，不静默回退。
- AHand 能把 Hermes ACP events 映射成统一 observation records。
- AHand 能保存 Hermes session id，用于后续 resume。
- raw ACP frames 和 normalized observations 都能在 RunStore 中用于 debug/replay。
- 默认不绕过 AHand 本地 policy / approval 模型。

## 非目标

第一阶段不做：

- 不复制 Multica 的 `agent_runtime` / `agent` / `agent_task_queue` 业务表。
- 不自动探测 PATH 中的 Hermes。
- 不做 runtime capability 上报或 provider 注册。
- 不把 Hermes 设为默认 agent。
- 不默认开启 `HERMES_YOLO_MODE=1`。
- 不要求 dashboard 第一阶段有 Hermes 专属 UI。
- 不要求 SDK 第一阶段有高级 `spawnAgent()` API。
- 不把完整 system prompt 塞进 `session/prompt`。
- 不把 ACP JSON-RPC raw stdout 当作 caller-facing stdout。
- 不用 `pipe_stream + raw` 作为正式接入完成标准。

## 设计原则

1. **Hermes ACP 是 input/output format，不是 execution mode。**  
   `ExecutionMode::PipeStream` 只说明 daemon 和 child process 使用 stdin/stdout/stderr pipes。Hermes ACP 的差异由 `inputFormat=hermes-acp-json-rpc` 和 `outputFormat=hermes-acp-json-rpc` 表达：双向 JSON-RPC、request id correlation、session state 和 event dispatch。

2. **raw 两侧都必须保留。**  
   `inputFormat=raw` 表示不转换 stdin；`outputFormat=raw` 表示 caller-facing stdout 仍是 child raw stdout。Hermes 正式接入不使用 raw 作为完成标准，但 raw 是 pipe_stream 的基础能力。

3. **统一 observation 输出。**  
   Hermes 专属事件必须归一到 Codex 已经采用的 `AgentObservationRecord` 维度：`agent`、`runtime`、`llmResponse`、`toolCall`、`error`、`raw`、`usage`。

4. **raw 可回放。**  
   ACP request/response/event 原始 JSON 必须保存，formatter 失败不能让调试失去证据。

5. **权限不默认放开。**  
   Multica 会强制 `HERMES_YOLO_MODE=1`，AHand 不能默认这么做。是否启用 YOLO 必须来自明确 job env、session mode 或专门开关，并且要能被 policy 审计。

6. **上下文通过 workspace 文件注入。**  
   借鉴 Multica，优先写 `AGENTS.md` 和 `.agent_context/skills/`，让 Hermes 从 cwd 加载项目上下文；`session/prompt` 只放本轮任务输入。

## ACP 运行模型

第一版采用最简单、可验证的 job-scoped ACP 进程模型：

```text
JobRequest
  -> validate hermes_path/cwd/env/prompt
  -> spawn "{hermes_path} acp"
  -> initialize
  -> if initialize ok: ACP ready
  -> session/new or session/resume
  -> session/set_model optional
  -> session/prompt
  -> collect events/result
  -> finish job and terminate child if still alive
```

这意味着：

- 每个 AHand job 默认对应一个 Hermes ACP child process。
- `initialize` 是 alive/ready check，而不是单独的 discovery 流程。
- AHand 不维护全局 Hermes runtime 列表。
- AHand 不因为 daemon 启动时找不到 Hermes 而报错；只有收到 Hermes job 且显式 path/env 无法启动或无法 initialize 时，该 job 才失败。
- 后续如果需要长驻 Hermes ACP process，可以在同一 format runner 之上增加 process pool / session pool；第一版不做。

## 建议接口

### 输入契约

Hermes 接入的第一原则是显式配置。AHand 不猜测、不扫描、不注册 runtime。对外使用 `executionMode`、`inputFormat`、`outputFormat` 和显式 executable/prompt/model/session/cwd/env；对内由 `inputFormat=hermes-acp-json-rpc` 转成 ACP JSON-RPC。

最小输入：

```text
hermes_path      # 必填，绝对路径优先；如果是 "hermes"，调用方必须提供 PATH env
cwd              # 必填或强烈建议；Hermes 从 cwd 加载 AGENTS.md / 项目上下文
env              # 显式环境；包括 PATH、HOME、认证变量等
prompt           # 本轮任务输入
model            # 可选
session_id       # 可选，用于 resume
timeout_ms       # 可选
```

format 到 Hermes 原生协议的映射：

| AHand field | Hermes ACP native action |
|---|---|
| `inputFormat=hermes-acp-json-rpc` | enable ACP stdin request sequence |
| `outputFormat=hermes-acp-json-rpc` | parse ACP stdout response/notification |
| `executable` | spawn `{executable} acp` |
| `prompt` | `session/prompt` text content block |
| `model` | `session/new.model` and `session/set_model` |
| `sessionId` | `session/resume`; absent means `session/new` |
| `cwd` / `env` | child process cwd/env and context discovery boundary |
| `instructions` | non-overwriting `AGENTS.md` / `AGENTS.ahand.md` context file |

### 阶段 1：复用现有 JobRequest 做 smoke test

先不新增 proto 字段，用 `tool + args + env + cwd` 验证 Hermes ACP 能启动：

```bash
ahandctl --ipc /tmp/ahand-local-debug.sock exec \
  --execution-mode pipe_stream \
  --cwd "$WORKDIR" \
  --env PATH="$PATH" \
  /absolute/path/to/hermes -- acp
```

这只能验证 `hermes acp` 能启动，不能算完成接入。因为 caller-facing stdout 是 ACP JSON-RPC，不是任务结果。

### 阶段 2：新增 input/output format 选择

建议在协议上新增更明确的 input/output format 字段，而不是滥用 `result_parser` 或旧 `format`：

```text
JobRequest.input_format = "raw" | "text" | "claude-stream-json" | "hermes-acp-json-rpc"
JobRequest.output_format = "raw" | "codex-jsonl" | "claude-stream-json" | "hermes-acp-json-rpc"
JobRequest.executable = optional string
JobRequest.model = optional string
JobRequest.session_id = optional string
JobRequest.prompt = optional string
```

兼容策略：

- 缺省 `input_format=raw`、`output_format=raw`，保持当前 batch/pty/pipe_stream 行为。
- `input_format=hermes-acp-json-rpc` 时，daemon 在 pipe_stream 上主动写 ACP request。
- `output_format=hermes-acp-json-rpc` 时，daemon parse Hermes stdout 上的 ACP response/notification，并输出 normalized observations。
- hub / SDK / ahandctl 对未知 format 做 400，不静默降级。
- 旧 `result_parser` 只作为兼容 parser hint；旧 `format` 废弃，目标字段是 `outputFormat`。

如果暂时不想改 proto，可以先用 env 作为实验开关：

```text
AHAND_INPUT_FORMAT=hermes-acp-json-rpc
AHAND_OUTPUT_FORMAT=hermes-acp-json-rpc
AHAND_AGENT_EXECUTABLE=/absolute/path/to/hermes
AHAND_AGENT_MODEL=<model>
AHAND_AGENT_SESSION_ID=<session_id>
AHAND_AGENT_PROMPT=<prompt>
```

但这只能作为本地实验，不建议作为稳定接口。

### ahandctl 调试接口

建议新增专门子命令，避免用户手写 ACP：

```bash
ahandctl hermes run \
  --ipc /tmp/ahand-local-debug.sock \
  --hermes-path /absolute/path/to/hermes \
  --cwd "$WORKDIR" \
  --env PATH="$PATH" \
  --model "Hermes-3" \
  --prompt "Run tests and fix failures"
```

或者在 `exec` 上先加实验参数：

```bash
ahandctl exec \
  --execution-mode pipe_stream \
  --input-format hermes-acp-json-rpc \
  --output-format hermes-acp-json-rpc \
  --cwd "$WORKDIR" \
  --env AHAND_AGENT_EXECUTABLE=/absolute/path/to/hermes \
  --env AHAND_AGENT_MODEL=Hermes-3 \
  --env AHAND_AGENT_PROMPT="$(cat prompt.txt)" \
  /absolute/path/to/hermes
```

## Daemon 模块设计

建议新增目录：

```text
crates/ahandd/src/agent/
  mod.rs
  input_format.rs
  output_format.rs
  hermes_acp.rs
  observation.rs
```

职责：

| 文件 | 职责 |
|---|---|
| `input_format.rs` | `InputFormat` trait / factory：`raw`、`text`、`claude-stream-json`、`hermes-acp-json-rpc` |
| `output_format.rs` | `OutputFormat` trait / factory：`raw`、`codex-jsonl`、`claude-stream-json`、`hermes-acp-json-rpc` |
| `hermes_acp.rs` | Hermes ACP stdin/stdout format runner、spawn `hermes acp`、JSON-RPC client、session 生命周期、事件读取 |
| `observation.rs` | Hermes ACP event -> `AgentObservationRecord` |
| `mod.rs` | format runner dispatch |

建议 trait：

```rust
pub trait AgentFormatRunner {
    async fn run(
        &mut self,
        req: AgentRunRequest,
        sink: impl EnvelopeSink,
        store: Option<Arc<RunStore>>,
        cancel_rx: mpsc::Receiver<()>,
    ) -> AgentRunResult;
}
```

第一阶段也可以不抽 trait，直接实现：

```rust
pub async fn run_hermes_acp<T>(
    device_id: String,
    req: JobRequest,
    tx: T,
    cancel_rx: mpsc::Receiver<()>,
    store: Option<Arc<RunStore>>,
) -> (i32, String)
where
    T: EnvelopeSink;
```

这样能和当前 `run_job` / `run_job_pty` / `run_job_stream` 保持同样调用形态。

## ACP JSON-RPC Client

Hermes ACP runner 启动：

```text
executable = request.agent_executable 或 env AHAND_AGENT_EXECUTABLE
args       = ["acp"]
cwd        = job cwd
env        = job env + AHand 内部 env
stdin      = piped
stdout     = piped
stderr     = piped
```

必须禁止用户通过 custom args 覆盖 `acp` 子命令。调用方只指定 Hermes binary 和环境，AHand 负责决定 Hermes 的协议模式。

`initialize` 成功就是第一版 health check / alive check。若 spawn 成功但 `initialize` 超时或返回 error，job 直接失败，错误写入 `stderr`、`observations.jsonl` 和 `acp-events.jsonl`。

JSON-RPC 调用顺序：

```text
initialize
session/new 或 session/resume
session/set_model  可选
session/prompt
```

需要实现：

- monotonically increasing JSON-RPC request id
- pending request map
- stdout line/frame reader
- response 与 request id correlation
- notification/event 分发
- stderr raw 保存和转发
- cancel 时 kill child 并输出 terminal observation
- timeout 时 kill child 并输出 terminal observation

如果 ACP frame 不是 newline-delimited JSON，需要先确认 Hermes 的实际 framing；实现前必须用本机 `hermes acp` 抓一份 fixture。不要假设一定是 JSONL，除非 fixture 证明。

## 上下文注入

借鉴 Multica，Hermes 的系统上下文优先写入 cwd 文件：

```text
{cwd}/AGENTS.md
{cwd}/.agent_context/skills/<skill>/SKILL.md
```

第一阶段建议：

- 如果调用方提供 `instructions`，写入 `AGENTS.md`。
- 如果 `AGENTS.md` 已存在，不直接覆盖；写入 `AGENTS.ahand.md` 或在 run dir 生成，并在 prompt 中引用。
- skills 先只支持从 AHand 已知 skill 目录复制到 `.agent_context/skills/`。
- 所有写入行为必须记录到 run artifact。

避免：

- 把完整 system prompt 拼进 `session/prompt`。
- 在用户仓库无提示地覆盖已有 `AGENTS.md`。
- 把 token / secret 写进 `AGENTS.md`。

## 环境变量

建议注入：

```text
AHAND_JOB_ID
AHAND_RUN_DIR
AHAND_DEVICE_ID
AHAND_INPUT_FORMAT
AHAND_OUTPUT_FORMAT
AHAND_AGENT_EXECUTABLE
AHAND_AGENT_MODEL
AHAND_AGENT_SESSION_ID
AHAND_HUB_URL        # 可选，若需要回写
```

保留用户 env，但拦截关键内部变量：

```text
AHAND_JOB_ID
AHAND_RUN_DIR
AHAND_DEVICE_ID
AHAND_INPUT_FORMAT
AHAND_OUTPUT_FORMAT

AHAND_AGENT_EXECUTABLE
```

`HERMES_YOLO_MODE=1` 只能在以下情况下设置：

- job env 显式传入，且 policy 允许；
- 或 AHand 新增 `hermes_yolo = true` 配置，默认 false；
- 或当前 session mode 明确是 `auto_accept`，并且审计记录中标记。

## Observation 映射

Hermes formatter 输出和 Codex formatter 共享 schema：

```json
{
  "schemaVersion": 1,
  "jobId": "job-1",
  "seq": 1,
  "kind": "agent_session",
  "agent": {
    "agentId": "job-1:hermes",
    "agentKind": "hermes",
    "agentSessionId": "hermes-session-id",
    "model": {
      "provider": "nous",
      "id": "Hermes-3"
    }
  },
  "runtime": {
    "jobId": "job-1",
    "executionMode": "pipe_stream",
    "inputFormat": "hermes-acp-json-rpc",
    "outputFormat": "hermes-acp-json-rpc",
    "cwd": "/repo",
    "tool": "hermes",
    "args": ["acp"]
  },
  "raw": {
    "source": "stdout",
    "protocol": "acp-json-rpc",
    "json": {}
  }
}
```

建议 kind 映射：

| Hermes / ACP 概念 | AHand kind |
|---|---|
| session created/resumed | `agent_session` |
| assistant text delta/result | `llm_call_delta` |
| thinking/reasoning | `llm_call_delta` with `channel = "thinking"` |
| tool call start | `tool_call_start` |
| tool call output/result | `tool_call_output` |
| tool call end | `tool_call_end` |
| status/progress | `status` |
| usage | `llm_call_end` with `usage` |
| error | `error` |
| unknown event | `raw` |
| JSON parse/framing error | `parse_error` |

字段提取必须对齐 `docs/HERMES_DATA_EXCHANGE.md` 的优先级：

- `tool_call` 输入字段优先级：`rawInput` -> `input` -> `parameters`。
- `tool_call_update` 输出字段优先级：`rawOutput` -> `output` -> `content[].text`。
- `content` 数组需要拼接所有 text block。
- diff block 不能把完整 diff 无限制塞进 output；应压缩成简短说明，例如 `--- path` / `+++ path` / byte-size summary。
- usage 字段按 Hermes 命名优先读取：`inputTokens`、`outputTokens`、`cachedReadTokens`、`totalTokens`、`thoughtTokens`，再兼容 snake_case fallback。
- `turn_end` / `end_turn` 和 `session/prompt.result` 都要提取 `stopReason` 和 usage。

第一阶段必须保证 raw event 保留；字段未识别时输出 `raw`，不能丢事件。

## Stderr Provider Error Handling

Hermes 的 stderr 不走 ACP JSON-RPC，但 provider 级错误可能只出现在 stderr，例如认证失败、429、额度不足或 provider-specific detail。AHand 需要同时做到：

- 原样保存 stderr 到 run artifact。
- 继续把 stderr chunk 作为 `JobEvent.stderr` 透传。
- 识别常见 provider error pattern，并输出结构化 `error` observation。
- 在最终结果阶段，如果 `session/prompt` 看似 `end_turn` 但 stderr 中有明确 provider failure，应把 job 标记为 failed 或至少输出 terminal error observation。
- 不因为无法识别 stderr 就吞掉原始诊断。

第一版可以先实现 pattern-based detector，后续再按真实 Hermes/provider 样例扩展。

## Permission and Policy Observations

Hermes 可能向 AHand 发起反向 JSON-RPC request，例如：

```text
session/request_permission
```

当前行为可以先返回 `approve_for_session`，但必须变成可观测和可审计：

- 收到 permission request 时输出 `permission_request` 或 `policy_decision` observation。
- 自动批准时输出明确的 `policy_decision` observation，包含 `decision=approved`、`scope=session`、`source=hermes-acp`。
- 拒绝或未知 request 时输出 `policy_decision` / `error` observation。
- 若启用 `HERMES_YOLO_MODE=1` 或类似自动批准行为，必须写入 run metadata 和 audit metadata。
- 后续接入 AHand approval/session policy 后，Hermes permission request 必须走同一套 policy decision path，而不是 formatter 内部静默批准。

这部分是安全边界，不应只存在 daemon log 中。

## RunStore Artifacts

Hermes run 目录建议包含：

```text
runs/<job_id>/
  request.json
  parser.json
  stdout                  # caller-facing observations JSONL
  stderr                  # raw Hermes stderr
  observations.jsonl      # same as formatted stdout
  acp-requests.jsonl      # daemon -> hermes JSON-RPC requests
  acp-events.jsonl        # hermes -> daemon responses/notifications
  hermes-session.json     # session id, model, cwd, resume metadata
  context/
    AGENTS.md or AGENTS.ahand.md
```

`stdout` 的语义：

- 对 process backend：保持当前 child stdout 或 formatter stdout。
- 对 `hermes-acp` backend：caller-facing stdout 是 observation JSONL，不是 raw ACP stdout。

raw ACP stdout 应写入 `acp-events.jsonl`，避免用户把协议帧误当任务文本。

## Hub / SDK 设计

### Control-plane

建议增加请求字段：

```json
{
  "deviceId": "device-123",
  "executionMode": "pipe_stream",
  "inputFormat": "hermes-acp-json-rpc",
  "outputFormat": "hermes-acp-json-rpc",
  "executable": "/absolute/path/to/hermes",
  "model": "Hermes-3",
  "sessionId": "optional-resume-id",
  "cwd": "/repo",
  "env": {
    "PATH": "/usr/local/bin:/usr/bin:/bin",
    "HOME": "/Users/me"
  },
  "prompt": "Run tests and fix failures"
}
```

如果暂时继续使用 `tool/args`：

```json
{
  "deviceId": "device-123",
  "tool": "/absolute/path/to/hermes",
  "inputFormat": "hermes-acp-json-rpc",
  "outputFormat": "hermes-acp-json-rpc",
  "cwd": "/repo",
  "env": {
    "PATH": "/usr/local/bin:/usr/bin:/bin",
    "AHAND_PROMPT": "Run tests and fix failures"
  }
}
```

更推荐新增 agent 专用 API，但可以后置：

```text
POST /api/control/agents/run
GET  /api/control/agents/runs/{id}/stream
POST /api/control/agents/runs/{id}/cancel
```

第一阶段为了少改 surface，可以复用 `/api/control/jobs`，但必须让字段语义清楚。

### SDK

建议新增高级 helper：

```ts
await client.spawnAgent({
  deviceId,
  executionMode: "pipe_stream",
  inputFormat: "hermes-acp-json-rpc",
  outputFormat: "hermes-acp-json-rpc",
  executable: "/absolute/path/to/hermes",
  cwd,
  env: { PATH: process.env.PATH! },
  prompt,
  model,
  sessionId,
  onObservation(record) {},
});
```

`spawn()` 可以保留底层 job 接口。不要让 SDK 用户自己拼 JSON-RPC。

## 实施阶段

### Phase 0: Hermes Fixture 和协议确认

- [x] 由调用方提供一个明确 Hermes binary 路径和运行环境。
- [ ] 用该路径运行 `{hermes_path} acp`，抓取 initialize/session/prompt 的真实 JSON-RPC frames。
- [ ] 确认 ACP framing：JSONL、Content-Length header，还是其他。
- [ ] 保存最小 fixture 到 `crates/ahandd/tests/fixtures/hermes/`。
- [ ] 记录 Hermes version、模型设置命令和错误样例。

验收：

- 有可离线解析的 fixture。
- 不再依赖文档猜测 ACP frame 形态。

### Phase 1: Explicit Launch and Alive Check

- [x] 定义本地调试入口，要求显式传 `--hermes-path`、`--cwd`、`--env`。
- [ ] 将本地调试入口对齐到统一 `AgentInput`，由 HermesAcpInputAdapter 消费。
- [x] spawn `{hermes_path} acp`。
- [x] 发送 `initialize`。
- [x] `initialize` 成功后标记 backend ready。
- [x] `initialize` 失败、超时、EOF 时明确 job failed。
- [x] 保存启动配置和 health-check 结果到 run artifact。

验收：

- 不传 Hermes path 时请求被拒绝。
- Hermes path 无效时错误清楚。
- Hermes 进程存在但 ACP 不 ready 时错误清楚。
- ready 后可以进入 session/prompt 阶段。

### Phase 2: Hermes ACP Client

- [x] 基于显式 path/env 实现 spawn `hermes acp`。
- [x] 实现 JSON-RPC request id、pending response、event reader。
- [x] 将 `initialize` 作为 mandatory ready check。
- [x] 实现 stderr raw capture。
- [x] 实现 cancel/timeout kill child。
- [x] 保存 `acp-requests.jsonl` 和 `acp-events.jsonl`。

验收：

- 单元测试可用 fixture 驱动 client parser。
- 集成测试可用 fake Hermes script 模拟 ACP。

### Phase 3: Session Lifecycle

- [x] 实现 `session/new`。
- [x] 实现 `session/resume`。
- [x] 实现 `session/set_model`。
- [x] 实现 `session/prompt`。
- [x] 保存 `hermes-session.json`。
- [x] model 设置失败时 job failed，不静默继续。

验收：

- 新任务能产生 session id。
- 带 session id 的任务走 resume。
- model failure 有明确错误。

### Phase 4: Observation Formatter

- [x] 新增 `agentKind = "hermes"`。
- [x] Hermes session event -> `agent_session`。
- [x] assistant text -> `llm_call_delta`。
- [x] tool call events -> `tool_call_start/output/end`。
- [x] usage -> `llm_call_end.usage`。
- [x] unknown events -> `raw`。
- [x] parse/framing error -> `parse_error`。
- [x] 对齐 `docs/HERMES_DATA_EXCHANGE.md` 的字段优先级：`rawInput/input/parameters`、`rawOutput/output/content[].text`、usage camelCase 字段。
- [x] 更准确处理 `content[]` text block 拼接。
- [x] 更准确处理 diff block，输出压缩 summary，避免无限制输出完整 diff。

验收：

- caller-facing stdout 是 observation JSONL。
- `observations.jsonl` 和 stdout 内容一致。
- raw ACP events 完整保留。
- Hermes data exchange 文档中的三种 update shape 都有测试覆盖：`sessionUpdate`、`type`、externally tagged object。
- tool input/output 字段优先级有测试覆盖。
- content array 和 diff block 有测试覆盖。

### Phase 5: Context Injection

- [x] 设计 `AGENTS.md` 写入策略，不覆盖用户文件。
- [ ] 支持 `.agent_context/skills/` 复制。
- [x] RunStore 记录写入的 context 文件。
- [x] prompt 中只传任务输入和必要引用。

验收：

- Hermes 能在 cwd 读取上下文。
- 已存在 `AGENTS.md` 时不会被静默覆盖。

### Phase 6: Hub / SDK Surface

- [x] proto 增加 `input_format` / `output_format` 或确定临时 env 方案。
- [x] hub control-plane 校验并转发 agent fields。
- [x] SDK 新增 `spawnAgent()` 或扩展 `spawn()`。
- [x] ahandctl 新增 `hermes <hermes-path> --prompt ...` 调试入口。

验收：

- SDK 用户不需要懂 ACP。
- 旧 process job 行为不变。
- 旧字段缺省时兼容。

### Phase 7: Policy and Safety

- [x] 明确 Hermes backend 是否允许自动工具执行。
- [x] `HERMES_YOLO_MODE` 默认 false。
- [ ] 若启用 YOLO，写入 audit/run metadata。
- [ ] Hermes 写文件/执行命令的行为和 AHand policy 的边界写清楚。
- [x] `session/request_permission` 必须输出 observation/audit record，不能只静默响应。
- [x] 自动批准、拒绝、未知 permission method 都要有 `policy_decision` 或 `error` observation。
- [ ] 后续接入 AHand approval/session policy，使 Hermes permission request 走统一 policy path。

### Phase 8: Stderr Provider Error Promotion

- [x] 保留 raw stderr artifact 和 stderr JobEvent。
- [x] 增加 provider error detector，识别认证失败、429、额度不足、provider unavailable 等常见模式。
- [x] stderr provider error 输出结构化 `error` observation。
- [x] 如果 prompt response 看似成功但 stderr 包含明确 provider failure，最终结果不能静默标成成功。
- [ ] provider error 样例加入 fixture。

验收：

- 默认配置不绕过 AHand 审批模型。
- 安全相关 env 无法被用户 custom env 覆盖。
- permission/policy 行为能在 stdout observations、run artifacts 和后续 audit 中追踪。
- stderr provider failure 能在 observation 中看到，并影响最终 job 状态或至少形成 terminal error observation。

## 测试计划

单元测试：

- ACP frame parser：完整 frame、chunk boundary、invalid JSON。
- JSON-RPC correlation：response id 匹配、unknown id、error response。
- Hermes observation formatter：session/text/tool/usage/error/raw。
- Hermes field priority：`rawInput/input/parameters`、`rawOutput/output/content[].text`。
- Hermes content handling：content array、nested content text、diff summary。
- Hermes permission observations：request、auto-approve、unknown method。
- Hermes stderr provider detector：认证失败、429、额度不足 fixture。
- explicit launch validation：missing executable、invalid executable、spawn failure。
- env filtering：内部变量不能被覆盖。

集成测试：

- fake `hermes` binary：读取 stdin JSON-RPC，按 fixture 输出 events。
- `run_hermes_acp` 成功路径：stdout observations、session artifact、exit 0。
- model set failure：job failed，error observation。
- timeout/cancel：child 被 kill，terminal error 正确。
- resume：传入 prior session id 后调用 `session/resume`。

端到端测试：

- 本地真实 Hermes CLI smoke test，使用显式 Hermes path/env。
- 通过 `ahandctl hermes run` 执行一个只读任务。
- 通过 hub control-plane `spawnAgent` 或 job fields 远程触发。

## 风险

1. **ACP framing 未确认。**  
   文档只说 JSON-RPC 2.0，不保证 newline-delimited JSON。必须先做 fixture。

2. **Hermes event schema 可能变化。**  
   formatter 第一版必须保留 unknown raw event，避免 schema 漂移导致数据丢失。

3. **Hermes 工具执行可能绕过 AHand policy。**  
   如果 Hermes 自己执行 shell/file ops，AHand 不一定能逐项审批。第一版要明确安全边界，默认不要启用 YOLO。

4. **AGENTS.md 写入可能污染用户仓库。**  
   必须有不覆盖策略和 artifact 记录。

5. **与现有 `pipe_stream` 概念混淆。**  
   Hermes ACP backend 可使用 pipe 实现传输，但用户接口上不应暴露 raw ACP stdout。

## 当前建议

先不要急着改 hub/SDK 大接口。推荐顺序：

1. 用显式 Hermes path/env 抓 ACP fixture。
2. 在 `ahandd` 内部实现 fake-testable Hermes ACP client。
3. 让本地 IPC 能用 `--hermes-path` 跑通 `hermes-acp` backend，完成 initialize、发送 prompt、输出 observations。
4. 再把字段提升到 proto / hub / SDK。

这样能最大限度复用当前 Codex observation 体系，同时避免把 Hermes 错误地做成普通 stdout parser。
