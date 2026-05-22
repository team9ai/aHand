# Claude Code 的 MCP 注入方式

本文说明 Multica 中 Claude Code provider 如何接收和注入 MCP 配置。

## 结论

Claude Code 是三者里 MCP 注入链路最完整的一条：

```text
agent.mcp_config
  -> daemon claim task
  -> ExecOptions.McpConfig
  -> 写入临时 JSON 文件
  -> claude --mcp-config <temp-file> --strict-mcp-config
```

也就是说，Claude Code 使用的是 **agent 级别的 `mcp_config` 字段**，不是从用户全局 Claude 配置里隐式读取。

## 配置入口

Multica 的 agent API 支持 `mcp_config` 字段。

创建 agent 时可以传：

```json
{
  "name": "Claude Agent",
  "runtime_id": "runtime-uuid",
  "mcp_config": {
    "mcpServers": {
      "filesystem": {
        "command": "npx",
        "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
      }
    }
  }
}
```

更新 agent 时也可以传：

```json
{
  "mcp_config": {
    "mcpServers": {
      "github": {
        "command": "npx",
        "args": ["-y", "@modelcontextprotocol/server-github"],
        "env": {
          "GITHUB_PERSONAL_ACCESS_TOKEN": "..."
        }
      }
    }
  }
}
```

如果要清空 MCP 配置，传：

```json
{
  "mcp_config": null
}
```

相关代码：

- `server/internal/handler/agent.go`
- `server/pkg/db/queries/agent.sql`
- `server/migrations/046_agent_mcp_config.up.sql`

## 服务端如何保存

`agent` 表里有：

```sql
ALTER TABLE agent ADD COLUMN mcp_config jsonb;
```

创建 agent 时，`mcp_config` 会写入：

```sql
INSERT INTO agent (..., mcp_config, ...)
```

更新 agent 时，非空 `mcp_config` 会覆盖原值；请求里没有 `mcp_config` 字段则保留原值；显式传 `null` 会清空。

## 权限与脱敏

`mcp_config` 可能包含 token、API key、命令参数等敏感信息，所以读取 agent 时会按权限脱敏。

只有 agent owner 或 workspace owner/admin 可以看到原始 `mcp_config`。其他成员会看到：

```json
{
  "mcp_config": null,
  "mcp_config_redacted": true
}
```

相关逻辑：

- `redactMcpConfig`
- `canViewAgentEnv`

## daemon 如何拿到 MCP 配置

daemon claim task 时，服务端把 agent 信息带回去。agent 数据里包含：

```go
McpConfig json.RawMessage `json:"mcp_config,omitempty"`
```

daemon 执行任务前会把它塞进 `ExecOptions`：

```go
if task.Agent != nil {
    mcpConfig = task.Agent.McpConfig
}

execOpts := agent.ExecOptions{
    ...
    McpConfig: mcpConfig,
}
```

相关代码：

- `server/internal/daemon/daemon.go`
- `server/internal/daemon/types.go`
- `server/pkg/agent/agent.go`

## Claude backend 如何注入

Claude backend 收到 `opts.McpConfig` 后，会把原始 JSON 写入临时文件：

```go
path, err := writeMcpConfigToTemp(opts.McpConfig)
args = append(args, "--mcp-config", path)
```

临时文件路径类似：

```text
/tmp/multica-mcp-123456.json
```

任务结束后会删除这个临时文件。

相关代码：

- `server/pkg/agent/claude.go`

## Claude 启动命令

最终 Claude Code 命令形态是：

```text
claude
  -p
  --output-format stream-json
  --input-format stream-json
  --verbose
  --strict-mcp-config
  --permission-mode bypassPermissions
  --disallowedTools AskUserQuestion
  --mcp-config /tmp/multica-mcp-xxxx.json
```

重点是两个参数：

```text
--mcp-config <path>
--strict-mcp-config
```

`--mcp-config` 指向 Multica 为本次任务生成的 MCP 配置文件。

`--strict-mcp-config` 表示 Claude 只使用这份受控配置，避免继承外层 Claude Code 会话或用户环境里的其他 MCP server。

## custom_args 不能覆盖 MCP 协议参数

Multica 明确阻止用户通过 `custom_args` 覆盖这些协议关键参数：

```text
-p
--output-format
--input-format
--permission-mode
--mcp-config
```

其中 `--mcp-config` 被 daemon 管理，原因是：

- MCP 配置来自 agent 的 `mcp_config`。
- 临时文件由 daemon 创建和清理。
- 允许 custom args 覆盖会破坏安全边界和任务可复现性。

## MCP 配置示例

典型配置：

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": [
        "-y",
        "@modelcontextprotocol/server-filesystem",
        "/Users/me/project"
      ]
    },
    "postgres": {
      "command": "uvx",
      "args": ["mcp-server-postgres"],
      "env": {
        "DATABASE_URL": "postgres://user:pass@localhost:5432/app"
      }
    }
  }
}
```

这份 JSON 会原样写入临时文件，不会在 Go 侧做 schema 转换。

## 数据流总结

```mermaid
flowchart LR
    A["Agent settings / API"] --> B["agent.mcp_config JSONB"]
    B --> C["Task claim response"]
    C --> D["daemon task.Agent.McpConfig"]
    D --> E["ExecOptions.McpConfig"]
    E --> F["/tmp/multica-mcp-*.json"]
    F --> G["claude --mcp-config file --strict-mcp-config"]
```

## 注意事项

- Claude Code 这条链路支持 agent 级 MCP 配置。
- MCP secret 建议放在 `mcp_config.env` 或 agent `custom_env` 中。
- 非 owner/admin 读取 agent 时，`mcp_config` 会被脱敏。
- `mcp_config: null` 表示清空配置。
- 请求里不带 `mcp_config` 表示保留原配置。

## 一句话总结

Claude Code 的 MCP 注入是通过 agent 表的 `mcp_config` 字段实现的：Multica daemon 在任务执行时把该 JSON 写成临时文件，并用 `claude --mcp-config <file> --strict-mcp-config` 启动 Claude Code。
