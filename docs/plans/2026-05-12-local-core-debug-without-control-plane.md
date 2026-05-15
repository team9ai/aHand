# AHand Local Sidecar 调试与观测方案

## 目标

为 AHand 增加一个**本地 sidecar 调试入口**，让开发者在不启动 `ahand-hub`、dashboard、PostgreSQL、Redis、control-plane JWT、设备注册流程的情况下，直接调试 daemon 内部核心执行逻辑。

这个 sidecar 的定位是：

- 本地开发和调试入口。
- 本地过程观测入口。
- Claude/Codex CLI 适配验证入口。
- 回归测试和排查 executor 问题的低成本入口。

它不是生产控制面的替代品，也不是绕过安全策略的后门。

## 当前实现状态

截至本方案更新时：

- `pipe_stream` 是主线功能，继续通过 protocol / hub / daemon / SDK 路径实现。
- control-plane 路径已增加 `pipe_stream` 解析单元测试，确保显式 `executionMode=pipe_stream` 优先于兼容字段 `interactive`。
- control-plane integration test 已补充 `pipe_stream` 下发断言，并通过编译检查；当前沙箱禁止绑定本地端口，因此不能在本环境实际运行该 integration test。
- `ahandctl identity show` 已作为手动注册辅助命令实现，可输出 daemon hub identity 的 `deviceId` 和 `publicKey`。
- `ahandd --mode local --debug-ipc` 已实现，可在不连接 hub 的情况下启动本地 IPC sidecar。
- `ahandctl --ipc <socket> exec --execution-mode <batch|pty|pipe_stream>` 已实现，并保持旧 `ahandctl exec <tool> [args...]` 默认 batch 行为。
- `ahandctl --ipc <socket> exec --result-parser <raw|codex-jsonl|claude-stream-json>` 已实现，可把 parser hint 写入本地 run artifacts；实际 Codex/Claude 输出解析仍未实现。
- IPC dispatch 已按 `resolve_job_execution_mode(req)` 分流到 `run_job`、`run_job_pty`、`run_job_stream`。
- IPC 已支持向运行中的 `pty` / `pipe_stream` job 转发 `StdinChunk` 和 `TerminalResize`。

因此，本文档中的基础 sidecar/local debug 路径已经可用；后续计划集中在 runs CLI、attach/tail、结构化观测查询和测试覆盖增强。

## 不影响现有功能的红线

Local sidecar 必须满足以下约束：

1. **默认关闭。** 不显式启用时，现有 `ahandd` 启动、hub 连接、dashboard、SDK、control-plane API 行为完全不变。
2. **不改变 hub 生产路径。** `SDK/dashboard -> ahand-hub -> ahandd -> executor` 仍是生产路径。
3. **不改变协议兼容。** `interactive` 继续保留，`execution_mode` 只作为新增显式字段；兼容逻辑不因 sidecar 改动。
4. **只在 local 模式放宽调试默认值。** 生产默认值不变；显式 `--mode local` 且未配置 session mode 时，默认使用 `trust`，让本地调试不依赖控制面审批。
5. **不新增生产依赖。** sidecar 不要求 Postgres、Redis、hub、dashboard 或远端凭证。
6. **不读取远端控制面凭证。** sidecar 不使用 control-plane JWT、hub service token、dashboard token。
7. **不复制一套执行器。** sidecar 必须复用 daemon 内部 dispatch、registry、session、approval、executor、RunStore。
8. **不破坏现有 artifact。** 只能增量增加本地观测文件或字段，不能改变已有 run store 语义。
9. **不改变 child process 语义。** 同一个 `JobRequest` 在 hub path 和 sidecar path 应选择同一 execution mode 和同一 executor runtime。

验收要求：

- 不传任何 local/debug 参数时，现有行为与当前版本一致。
- 现有 `ahandctl start/status/stop/restart` 行为不变。
- 现有 hub、SDK、daemon 测试继续通过。
- sidecar 相关测试不依赖外部 hub。

## 目标体验

### 最短本地调试

```bash
ahandd --mode local --debug-ipc
```

或在现有配置基础上：

```bash
ahandd --config ~/.ahand/config.toml --debug-ipc
```

然后本地注入 job：

