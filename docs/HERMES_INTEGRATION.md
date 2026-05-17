# Multica 如何接入 Hermes Agent

本文说明 Multica 项目本身是如何和 Hermes Agent 连接起来的。这里的 Hermes 指 Nous Research 的 `hermes` CLI，Multica 不是直接调用某个 LLM HTTP API，而是通过本机 daemon 启动 `hermes acp` 进程，并使用 ACP 协议与它通信。

## 总体链路

Multica 的 Hermes 接入链路分为五层：

1. 本机 daemon 启动时探测 `hermes` CLI。
2. daemon 将探测到的 Hermes 注册成一个 `agent_runtime`。
3. 工作区里的 `agent` 绑定到这个 Hermes runtime。
4. 有任务时，daemon 根据 runtime provider 创建 Hermes backend。
5. Hermes backend 启动 `hermes acp`，通过 ACP JSON-RPC 执行任务。

换句话说，Multica 里的 agent 是业务层实体，Hermes 是这个 agent 背后实际执行代码任务的本地 provider。

## 1. daemon 探测 Hermes CLI

daemon 读取配置时会探测一组支持的本地 AI 编程工具，其中包括 Hermes：

```go
probe("MULTICA_HERMES_PATH", "hermes", "MULTICA_HERMES_MODEL")
```

对应文件：

- `server/internal/daemon/config.go`

含义是：

- 如果设置了 `MULTICA_HERMES_PATH`，就用这个路径作为 Hermes 可执行文件。
- 如果没有设置，就默认查找 PATH 里的 `hermes`。
- 如果设置了 `MULTICA_HERMES_MODEL`，这个值会作为 Hermes runtime 的默认模型。

探测成功后，daemon 会把 Hermes 放进本机可用 agents map：

```go
agents["hermes"] = e
```

这里的 key `"hermes"` 后续会作为 provider 类型贯穿整个系统。

## 2. Hermes 被注册成 agent runtime

daemon 会把本机探测到的 runtime 列表提交给 Multica server。服务端收到后，根据 runtime 的 `Type` 字段写入 `agent_runtime` 表。

关键数据结构是：

```sql
CREATE TABLE agent_runtime (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    daemon_id TEXT,
    name TEXT NOT NULL,
    runtime_mode TEXT NOT NULL CHECK (runtime_mode IN ('local', 'cloud')),
    provider TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'offline',
    ...
    UNIQUE (workspace_id, daemon_id, provider)
);
```

对应文件：

- `server/migrations/004_agent_runtime_loop.up.sql`
- `server/internal/handler/daemon.go`
- `server/pkg/db/queries/runtime.sql`

对 Hermes 来说，写入表里的核心字段通常是：

```text
runtime_mode = local
provider     = hermes
status       = online
```

`UNIQUE (workspace_id, daemon_id, provider)` 保证同一个 workspace、同一个 daemon、同一个 provider 只注册一个 runtime。

## 3. Multica agent 绑定 Hermes runtime

Multica 里的 agent 是业务实体，它不直接等于 Hermes 进程。agent 表通过 `runtime_id` 指向某个 `agent_runtime`。

创建 agent 时会保存这些字段：

```sql
INSERT INTO agent (
    workspace_id,
    name,
    description,
    runtime_mode,
    runtime_config,
    runtime_id,
    visibility,
    max_concurrent_tasks,
    owner_id,
    instructions,
    custom_env,
    custom_args,
    mcp_config,
    model
)
```

对应文件：

- `server/pkg/db/queries/agent.sql`

因此，一个 Hermes agent 本质上是：

```text
agent.runtime_id -> agent_runtime.id
agent_runtime.provider = "hermes"
```

agent 自己保存人设说明、环境变量、额外命令行参数、MCP 配置和模型选择；runtime 保存这个 agent 应该由哪个本地 provider 执行。

## 4. daemon 领取任务后创建 Hermes backend

