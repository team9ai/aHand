# Hermes 与 Multica 的数据交换格式

本文描述 Multica daemon 和 Hermes CLI 之间的数据交换格式。这里讨论的是运行时协议层，也就是 daemon 启动 `hermes acp` 后，通过 stdin/stdout 与 Hermes 交换的 JSON-RPC 消息。

## 传输方式

Multica 不通过 HTTP 调用 Hermes，也不直接调用 LLM API。执行 Hermes agent 时，daemon 会启动本地进程：

```text
hermes acp
```

然后使用：

```text
stdin  -> Multica 写 JSON-RPC 请求给 Hermes
stdout -> Hermes 写 JSON-RPC 响应和通知给 Multica
stderr -> Hermes 日志和 provider 错误
```

每一条 JSON-RPC 消息都是一行 JSON：

```text
{"jsonrpc":"2.0",...}\n
```

核心代码在：

- `server/pkg/agent/hermes.go`

## JSON-RPC 基本形态

Multica 发给 Hermes 的请求统一是：

```json
{
  "jsonrpc": "2.0",
  "id": 0,
  "method": "method/name",
  "params": {}
}
```

Hermes 对请求的响应是：

```json
{
  "jsonrpc": "2.0",
  "id": 0,
  "result": {}
}
```

失败响应是：

```json
{
  "jsonrpc": "2.0",
  "id": 0,
  "error": {
    "code": -32603,
    "message": "Internal error",
    "data": "provider-specific detail"
  }
}
```

Hermes 主动推送的流式事件是 notification，没有 `id`：

```json
{
  "jsonrpc": "2.0",
  "method": "session/update",
  "params": {
    "sessionId": "ses_1",
    "update": {}
  }
}
```

Multica 也兼容另一种 notification 方法名：

```text
session/notification
```

## 执行一次任务的请求序列

一次 Hermes 任务通常经历以下 RPC：

```text
initialize
session/new 或 session/resume
session/set_model  可选
session/prompt
```

### 1. initialize

Multica 首先发起初始化握手：

```json
{
  "jsonrpc": "2.0",
  "id": 0,
  "method": "initialize",
  "params": {
    "protocolVersion": 1,
    "clientInfo": {
      "name": "multica-agent-sdk",
      "version": "0.2.0"
    },
    "clientCapabilities": {}
  }
}
```

Hermes 返回 `result` 后，Multica 才继续创建或恢复 session。

## 2. session/new

没有历史会话时，Multica 创建新 session：

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "session/new",
  "params": {
    "cwd": "/path/to/task/workdir",
    "mcpServers": [],
    "model": "provider:model"
  }
}
```

字段说明：

| 字段 | 说明 |
| --- | --- |
| `cwd` | Hermes 运行的任务工作目录 |
| `mcpServers` | 当前 Hermes backend 固定传空数组 |
| `model` | 可选；只有用户配置了模型时才传 |

如果没有显式模型，`model` 字段会省略：

```json
{
  "cwd": "/path/to/task/workdir",
  "mcpServers": []
}
```

Hermes 返回的结果里需要包含 session id：

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "sessionId": "ses_abc123"
  }
}
```

Multica 会把 `sessionId` 保存为后续 resume 的依据。

## 3. session/resume

如果任务有历史 session，Multica 会恢复会话：

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "session/resume",
  "params": {
    "cwd": "/path/to/task/workdir",
    "sessionId": "ses_abc123"
  }
}
```

Hermes 返回：

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "sessionId": "ses_abc123"
  }
}
```

如果 Hermes 返回了不同的 `sessionId`，Multica 会以 Hermes 返回值为准。这用于处理本地 Hermes 会话库丢失后，Hermes 自动创建新 session 的情况。

## 4. session/set_model

如果 agent 或 runtime 配置了模型，Multica 会在发送 prompt 前切换模型：

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "session/set_model",
  "params": {
    "sessionId": "ses_abc123",
    "modelId": "provider:model"
  }
}
```

如果这个 RPC 失败，Multica 会让任务失败，不会静默回退到 Hermes 默认模型。

## 5. session/prompt

真正的任务内容通过 `session/prompt` 发送：

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "session/prompt",
  "params": {
    "sessionId": "ses_abc123",
    "prompt": [
      {
        "type": "text",
        "text": "具体任务 prompt"
      }
    ]
  }
}
```

注意：Multica 不会把完整 system prompt 拼进这里。Hermes 的上下文主要通过任务工作目录里的文件读取，例如：

```text
AGENTS.md
.agent_context/skills/
```

## session/prompt 响应

