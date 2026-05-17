# Claude Code 与 Multica 的数据交换格式

本文描述 Multica daemon 和 Claude Code CLI 之间的数据交换格式。这里讨论的是运行时协议层，也就是 daemon 启动 `claude` 后，通过 stdin/stdout 与 Claude Code 交换的 `stream-json` 消息。

## 传输方式

Multica 不通过 HTTP 调用 Claude Code，也不直接调用 Anthropic API。执行 Claude agent 时，daemon 会启动本地进程：

```text
claude -p --output-format stream-json --input-format stream-json ...
```

然后使用：

```text
stdin  -> Multica 写一条 JSON user message 给 Claude
stdout -> Claude 写一行一个 JSON 事件给 Multica
stderr -> Claude 日志和 CLI 崩溃信息
```

每一条 stdin/stdout 消息都是一行 JSON：

```text
{"type":"user",...}\n
```

核心代码在：

- `server/pkg/agent/claude.go`

## 启动命令

Multica 固定使用 Claude Code 的非交互 stream-json 模式：

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

可选追加参数：

```text
--model <model>
--max-turns <n>
--append-system-prompt <runtimeBrief>
--resume <session_id>
--mcp-config <temp-json-path>
```

## Multica -> Claude 的输入格式

Claude backend 启动进程后，会向 stdin 写入一条 user message：

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

写完后，Multica 会关闭 stdin。后续数据主要从 Claude stdout 读取。

## Claude -> Multica 的输出 envelope

Claude Code `stream-json` 输出的顶层 envelope 在 Multica 中对应这个结构：

```go
type claudeSDKMessage struct {
    Type       string          `json:"type"`
    Message    json.RawMessage `json:"message,omitempty"`
    Subtype    string          `json:"subtype,omitempty"`
    SessionID  string          `json:"session_id,omitempty"`
    ResultText string          `json:"result,omitempty"`
    IsError    bool            `json:"is_error,omitempty"`
    DurationMs float64         `json:"duration_ms,omitempty"`
    NumTurns   int             `json:"num_turns,omitempty"`
    Log        *claudeLogEntry `json:"log,omitempty"`
    RequestID  string          `json:"request_id,omitempty"`
    Request    json.RawMessage `json:"request,omitempty"`
}
```

Multica 当前主要处理这些 `type`：

| Claude `type` | Multica 处理 |
| --- | --- |
| `assistant` | 解析 assistant message，提取文本、thinking、tool use、usage |
| `user` | 解析 tool result |
| `system` | 提取 `session_id`，发送 running 状态 |
| `result` | 提取最终结果、最终 session id、错误状态 |
| `log` | 转成内部 log message |

## system 事件

Claude 运行初期或过程中会输出 system 事件，通常包含 session id：

```json
{
  "type": "system",
  "subtype": "init",
  "session_id": "claude-session-123"
}
```

Multica 会：

1. 记录 `session_id`。
2. 向内部流发送 running 状态。

内部映射：

```go
Message{
    Type:      MessageStatus,
    Status:    "running",
    SessionID: "claude-session-123",
}
```

## assistant 事件

Claude 的模型输出在 `assistant` 事件里：

```json
{
  "type": "assistant",
  "message": {
    "role": "assistant",
    "model": "claude-sonnet-4-6",
    "content": [
      {
        "type": "text",
        "text": "我会先检查项目结构。"
      }
    ],
    "usage": {
      "input_tokens": 100,
      "output_tokens": 20,
      "cache_read_input_tokens": 10,
      "cache_creation_input_tokens": 5
    }
  }
}
```

Multica 会解析 `message` 字段，并遍历 `content` 数组。

## assistant text block

文本块格式：

```json
{
  "type": "text",
  "text": "执行完成。"
}
```

Multica 映射成：

```go
Message{
    Type:    MessageText,
    Content: "执行完成。",
}
```

这些文本也会累积成最终 `Result.Output`。不过如果 Claude 最后输出 `result.result`，Multica 会用最终 result 覆盖累积文本。

## assistant thinking block

thinking 块格式：

```json
{
  "type": "thinking",
  "text": "需要先找入口文件。"
}
```

Multica 映射成：

```go
Message{
    Type:    MessageThinking,
    Content: "需要先找入口文件。",
}
```

## assistant tool_use block

Claude 发起工具调用时，assistant message 里会包含 `tool_use`：