当 issue 被分配给 agent、评论里 @ agent、聊天触发 agent 或 autopilot 到点运行时，服务端会创建 `agent_task_queue` 任务。daemon 轮询到任务后，会根据任务绑定的 runtime provider 创建实际 backend。

关键代码路径：

- `server/internal/daemon/daemon.go`
- `server/pkg/agent/agent.go`

daemon 执行任务时会调用：

```go
backend, err := agent.New(provider, agent.Config{
    ExecutablePath: entry.Path,
    Env:            agentEnv,
    Logger:         d.logger,
})
```

如果 provider 是 `"hermes"`，`agent.New` 会返回：

```go
case "hermes":
    return &hermesBackend{cfg: cfg}, nil
```

这一步把业务层任务切换到了 Hermes 后端实现。

## 5. Hermes backend 启动 `hermes acp`

Hermes 的核心实现位于：

- `server/pkg/agent/hermes.go`

`hermesBackend` 会启动 Hermes CLI 的 ACP 模式：

```go
hermesArgs := append([]string{"acp"}, filterCustomArgs(opts.CustomArgs, hermesBlockedArgs, b.cfg.Logger)...)
cmd := exec.CommandContext(runCtx, execPath, hermesArgs...)
```

如果没有配置 `ExecutablePath`，默认执行：

```text
hermes acp
```

这里的 `acp` 是 Hermes 的协议子命令。Multica 会阻止用户通过 `custom_args` 覆盖这个子命令，因为覆盖后 daemon 就无法和 Hermes 按 ACP 协议通信。

## 6. 通过 ACP JSON-RPC 通信

Multica 和 Hermes 的通信不是 HTTP，而是：

```text
daemon <-> hermes acp
stdin/stdout 上的 JSON-RPC 2.0
```

Hermes backend 启动进程后，会拿到：

```go
stdout, err := cmd.StdoutPipe()
stdin, err := cmd.StdinPipe()
```

然后通过 `hermesClient` 写入 JSON-RPC 请求、读取 Hermes 返回的事件。

一次任务的大致 ACP 调用顺序是：

```text
initialize
session/new 或 session/resume
session/set_model  可选
session/prompt
```

对应代码：

```go
c.request(runCtx, "initialize", ...)
c.request(runCtx, "session/new", ...)
c.request(runCtx, "session/resume", ...)
c.request(runCtx, "session/set_model", ...)
c.request(runCtx, "session/prompt", ...)
```

### 新会话

如果任务没有历史 session，Multica 调：

```go
session/new
```

并从响应里提取 Hermes session id。

### 恢复会话

如果任务有 `PriorSessionID`，Multica 调：

```go
session/resume
```

这样 Hermes 可以接着之前上下文继续工作。Hermes 在 Multica 的 provider 对照里属于真正支持 session resume 的后端。

### 模型选择

如果 agent 或 runtime 配置了 model，Multica 会在发送 prompt 前调用：

```go
session/set_model
```

如果切换模型失败，任务会直接失败，而不是静默回退到 Hermes 默认模型。这样用户不会误以为自己选择的模型已生效。

## 7. 上下文通过工作目录文件注入

Hermes 的特殊点是：Multica 不把完整 system prompt 拼进 `session/prompt`。daemon 会先在任务 workdir 写入 `AGENTS.md`，让 Hermes 自己从当前工作目录读取上下文。

对应文件：

- `server/internal/daemon/execenv/runtime_config.go`
- `server/internal/daemon/daemon.go`

对 Hermes，Multica 会写：

```text
{workDir}/AGENTS.md
```

注释里明确说明：

```text
For Hermes: writes {workDir}/AGENTS.md
```

任务执行时，Hermes backend 也明确忽略 `ExecOptions.SystemPrompt`：

```go
// Hermes ACP loads project/context files from cwd (AGENTS.md, .agent_context, etc.) itself.
```

