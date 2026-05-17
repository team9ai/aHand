# Multica 如何接入 Claude Code

本文说明 Multica 项目本身是如何和 Claude Code 连接起来的。这里的 Claude Code 指本地 `claude` CLI。Multica 不直接调用 Anthropic HTTP API，而是由本机 daemon 启动 `claude` 进程，并使用 Claude Code 的 `stream-json` 输入输出格式与它交换数据。

## 总体链路

Multica 的 Claude Code 接入链路分为五层：

1. 本机 daemon 启动时探测 `claude` CLI。
2. daemon 将探测到的 Claude Code 注册成一个 `agent_runtime`。
3. 工作区里的 `agent` 绑定到这个 Claude runtime。
4. 有任务时，daemon 根据 runtime provider 创建 Claude backend。
5. Claude backend 启动 `claude -p --output-format stream-json --input-format stream-json`，通过 stdin/stdout 执行任务。

换句话说，Multica 里的 agent 是业务层实体，Claude Code 是这个 agent 背后实际执行代码任务的本地 provider。

## 1. daemon 探测 Claude CLI

daemon 读取配置时会探测一组支持的本地 AI 编程工具，其中包括 Claude Code：

```go
probe("MULTICA_CLAUDE_PATH", "claude", "MULTICA_CLAUDE_MODEL")
```

对应文件：

- `server/internal/daemon/config.go`

含义是：

- 如果设置了 `MULTICA_CLAUDE_PATH`，就用这个路径作为 Claude 可执行文件。
- 如果没有设置，就默认查找 PATH 里的 `claude`。
- 如果设置了 `MULTICA_CLAUDE_MODEL`，这个值会作为 Claude runtime 的默认模型。

探测成功后，daemon 会把 Claude 放进本机可用 agents map：

```go
agents["claude"] = e
```

这里的 key `"claude"` 后续会作为 provider 类型贯穿整个系统。

## 2. Claude 被注册成 agent runtime

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

对 Claude Code 来说，写入表里的核心字段通常是：

```text
runtime_mode = local
provider     = claude
status       = online
```

## 3. Multica agent 绑定 Claude runtime

Multica 里的 agent 是业务实体，它不直接等于 Claude 进程。agent 表通过 `runtime_id` 指向某个 `agent_runtime`。

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

因此，一个 Claude Code agent 本质上是：

```text
agent.runtime_id -> agent_runtime.id
agent_runtime.provider = "claude"
```

agent 自己保存人设说明、环境变量、额外命令行参数、MCP 配置和模型选择；runtime 保存这个 agent 应该由哪个本地 provider 执行。

## 4. daemon 领取任务后创建 Claude backend

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

如果 provider 是 `"claude"`，`agent.New` 会返回：

```go
case "claude":
    return &claudeBackend{cfg: cfg}, nil
```

这一步把业务层任务切换到了 Claude Code 后端实现。

## 5. Claude backend 启动 Claude Code stream-json 模式

Claude 的核心实现位于：

- `server/pkg/agent/claude.go`

`claudeBackend` 会构造固定协议参数：

```text
claude
  -p
  --output-format stream-json
  --input-format stream-json
  --verbose
  --strict-mcp-config
  --permission-mode bypassPermissions
  --disallowedTools AskUserQuestion
```

这些参数的作用是：

| 参数 | 作用 |
| --- | --- |
| `-p` | Claude Code 非交互执行模式 |
| `--output-format stream-json` | stdout 输出一行一个 JSON 事件 |
| `--input-format stream-json` | stdin 接收 JSON 输入 |
| `--verbose` | 输出更完整的结构化事件 |
| `--strict-mcp-config` | 只使用传入的 MCP 配置 |
| `--permission-mode bypassPermissions` | daemon 无人值守运行，跳过交互授权 |
| `--disallowedTools AskUserQuestion` | 禁用内置交互提问工具，避免问题丢在无人值守进程里 |

如果配置了模型，会追加：

```text
--model <model>
```

如果配置了最大轮数，会追加：

```text
--max-turns <n>
```

如果是恢复会话，会追加：

```text
--resume <session_id>
```

如果有 runtime brief 需要内联，会追加：

```text
--append-system-prompt <runtimeBrief>
```

## 6. MCP 配置如何传给 Claude

Claude Code 支持 MCP。Multica agent 表里有 `mcp_config` 字段。

执行任务时，如果 agent 配置了 MCP，Claude backend 会：

1. 把 `mcp_config` 写入一个临时 JSON 文件。
2. 启动 Claude 时追加：

```text
--mcp-config /tmp/multica-mcp-xxxx.json
```

3. 进程结束后删除临时文件。

对应代码在：

- `server/pkg/agent/claude.go`

并且 `--mcp-config` 被列为协议关键参数，用户不能通过 `custom_args` 覆盖。

## 7. 上下文通过 CLAUDE.md 和 native skills 注入

