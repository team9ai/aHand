# Codex 的 MCP 注入方式

本文说明 Multica 中 Codex provider 当前如何接收 MCP 配置，以及它和 Claude Code / Hermes 的差异。

## 结论

Codex 当前 **不直接消费 agent 表里的 `mcp_config` 字段**。

实际 MCP 注入路径是：

```text
用户全局 Codex home
  ~/.codex/config.toml 里的 [mcp_servers.*]
  -> daemon 为任务创建 per-task CODEX_HOME
  -> 复制 config.toml 到 {envRoot}/codex-home/config.toml
  -> 设置环境变量 CODEX_HOME={envRoot}/codex-home
  -> codex app-server 从 per-task CODEX_HOME/config.toml 读取 MCP 配置
```

也就是说，Codex 的 MCP 配置来源是 **Codex CLI 自己的 config.toml**，不是 Multica agent 的 `mcp_config`。

## Codex 启动方式

Multica 启动 Codex 的命令是：

```text
codex app-server --listen stdio://
```

对应代码：

- `server/pkg/agent/codex.go`

Multica 通过 JSON-RPC 2.0 over stdio 和 Codex app-server 通信。

## per-task CODEX_HOME

为了隔离任务环境，daemon 不直接让 Codex 使用用户真实的 `~/.codex`。对 Codex provider，执行环境准备阶段会创建：

```text
{envRoot}/codex-home/
```

然后执行任务时注入：

```text
CODEX_HOME={envRoot}/codex-home
```

相关代码：

- `server/internal/daemon/execenv/execenv.go`
- `server/internal/daemon/execenv/codex_home.go`
- `server/internal/daemon/daemon.go`

## 从全局 Codex home 复制哪些文件

`prepareCodexHomeWithOpts` 会从共享 Codex home 复制或链接部分内容。

共享 Codex home 的来源：

```text
$CODEX_HOME
```

如果没有设置，则使用：

```text
~/.codex
```

复制到 per-task CODEX_HOME 的文件包括：

```text
config.json
config.toml
instructions.md
```

其中 `config.toml` 是 MCP 配置最重要的来源。

链接到 per-task CODEX_HOME 的内容包括：

```text
auth.json
sessions/
```

这样 Codex 任务可以共享登录态和 session 日志，同时 config 文件又是任务级隔离副本。

## MCP 配置在 Codex 里的形态

Codex 的 MCP server 通常配置在 `config.toml` 中，例如：

```toml
[mcp_servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/Users/me/project"]

[mcp_servers.postgres]
command = "uvx"
args = ["mcp-server-postgres"]

[mcp_servers.postgres.env]
DATABASE_URL = "postgres://user:pass@localhost:5432/app"
```

Multica 会把用户共享 Codex home 里的 `config.toml` 复制到：

```text
{envRoot}/codex-home/config.toml
```

然后 Codex app-server 从这个 per-task config 里读取 `[mcp_servers.*]`。

## Multica 对 Codex config.toml 做的处理

复制 `config.toml` 后，Multica 会做几类 daemon-managed 修改。

### 1. 清理不兼容的 skills 配置

Multica 会删除 `[[skills.config]]` 条目。

原因是 Codex Desktop 写入的 plugin-backed skills 配置可能缺少 CLI 需要的 `path` 字段，导致 Codex CLI 解析失败。Multica 会把 agent skills 直接写入 per-task `codex-home/skills/`，所以这些 user-level skills registry 对任务来说是冗余的。

注意：这个清理目标是 `[[skills.config]]`，不是 `[mcp_servers.*]`。

相关代码：

- `server/internal/daemon/execenv/codex_skill_strip.go`

### 2. 注入 sandbox / network 配置

Multica 会确保 per-task `config.toml` 中有 daemon 管理的 sandbox 配置，保证任务可以按当前平台策略运行。

相关代码：

- `server/internal/daemon/execenv/codex_sandbox.go`

### 3. 禁用 Codex native multi-agent

Multica 会在 daemon-managed task session 中禁用 Codex native subagents，避免父 turn 提前被判定完成。

相关代码：

- `server/internal/daemon/execenv/codex_multi_agent.go`

## agent.mcp_config 当前不会注入 Codex

虽然通用 `ExecOptions` 里有：

```go
McpConfig json.RawMessage
```

daemon 也会把 `task.Agent.McpConfig` 放进：

```go
execOpts.McpConfig
```

但当前 `codexBackend` 没有读取 `opts.McpConfig`，也没有把它转换成 Codex `config.toml` 的 `[mcp_servers.*]`。

因此：

```text
agent.mcp_config -> Codex
```

这条链路当前不存在。

如果你在 agent 设置里填了 `mcp_config`，Claude Code 会消费它，但 Codex 不会。

## Codex MCP 工具调用事件

Codex app-server 运行中可能发出 MCP tool call 相关通知。Multica 的 Codex client 识别这些通知，并把它们当作语义活动，避免任务被错误判定为无进展。

代码里可见的相关事件包括：

```text
item/mcpToolCall/progress
mcpServer/elicitation/request
```

其中 `mcpServer/elicitation/request` 会被 daemon 自动接受：

```go
c.respond(id, map[string]any{
    "action": "accept",
    "content": nil,
    "_meta": nil,
})
```

这说明 Codex runtime 可以处理 MCP server 交互，但 MCP server 的定义仍来自 Codex 自己的 config。

## 如何给 Codex 注入 MCP

当前正确做法是配置用户的 Codex home：

1. 找到共享 Codex home：

```text
$CODEX_HOME
```

如果没有设置，则是：

```text
~/.codex
```

2. 编辑：

```text
~/.codex/config.toml
```

或：

```text
$CODEX_HOME/config.toml
```

3. 添加 MCP server：

```toml
[mcp_servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/Users/me/project"]
```

4. 重启或重新触发 Multica daemon task。

daemon 在准备 Codex 任务时会复制这份配置到 per-task CODEX_HOME。

## 数据流总结

```mermaid
flowchart LR
    A["~/.codex/config.toml"] --> B["prepareCodexHome"]
    B --> C["{envRoot}/codex-home/config.toml"]
    C --> D["CODEX_HOME env var"]
    D --> E["codex app-server --listen stdio://"]
    E --> F["Codex loads [mcp_servers.*]"]
```

## 如果要支持 agent 级 MCP，代码需要改哪里

如果目标是像 Claude Code 一样让 Codex 也消费 `agent.mcp_config`，需要新增一条转换链路：

```text
agent.mcp_config JSON
  -> daemon ExecOptions.McpConfig
  -> Codex per-task config.toml [mcp_servers.*]
  -> CODEX_HOME/config.toml
```

可能改动点：

- 在 `execenv.PrepareParams` 或 `ReuseParams` 里增加 MCP 配置输入。
- 在 `prepareCodexHomeWithOpts` 后把 `mcp_config` 合并进 `{codexHome}/config.toml`。
- 定义 JSON MCP 配置到 Codex TOML `[mcp_servers.*]` 的转换规则。
- 明确优先级：agent.mcp_config 覆盖还是合并用户 `~/.codex/config.toml`。

当前源码还没有这条实现。

## 一句话总结

Codex 的 MCP 注入当前依赖 Codex CLI 自己的 `config.toml`：Multica 为每个任务创建 per-task `CODEX_HOME`，复制用户 `~/.codex/config.toml`，并让 `codex app-server` 从其中的 `[mcp_servers.*]` 读取 MCP server；Multica 的 `agent.mcp_config` 当前不会被 Codex backend 使用。