这样做的原因是：

- 避免把完整 runtime brief 重复塞进用户 prompt。
- 避免 prompt 过大。
- 避免重复上下文触发上游安全过滤。
- 保持 Hermes 使用自己的 cwd-scoped context 加载机制。

## 8. Skills 的接入方式

Hermes 目前没有在 Multica 里配置原生专用 skills 发现路径。对 Hermes，skills 走 fallback 路径：

```text
.agent_context/skills/
```

`AGENTS.md` 里会提示 Hermes：

```text
Detailed skill instructions are in `.agent_context/skills/`.
Each subdirectory contains a `SKILL.md`.
```

这意味着 Multica 会把 skill 文件放在通用上下文目录里，但 Hermes CLI 是否真的读取并遵循这些 skill，取决于 Hermes 自身对该路径的支持情况。

## 9. 环境变量注入

daemon 会为 Hermes 任务注入一组 Multica 内部环境变量，让 Hermes 运行期间可以调用 `multica` CLI 回写任务状态、评论和结果。

典型变量包括：

```text
MULTICA_TOKEN
MULTICA_SERVER_URL
MULTICA_DAEMON_PORT
MULTICA_WORKSPACE_ID
MULTICA_AGENT_NAME
MULTICA_AGENT_ID
MULTICA_TASK_ID
MULTICA_TASK_SLOT
```

此外，Hermes backend 会强制注入：

```text
HERMES_YOLO_MODE=1
```

目的是让 Hermes 自动批准工具执行，避免 agent 在无人值守任务中卡在确认步骤。

用户在 agent 设置里配置的 `custom_env` 也会注入到进程环境中，但 daemon 会拦截关键内部变量，防止覆盖 Multica 自己需要的认证和任务上下文。

## 10. 结果、工具调用和 token usage 回收

Hermes 运行时会通过 ACP 发送多种事件。Multica 会把这些事件统一转换成内部的 agent message：

```go
MessageText
MessageThinking
MessageToolUse
MessageToolResult
MessageStatus
MessageError
MessageLog
```

最终结果会汇总成：

```go
Result{
    Status:    finalStatus,
    Output:    finalOutput,
    Error:     finalError,
    SessionID: sessionID,
    Usage:     usageMap,
}
```

daemon 再把结果写回 server，更新任务状态、保存 session id、记录 token usage，并根据任务类型决定是否补充评论。

## 关键源码索引

- `server/internal/daemon/config.go`：探测 `hermes` CLI，读取 `MULTICA_HERMES_PATH` / `MULTICA_HERMES_MODEL`。
- `server/internal/handler/daemon.go`：daemon 注册 runtime，写入 `agent_runtime.provider = "hermes"`。
- `server/pkg/db/queries/runtime.sql`：runtime upsert 查询。
- `server/pkg/db/queries/agent.sql`：agent 创建、更新、绑定 runtime。
- `server/internal/daemon/daemon.go`：daemon 领取任务、构造执行环境、创建 backend、执行任务。
- `server/pkg/agent/agent.go`：统一 backend factory，`"hermes"` 映射到 `hermesBackend`。
- `server/pkg/agent/hermes.go`：Hermes ACP backend 的核心实现。
- `server/pkg/agent/models.go`：通过临时 `hermes acp` 做模型发现。
- `server/internal/daemon/execenv/runtime_config.go`：为 Hermes 写入 `AGENTS.md` 和 `.agent_context/skills/` 引导。

## 一句话总结

Multica 接入 Hermes 的方式是：本机 daemon 自动发现 `hermes` CLI 并注册为 `agent_runtime`，业务 agent 绑定这个 runtime；任务到来时 daemon 创建 `hermesBackend`，启动 `hermes acp`，通过 ACP JSON-RPC 创建或恢复会话、设置模型、发送 prompt，并从 Hermes 的流式事件中收集文本、工具调用、token usage 和最终结果。