Hermes 在 prompt 完成后会返回：

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "result": {
    "stopReason": "end_turn",
    "usage": {
      "inputTokens": 1000,
      "outputTokens": 200,
      "totalTokens": 1200,
      "thoughtTokens": 0,
      "cachedReadTokens": 50
    }
  }
}
```

Multica 读取的字段是：

| 字段 | Multica 用途 |
| --- | --- |
| `stopReason` | 判断是否 cancelled |
| `usage.inputTokens` | 输入 token |
| `usage.outputTokens` | 输出 token |
| `usage.cachedReadTokens` | cache read token |

`totalTokens` 和 `thoughtTokens` 当前不会进入最终 `TokenUsage`。

## Hermes 流式通知格式

Hermes 的主要运行过程通过 notification 推给 Multica：

```json
{
  "jsonrpc": "2.0",
  "method": "session/update",
  "params": {
    "sessionId": "ses_abc123",
    "update": {
      "sessionUpdate": "agent_message_chunk",
      "content": {
        "type": "text",
        "text": "Hello"
      }
    }
  }
}
```

Multica 兼容三种 update 类型写法。

### 写法 A：`sessionUpdate`

```json
{
  "sessionUpdate": "agent_message_chunk",
  "content": {
    "type": "text",
    "text": "Hello"
  }
}
```

### 写法 B：`type`

```json
{
  "type": "AgentMessageChunk",
  "content": {
    "type": "text",
    "text": "Hello"
  }
}
```

### 写法 C：externally tagged object

```json
{
  "agentMessageChunk": {
    "content": {
      "type": "text",
      "text": "Hello"
    }
  }
}
```

Multica 会把这些形式归一化成内部 update type。

## 支持的 update type

Multica 当前处理这些 Hermes / ACP update：

| ACP update | Multica 内部消息 |
| --- | --- |
| `agent_message_chunk` | `MessageText` |
| `agent_thought_chunk` | `MessageThinking` |
| `tool_call` | `MessageToolUse` 或缓冲 |
| `tool_call_update` | `MessageToolUse` + `MessageToolResult` |
| `usage_update` | 更新 token usage 快照 |
| `turn_end` / `end_turn` | 提取 stop reason 和 usage |

## agent_message_chunk

Hermes 输出给用户的文本 chunk：

```json
{
  "jsonrpc": "2.0",
  "method": "session/update",
  "params": {
    "sessionId": "ses_1",
    "update": {
      "sessionUpdate": "agent_message_chunk",
      "content": {
        "type": "text",
        "text": "Hello world"
      }
    }
  }
}
```

Multica 映射成：

```go
Message{
    Type:    MessageText,
    Content: "Hello world",
}
```

这些文本也会累积成最终 `Result.Output`。

## agent_thought_chunk

Hermes 的思考过程 chunk：

```json
{
  "jsonrpc": "2.0",
  "method": "session/update",
  "params": {
    "sessionId": "ses_1",
    "update": {
      "sessionUpdate": "agent_thought_chunk",
      "content": {
        "type": "text",
        "text": "Let me think..."
      }
    }
  }
}
```

Multica 映射成：

```go
Message{
    Type:    MessageThinking,
    Content: "Let me think...",
}
```

## tool_call

Hermes 发起工具调用时，会发送 `tool_call`：

```json
{
  "jsonrpc": "2.0",
  "method": "session/update",
  "params": {
    "sessionId": "ses_1",
    "update": {
      "sessionUpdate": "tool_call",
      "toolCallId": "tc-abc123",
      "title": "terminal: ls -la",
      "kind": "execute",
      "status": "pending",
      "rawInput": {
        "command": "ls -la"
      }
    }
  }
}
```

Multica 会从这些字段里提取工具信息：

| Hermes 字段 | Multica 用途 |
| --- | --- |
| `toolCallId` | `Message.CallID` |
| `title` | 推断工具名 |
| `kind` | 辅助推断工具名 |
| `rawInput` | 工具输入 |
| `input` | `rawInput` 为空时的 fallback |
| `parameters` | `input` 为空时的 fallback |

上例会映射成：

```go
Message{
    Type:   MessageToolUse,
    Tool:   "terminal",
    CallID: "tc-abc123",
    Input: map[string]any{
        "command": "ls -la",
    },
}
```

## tool name 推断规则

Hermes 的工具名主要从 `title` 里推断。例如：

```text
terminal: ls -la -> terminal
read: /path/file  -> read
execute code      -> execute_code
```

如果 `title` 无法推断，Multica 会 fallback 到 `name` 字段。

## tool_call_update

工具完成时，Hermes 会发送 `tool_call_update`：

```json
{
  "jsonrpc": "2.0",
  "method": "session/update",
  "params": {
    "sessionId": "ses_1",
    "update": {
      "sessionUpdate": "tool_call_update",
      "toolCallId": "tc-abc123",
      "status": "completed",
      "kind": "execute",
      "rawOutput": "file1.go\nfile2.go\n"
    }
  }
}
```

Multica 映射成：

```go
Message{
    Type:   MessageToolResult,
    CallID: "tc-abc123",
    Output: "file1.go\nfile2.go\n",
}
```

输出字段优先级是：

```text
rawOutput
output
content[].text
```

如果 `status` 不是 `completed` 或 `failed`，Multica 通常只缓冲，不立即渲染结果。

## content block 格式

有些 ACP 实现会把工具参数或输出放在 `content` 数组里：

```json
{
  "content": [
    {
      "type": "content",
      "content": {
        "type": "text",
        "text": "hi\n"
      }
    }
  ]
}
```

Multica 会拼接所有 text block。

文件 diff block 会被压缩成简短说明：

```json
{
  "type": "diff",
  "path": "app.go",
  "oldText": "old",
  "newText": "new"
}
```

会被渲染成类似：

```text
--- app.go
+++ app.go
(edited: 3 -> 3 bytes)
```

Multica 不会把完整 diff 全量塞进 tool result，避免输出过大。

## usage_update

Hermes 可以在运行过程中推送 token usage 快照：

```json
{
  "jsonrpc": "2.0",
  "method": "session/update",
  "params": {
    "sessionId": "ses_1",
    "update": {
      "sessionUpdate": "usage_update",
      "usage": {
        "inputTokens": 500,
        "outputTokens": 200,
        "cachedReadTokens": 100
      }
    }
  }
}
```

Multica 会把 usage 当成累计快照处理：如果新的数字更大，就覆盖当前值。

内部字段映射：

```go
TokenUsage{
    InputTokens:     usage.inputTokens,
    OutputTokens:    usage.outputTokens,
    CacheReadTokens: usage.cachedReadTokens,
}
```

## turn_end

有些 ACP 实现会通过 notification 发送 turn end：

```json
{
  "jsonrpc": "2.0",
  "method": "session/notification",
  "params": {
    "sessionId": "ses_1",
    "update": {
      "type": "TurnEnd",
      "stopReason": "end_turn",
      "usage": {
        "inputTokens": 3,
        "outputTokens": 4,
        "cachedReadTokens": 1
      }
    }
  }
}
```

Multica 会像处理 `session/prompt` response 一样提取 `stopReason` 和 usage。

## Hermes -> Multica 的反向请求

除了 notification，Hermes 也可能向 Multica 发起 JSON-RPC request。Multica 当前主要处理：

```text
session/request_permission
```

请求形态：

```json
{
  "jsonrpc": "2.0",
  "id": 100,
  "method": "session/request_permission",
  "params": {}
}
```

Multica 是无人值守 daemon，所以会自动批准：

```json
{
  "jsonrpc": "2.0",
  "id": 100,
  "result": {
    "outcome": {
      "outcome": "selected",
      "optionId": "approve_for_session"
    }
  }
}
```

对于未知 Hermes -> Multica request，Multica 返回 JSON-RPC method not found：

```json
{
  "jsonrpc": "2.0",
  "id": 100,
  "error": {
    "code": -32601,
    "message": "method not found: unknown/method"
  }
}
```

## stderr 数据

Hermes 的 stderr 不走 JSON-RPC。Multica 会做两件事：

1. 把 stderr 写入 daemon 日志，前缀类似 `[hermes:stderr]`。
2. 解析 provider 层错误，例如 429、认证失败、额度不足等。

这是因为某些情况下 Hermes 的 `session/prompt` 仍可能返回 `stopReason=end_turn`，但真实 provider 错误只出现在 stderr 或 agent text 中。Multica 会在最终结果阶段提升这些错误，避免把失败误报成成功。

## Multica 内部结果格式

Hermes 的流式事件最终会归一化为 Multica 的内部 `Message`：

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

其中：

| Result 字段 | 来源 |
| --- | --- |
| `Status` | RPC 是否成功、是否 timeout、是否 cancelled、stderr 错误提升 |
| `Output` | `agent_message_chunk.content.text` 累积 |
| `Error` | JSON-RPC error、timeout、provider 错误 |
| `SessionID` | `session/new` 或 `session/resume` 返回 |
| `Usage` | `usage_update`、`turn_end`、`session/prompt.result.usage` |

## 模型发现的数据格式

Multica 的 Hermes 模型发现也走 ACP。它会临时启动一个 `hermes acp`，执行最小握手和 `session/new`，然后读取响应中的模型信息。

期望形态类似：

```json
{
  "sessionId": "ses_model_discovery",
  "models": {
    "availableModels": [
      {
        "modelId": "provider:model-a",
        "displayName": "Model A"
      }
    ],
    "currentModelId": "provider:model-a"
  }
}
```

Multica 会把 `availableModels` 转成 UI 可选模型，并用 `currentModelId` 标识默认模型。

如果 Hermes 不存在、未登录、配置错误或响应里没有模型列表，Multica 返回空模型列表，让 UI fallback 到手动输入。

## 一句话总结

Hermes 和 Multica 的运行时数据交换是基于 `hermes acp` 的 JSON-RPC 2.0 行协议：Multica 发送 `initialize`、`session/new` / `session/resume`、`session/set_model`、`session/prompt`；Hermes 通过 `session/update` / `session/notification` 推送文本、思考、工具调用、usage 和 turn end；Multica 再把这些 ACP 帧归一化为自己的 `Message` 和 `Result`，写回任务队列和 UI。