```bash
ahandctl --ipc ~/.ahand/ahandd.sock exec --execution-mode batch echo hello

ahandctl --ipc ~/.ahand/ahandd.sock exec --execution-mode pipe_stream \
  --result-parser codex-jsonl \
  codex -- exec --skip-git-repo-check --json --cd "$PWD" -

ahandctl --ipc ~/.ahand/ahandd.sock exec --execution-mode pipe_stream \
  --result-parser claude-stream-json \
  claude -- -p "Review this repo" --output-format stream-json
```

如果参数以 `-` 开头，需要在 tool 后加 `--`，例如：

```bash
ahandctl --ipc ~/.ahand/ahandd.sock exec sh -- -c 'echo hello'
```

旧形式仍然兼容，默认等价于 `--execution-mode batch`：

```bash
ahandctl --ipc ~/.ahand/ahandd.sock exec echo hello
```

### 本地观测

```bash
ahandctl runs list
ahandctl runs show <job_id>
ahandctl runs tail <job_id>
```

或者直接看文件：

```text
~/.ahand/data/runs/<job_id>/
  request.json
  parser.json
  stdout
  stderr
  result.json
  events.jsonl
```

`parser.json` 当前只记录 parser 配置和错误计数，默认 parser 是 `raw`。`codex-jsonl` / `claude-stream-json` 的结构化解析属于 result parser 后续阶段。

## 架构

生产路径保持不变：

```text
SDK / dashboard
-> ahand-hub
-> WebSocket gateway
-> ahandd
-> shared dispatch
-> executor
```

新增本地 sidecar 路径：

```text
ahandctl / local debug tool
-> local IPC socket
-> ahandd sidecar IPC server
-> shared dispatch
-> executor
```

两条路径只在 daemon 内部的 `shared dispatch` 汇合。sidecar 不接管 hub，不改 hub job 状态机，不要求 SDK/dashboard 变更。

建议抽出 daemon 内部共享函数：

```text
dispatch_job(
  device_id,
  job_request,
  caller_uid,
  sink,
  registry,
  session_mgr,
  approval_mgr,
  store
)
```

调用方：

- hub WebSocket path
- local IPC path
- future test harness

## Execution Mode 支持

主线远程路径已经需要覆盖三种执行模式。本地 sidecar 后续也要覆盖同一组三种模式，确保本地调试和 hub 下发语义一致：

| Mode | 本地用途 | stdin | stdout/stderr | TTY |
|---|---|---|---|---|
| `batch` | 普通命令、脚本、测试 | 不维护持续 stdin | 分离 | 否 |
| `pipe_stream` | Claude/Codex/headless agent、stdin consumer | 持续写 child stdin | 分离 | 否 |
| `pty` | shell、TUI、需要真实终端的程序 | 写 PTY master | 合并终端字节流 | 是 |

未来 `ahandctl exec` 需要显式开关：

```bash
--execution-mode batch
--execution-mode pipe_stream
--execution-mode pty
```

默认保持 `batch`，保证旧用法不变。

## 需要开发的能力

### 1. Local Mode

新增 daemon local mode：

```bash
ahandd --mode local --debug-ipc
```

行为：

- 不连接 hub。
- 不需要 `server_url`。
- 必须启用 IPC，否则直接报错。
- 使用本地 identity 派生 device id。
- 显式 `--data-dir` 时，local identity 默认写入该 data dir，避免调试依赖 `~/.ahand` 可写。
- 未配置 session mode 时，local mode 默认 `trust`，仅影响显式 local debug。
- data dir 仍使用 `~/.ahand/data` 或显式 `--data-dir`。

兼容：

- 默认 `ahand-cloud` 不变。
- 现有 `openclaw-gateway` 不变。
- `ahandctl start` 不自动进入 local mode，除非 config 显式设置。

### 2. ahandctl 本地注入

**Status:** 已实现。旧 `ahandctl exec` 行为已保留，默认 `batch`。

扩展：

```bash
ahandctl --ipc ~/.ahand/ahandd.sock exec \
  --execution-mode pipe_stream \
  --cwd /path/to/repo \
  --timeout-ms 1800000 \
  codex exec "Run tests"
```

建议字段：

- `--execution-mode`
- `--cwd`
- `--timeout-ms`
- `--env KEY=VALUE`
- `tool`
- `args...`