```json
{
  "type": "assistant",
  "message": {
    "role": "assistant",
    "model": "claude-sonnet-4-6",
    "content": [
      {
        "type": "tool_use",
        "id": "toolu_abc123",
        "name": "Bash",
        "input": {
          "command": "ls -la"
        }
      }
    ],
    "usage": {
      "input_tokens": 120,
      "output_tokens": 30
    }
  }
}
```

Multica 映射成：

```go
Message{
    Type:   MessageToolUse,
    Tool:   "Bash",
    CallID: "toolu_abc123",
    Input: map[string]any{
        "command": "ls -la",
    },
}
```

字段映射：

| Claude 字段 | Multica 字段 |
| --- | --- |
| `content[].name` | `Message.Tool` |
| `content[].id` | `Message.CallID` |
| `content[].input` | `Message.Input` |

## user tool_result 事件

Claude Code 的工具结果会作为 `user` 类型事件回流：

```json
{
  "type": "user",
  "message": {
    "role": "user",
    "content": [
      {
        "type": "tool_result",
        "tool_use_id": "toolu_abc123",
        "content": "file1.go\nfile2.go\n"
      }
    ]
  }
}
```

Multica 映射成：

```go
Message{
    Type:   MessageToolResult,
    CallID: "toolu_abc123",
    Output: "file1.go\nfile2.go\n",
}
```

字段映射：

| Claude 字段 | Multica 字段 |
| --- | --- |
| `content[].tool_use_id` | `Message.CallID` |
| `content[].content` | `Message.Output` |

注意：当前实现把 `content` 的 raw JSON 直接转成字符串。如果 Claude 输出的是 JSON string，内部字符串可能保留 JSON 编码形态。

## usage 数据

Claude 的 usage 位于 assistant message 的 `usage` 字段：

```json
{
  "usage": {
    "input_tokens": 100,
    "output_tokens": 20,
    "cache_read_input_tokens": 10,
    "cache_creation_input_tokens": 5
  }
}
```

Multica 按 `message.model` 分组累加：

```go
usage[model].InputTokens += input_tokens
usage[model].OutputTokens += output_tokens
usage[model].CacheReadTokens += cache_read_input_tokens
usage[model].CacheWriteTokens += cache_creation_input_tokens
```

内部结构：

```go
TokenUsage{
    InputTokens:      usage.input_tokens,
    OutputTokens:     usage.output_tokens,
    CacheReadTokens:  usage.cache_read_input_tokens,
    CacheWriteTokens: usage.cache_creation_input_tokens,
}
```

和 Hermes 不同，Claude usage 是按 assistant message 累加；Hermes 的 `usage_update` 更像累计快照。

## result 事件

Claude 结束时会输出 `result` 事件：

```json
{
  "type": "result",
  "subtype": "success",
  "session_id": "claude-session-123",
  "result": "最终回答内容",
  "is_error": false,
  "duration_ms": 12345,
  "num_turns": 3
}
```

Multica 会：

1. 关闭 stdin。
2. 记录最终 `session_id`。
3. 如果 `result` 非空，用它覆盖之前累积的 assistant text。
4. 如果 `is_error=true`，把任务标记为 failed，并把 `result` 当作错误文本。

成功映射：

```go
Result{
    Status:    "completed",
    Output:    "最终回答内容",
    SessionID: "claude-session-123",
    Usage:     usage,
}
```

失败映射：

```go
Result{
    Status:    "failed",
    Error:     "错误文本",
    SessionID: "claude-session-123",
    Usage:     usage,
}
```

## log 事件

Claude 的日志事件格式：

```json
{
  "type": "log",
  "log": {
    "level": "warn",
    "message": "some warning"
  }
}
```

Multica 映射成：

```go
Message{
    Type:    MessageLog,
    Level:   "warn",
    Content: "some warning",
}
```

## control_request / control_response

代码里保留了 Claude control request 的响应逻辑：

```json
{
  "type": "control_request",
  "request_id": "req_123",
  "request": {
    "subtype": "permission",
    "tool_name": "Bash",
    "input": {
      "command": "ls"
    }
  }
}
```

对应响应格式：

```json
{
  "type": "control_response",
  "response": {
    "subtype": "success",
    "request_id": "req_123",
    "response": {
      "behavior": "allow",
      "updatedInput": {
        "command": "ls"
      }
    }
  }
}
```

不过当前主执行路径启动 Claude 时使用：

```text
--permission-mode bypassPermissions
```

并且写完 user message 后会关闭 stdin，所以正常 Hermes 式的“运行中反向请求授权”不是 Claude 这条路径的主要机制。Claude 的无人值守授权主要依赖 `bypassPermissions`。

## stderr 数据

Claude 的 stderr 不走 stream-json。Multica 会：