Claude Code 的上下文注入使用它自己的原生文件机制。

对应文件：

- `server/internal/daemon/execenv/runtime_config.go`
- `server/internal/daemon/execenv/context.go`

对 Claude，Multica 会写：

```text
{workDir}/CLAUDE.md
```

注释里明确说明：

```text
For Claude: writes {workDir}/CLAUDE.md
```

这个文件包含 Multica runtime brief，包括：

- 当前 agent 身份和说明。
- 可用的 `multica` CLI 命令。
- issue / comment / chat / autopilot 的工作流程。
- 项目资源信息。
- 最终必须如何回写评论和状态。

Skills 对 Claude 走原生发现路径：

```text
{workDir}/.claude/skills/{skill-name}/SKILL.md
```

这点和 Hermes 不同。Hermes 在当前项目里使用 `.agent_context/skills/` fallback，而 Claude 使用 Claude Code 原生 `.claude/skills/`。

## 8. prompt 如何发送给 Claude

Claude backend 启动进程后，会往 stdin 写一条 JSON：

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

写完后关闭 stdin。之后 Claude 会在 stdout 上持续输出 `stream-json` 事件。

## 9. 环境变量注入

daemon 会为 Claude 任务注入一组 Multica 内部环境变量，让 Claude 运行期间可以调用 `multica` CLI 回写任务状态、评论和结果。

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

此外，用户在 agent 设置里配置的 `custom_env` 也会注入到进程环境中，但 daemon 会拦截关键内部变量，防止覆盖 Multica 自己需要的认证和任务上下文。

Claude backend 还会过滤父进程里的 Claude Code 相关环境变量：

```text
CLAUDECODE
CLAUDECODE_*
CLAUDE_CODE_*
```

这样可以避免外层 Claude Code 环境影响子进程。

## 10. custom args 的处理

Claude agent 支持 `custom_args`，但 Multica 会拦截会破坏通信协议的参数：

```text
-p
--output-format
--input-format
--permission-mode
--mcp-config
```

这些参数由 daemon 固定控制。其他参数会被追加到 Claude 命令后面。

daemon 级别还支持：

```text
MULTICA_CLAUDE_ARGS
```

这会作为 provider 默认参数传给 Claude backend，然后再追加每个 agent 自己的 `custom_args`。

## 11. 会话恢复

Claude Code 的会话恢复通过 CLI 参数：

```text
--resume <session_id>
```

执行过程中，Claude 会在 `system` 或 `result` 事件里输出 `session_id`。Multica 会把它保存到任务结果，供下一次同 agent、同 issue 或同 chat session 继续恢复。

如果请求了 resume，但 Claude 输出了新的 session id 并且本次执行失败，Multica 会把最终 `SessionID` 清空，让 daemon 的“恢复失败后重试新会话”逻辑可以触发。

## 12. 模型列表

Claude Code 的模型列表在 Multica 里是静态 catalog，不是运行时动态探测。

对应文件：

- `server/pkg/agent/models.go`

`ListModels("claude")` 会返回 `claudeStaticModels()`。执行时如果 agent.model 为空，daemon 不会传 `--model`，而是让 Claude Code 自己使用 CLI 当前默认模型。

## 关键源码索引

- `server/internal/daemon/config.go`：探测 `claude` CLI，读取 `MULTICA_CLAUDE_PATH` / `MULTICA_CLAUDE_MODEL` / `MULTICA_CLAUDE_ARGS`。
- `server/internal/handler/daemon.go`：daemon 注册 runtime，写入 `agent_runtime.provider = "claude"`。
- `server/pkg/db/queries/runtime.sql`：runtime upsert 查询。
- `server/pkg/db/queries/agent.sql`：agent 创建、更新、绑定 runtime。
- `server/internal/daemon/daemon.go`：daemon 领取任务、构造执行环境、创建 backend、执行任务。
- `server/pkg/agent/agent.go`：统一 backend factory，`"claude"` 映射到 `claudeBackend`。
- `server/pkg/agent/claude.go`：Claude Code stream-json backend 的核心实现。
- `server/pkg/agent/models.go`：Claude 静态模型列表。
- `server/internal/daemon/execenv/runtime_config.go`：为 Claude 写入 `CLAUDE.md`。
- `server/internal/daemon/execenv/context.go`：为 Claude 写入 `.claude/skills/`。

## 一句话总结

Multica 接入 Claude Code 的方式是：本机 daemon 自动发现 `claude` CLI 并注册为 `agent_runtime`，业务 agent 绑定这个 runtime；任务到来时 daemon 创建 `claudeBackend`，启动 `claude -p --output-format stream-json --input-format stream-json`，通过 stdin 发送用户任务 JSON，通过 stdout 读取 Claude 的结构化事件，并把文本、工具调用、token usage、session id 和最终结果写回 Multica。