发送 `JobRequest` 时：

```text
execution_mode = selected mode
interactive = selected mode == pty
```

### 3. IPC Dispatch 复用 hub 逻辑

**Status:** 基础分流已实现。IPC path 已按 execution mode 调用同一组 executor runtime；后续仍建议进一步抽出 shared dispatch，减少 `ahand_client.rs` 与 `ipc.rs` 中 job 启动逻辑的重复。

长期建议抽取共享 dispatch：

```text
resolve_job_execution_mode(req)
-> batch       -> run_job
-> pipe_stream -> run_job_stream
-> pty         -> run_job_pty
```

共享逻辑必须覆盖：

- idempotency
- session check
- approval request / response
- registry register/remove
- stdin sender registration
- concurrency permit
- cancel
- timeout
- RunStore
- JobEvent / JobFinished / JobRejected

### 4. Stdin 与 Attach

为了调试 `pipe_stream`，需要本地 stdin 注入：

```bash
printf 'continue\n' | ahandctl --ipc ~/.ahand/ahandd.sock stdin <job_id>
```

进一步提供 attach：

```bash
ahandctl --ipc ~/.ahand/ahandd.sock attach <job_id>
```

attach 行为：

- 持续显示 stdout/stderr。
- 从本地 stdin 读 bytes 并转发到 job stdin。
- 默认 Ctrl-C 只 detach。
- `--cancel-on-ctrl-c` 才取消远端 job。

模式差异：

- `batch`: stdin 返回清晰错误。
- `pipe_stream`: 写 child stdin。
- `pty`: 写 PTY master。

### 5. 本地观测数据

本地 sidecar 的主要价值是观测，因此要补齐过程数据。

建议 run 目录：

```text
~/.ahand/data/runs/<job_id>/
  request.json
  stdout
  stderr
  finished.json
  events.jsonl
  metadata.json
```

`events.jsonl` 建议记录：

- job accepted
- execution mode
- child pid
- started_at
- stdout chunk metadata
- stderr chunk metadata
- stdin chunk metadata
- cancel requested
- timeout
- finished

注意：stdout/stderr 内容仍写独立文件，`events.jsonl` 只放 metadata 和小摘要，避免超大日志膨胀。

### 6. 本地 Runs CLI

新增：

```bash
ahandctl runs list
ahandctl runs show <job_id>
ahandctl runs tail <job_id>
```

用途：

- 不依赖 hub dashboard 查看本地任务。
- 快速复盘 Claude/Codex 输出。
- 调试 executor 和 mode 选择。

第一阶段可以先文档化目录结构，不急着做完整 CLI。

### 7. Claude/Codex 本地示例

当前 pipe stream 示例：

```bash
ahandctl --ipc ~/.ahand/ahandd.sock exec --execution-mode pipe_stream \
  --result-parser claude-stream-json \
  claude -- -p "Summarize this repo" --output-format stream-json

printf 'Run tests and summarize failures\n' | \
ahandctl --ipc ~/.ahand/ahandd.sock exec --execution-mode pipe_stream \
  --result-parser codex-jsonl \
  codex -- exec --skip-git-repo-check --json --cd "$PWD" -
```

如果 PATH 依赖 shell 初始化：

```bash
ahandctl --ipc ~/.ahand/ahandd.sock exec --execution-mode pipe_stream \
  --result-parser raw \
  shell -- -lc 'codex exec "Run tests"'
```

需要真实 TTY 时：

```bash
ahandctl --ipc ~/.ahand/ahandd.sock exec --execution-mode pty shell
```

## 分阶段实施

### Phase 0: 冻结边界

Tasks:

- [x] 明确 sidecar 默认关闭。
- [x] 明确 sidecar 不读 hub/control-plane 凭证。
- [x] 明确 sidecar 必须走 shared dispatch。
- [x] 增加文档说明不影响生产路径。

Acceptance:

- [ ] 评审确认 sidecar 是 additive，不改变现有 hub/SDK/dashboard 行为。

### Phase 1: 现有 IPC 最小可用文档

Tasks:

- [ ] 新增 `docs/usage/local-core-debug.md`。
- [ ] 说明 `debug_ipc = true` / `--debug-ipc`。
- [ ] 说明 `ahandctl --ipc ... exec`。
- [ ] 标注当前能力边界：适合 batch-style stdout/stderr。