1. 把 stderr 写入 daemon 日志，前缀类似 `[claude:stderr]`。
2. 保留一个有限大小的 stderr tail。
3. 如果 Claude 启动失败、写 stdin 失败、进程异常退出或任务失败，把 stderr tail 拼进 `Result.Error`。

这样用户不会只看到：

```text
claude exited with error: exit status 3
```

而是能看到最后一段 Claude stderr，方便定位 CLI 崩溃、权限、依赖或认证问题。

## 超时和取消

Claude backend 有任务超时控制。最终状态规则大致是：

| 条件 | Result.Status |
| --- | --- |
| context deadline exceeded | `timeout` |
| context canceled | `aborted` |
| Claude result `is_error=true` | `failed` |
| 进程异常退出且之前未失败 | `failed` |
| 正常结束 | `completed` |

超时时错误文本类似：

```text
claude timed out after 20m0s
```

## 会话恢复的数据

恢复会话不是通过 stdin JSON 字段，而是通过 CLI 参数：

```text
--resume <session_id>
```

Claude 在 stdout 的 `system` 或 `result` 事件里返回：

```json
{
  "session_id": "claude-session-123"
}
```

Multica 会把最终 session id 放入：

```go
Result.SessionID
```

如果请求了 resume，但 Claude 返回了不同 session id 并且本次失败，Multica 会清空最终 `SessionID`，让 daemon 触发 fresh session retry。

## MCP 配置的数据形态

Claude 的 MCP 配置不通过 stdin 传。Multica 会把 agent 的 `mcp_config` 原始 JSON 写入临时文件：

```json
{
  "mcpServers": {
    "server-name": {
      "command": "some-command",
      "args": []
    }
  }
}
```

然后通过 CLI 参数传给 Claude：

```text
--mcp-config /tmp/multica-mcp-xxxx.json
```

同时启用：

```text
--strict-mcp-config
```

确保 Claude 使用的是 Multica 为本次 agent 配置的 MCP 集合。

## Multica 内部消息格式

Claude 的 stream-json 事件最终会归一化为 Multica 的内部 `Message`：

```go
type Message struct {
    Type      MessageType
    Content   string
    Tool      string
    CallID    string
    Input     map[string]any
    Output    string
    Status    string
    Level     string
    SessionID string
}
```

最终任务结果是：

```go
type Result struct {
    Status     string
    Output     string
    Error      string
    DurationMs int64
    SessionID  string
    Usage      map[string]TokenUsage
}
```

字段来源：

| Result 字段 | 来源 |
| --- | --- |
| `Status` | `result.is_error`、进程退出状态、timeout、cancel |
| `Output` | assistant text 累积，或最终 `result.result` |
| `Error` | `result.result` 错误文本、进程错误、stderr tail |
| `SessionID` | `system.session_id` 或 `result.session_id` |
| `Usage` | assistant message 的 `usage`，按 model 累加 |

## 和 Hermes 数据交换的主要差异

| 维度 | Claude Code | Hermes |
| --- | --- | --- |
| 协议 | Claude Code `stream-json` | ACP JSON-RPC 2.0 |
| 启动命令 | `claude -p --output-format stream-json --input-format stream-json` | `hermes acp` |
| 请求关联 | 没有 JSON-RPC id，靠事件流 | 每个 request 有 `id` |
| prompt 输入 | stdin 写一条 `type=user` JSON | `session/prompt` RPC |
| 会话创建 | Claude CLI 内部处理 | `session/new` RPC |
| 会话恢复 | `--resume <session_id>` CLI 参数 | `session/resume` RPC |
| 模型设置 | `--model <model>` CLI 参数 | `session/set_model` RPC |
| 工具授权 | `--permission-mode bypassPermissions` | `session/request_permission` 自动 approve，且 Hermes 设 `HERMES_YOLO_MODE=1` |
| 上下文文件 | `CLAUDE.md` | `AGENTS.md` |
| skill 路径 | `.claude/skills/` 原生发现 | `.agent_context/skills/` fallback |
| usage | assistant message usage 累加 | usage update / turn end / prompt result |

## 一句话总结

Claude Code 和 Multica 的运行时数据交换是基于 Claude CLI 的 `stream-json` 行协议：Multica 启动 `claude -p`，通过 stdin 发送一条 `type=user` JSON 任务，通过 stdout 读取 `system`、`assistant`、`user`、`result`、`log` 事件，再把文本、thinking、工具调用、工具结果、usage、session id 和最终结果归一化成 Multica 的 `Message` 和 `Result`。