Acceptance:

- 不启动 hub 可以本地执行 `echo`、`cargo test`、简单 Claude/Codex headless 命令。

### Phase 2: ahandctl execution-mode

Tasks:

- [ ] `ahandctl exec` 新增 `--execution-mode`。
- [ ] 默认 `batch`。
- [ ] 发送 `execution_mode` 和兼容 `interactive`。
- [ ] 加 `--cwd`、`--timeout-ms`、`--env`。
- [ ] 增加参数解析测试。

Acceptance:

- 旧命令不变。
- 新命令能发出 `pipe_stream` JobRequest。

Note:

- 该阶段必须独立提交或至少独立验证，避免与主线 `pipe_stream` implementation 混在一起。
- 任何未完成的 CLI 改动都不应保留在工作区影响现有 `ahandctl exec`。

### Phase 3: Shared Dispatch

Tasks:

- [ ] 从 hub WebSocket path 抽出 shared dispatch。
- [ ] IPC path 使用同一个 dispatch。
- [ ] 三种 mode 都经过同样的 registry/session/approval/cancel/timeout。
- [ ] 补 IPC mode tests。

Acceptance:

- 同一 JobRequest 在 hub path 和 IPC path 行为一致。
- `batch`、`pipe_stream`、`pty` 都可从 IPC 启动。

### Phase 4: Stdin / Attach

Tasks:

- [ ] 新增 `ahandctl stdin <job_id>`。
- [ ] 新增 `ahandctl attach <job_id>`。
- [ ] 支持 `pipe_stream` stdin。
- [ ] 支持 `pty` stdin。
- [ ] 对 `batch` stdin 给明确错误。

Acceptance:

- 可以通过 IPC 调试 stdin consumer。
- Claude/Codex 如果需要持续 stdin，可以在本地验证。

### Phase 5: Local Observability

Tasks:

- [ ] `RunStore` 增加 execution mode、cwd、env 摘要。
- [ ] 增加 `events.jsonl`。
- [ ] 增加 `metadata.json`。
- [ ] 可选增加 `ahandctl runs` 系列命令。

Acceptance:

- 不启动 hub 也能复盘 job 的过程和终态。

### Phase 6: Local Mode

Tasks:

- [ ] `ConnectionMode` 增加 `local`。
- [ ] 支持 `ahandd --mode local --debug-ipc`。
- [ ] local mode 不启动 hub reconnect loop。
- [ ] local mode 未启用 IPC 时直接报错。
- [ ] local mode 文档化。

Acceptance:

- 无 hub URL 也能启动本地 core runner。
- 不产生 hub reconnect 噪音。

## 测试矩阵

| Case | Command | Expected |
|---|---|---|
| default unchanged | `ahandd` existing config | behavior unchanged |
| IPC disabled | no `--debug-ipc` | no sidecar socket |
| batch | `exec --execution-mode batch echo hello` | stdout + exit 0 |
| pipe stream | `exec --execution-mode pipe_stream ...` | stdout/stderr separated |
| pty | `exec --execution-mode pty shell` | terminal bytes |
| stdin pipe | `stdin <job_id>` | child receives bytes |
| cancel | `cancel <job_id>` | child killed |
| timeout | low timeout | finished timeout |
| approval | strict mode | approval request emitted |
| artifact | any mode | run files written |
| production path | hub dispatch | unchanged |

## 推荐第一批改动

sidecar 第一批只做最小闭环：

1. 文档化 local sidecar 约束和使用方式。
2. `ahandctl exec --execution-mode`。
3. IPC path 复用 shared dispatch。

但在当前开发顺序里，主线 `pipe_stream` 功能优先，sidecar 不阻塞主线：

1. 继续完成 `pipe_stream` 的 protocol / hub / daemon / SDK / DB 兼容。
2. 用 control-plane tests 锁住 `executionMode=pipe_stream` 不退化。
3. 再开始 sidecar Phase 1/2。

暂缓：

- 完整 `ahandctl attach`。
- `ahandctl runs`。
- Windows named pipe。
- 自动 Claude/Codex 参数封装。

这样可以快速获得本地观测和调试能力，同时把对现有功能的影响控制在最小范围。
