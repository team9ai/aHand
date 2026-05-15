# cc-connect 架构与对接总览

本文提供 cc-connect 的高层概览，说明它如何作为中间层连接外部入口、核心编排逻辑和后端 Agent/服务。协议字段、平台配置和适配器实现细节请参考对应专项文档。

## 1. 整体定位

cc-connect 是一个桥接进程，运行在用户机器或服务器上。它负责把外部系统的请求转换成统一的消息流，交给内部 Engine 管理会话、权限和路由，再转发给后端 AI Agent 或相关服务执行。

```
┌──────────────────────────────────────────────┐
│                  外部入口                     │
│                                              │
│  聊天平台 / 自定义适配器 / Web UI / 管理工具  │
└──────────────────────┬───────────────────────┘
                       │
                       ▼
┌──────────────────────────────────────────────┐
│              cc-connect 中间层                │
│                                              │
│  平台接入 → Core Engine → 会话/权限/路由       │
└──────────────────────┬───────────────────────┘
                       │
                       ▼
┌──────────────────────────────────────────────┐
│                  后端服务                     │
│                                              │
│  Agent CLI / ACP Agent                       │
└──────────────────────────────────────────────┘
```

## 2. 外部如何访问中间层

外部系统主要通过三种方式访问 cc-connect：

| 入口 | 面向对象 | 连接方式 | 用途 |
|------|----------|----------|------|
| 原生聊天平台 | 飞书、Telegram、Slack、Discord、钉钉、企业微信等 | 平台各自的 WebSocket、Long Polling、Gateway、Webhook 等 | 用户在聊天工具里和 Agent 对话 |
| Bridge | 自定义平台、脚本、内部系统、自定义 UI | WebSocket + REST | 让外部系统以统一方式接入 cc-connect |
| Management API / Web UI | 管理后台、TUI、桌面端、运维系统 | HTTP REST | 管理项目、会话、配置、定时任务和运行状态 |

粗略链路如下：

```
外部用户或系统
    │
    ▼
聊天平台 / Bridge / Management API
    │
    ▼
cc-connect
```

其中：

- 聊天平台入口适合正常用户对话。
- Bridge 适合第三方或内部系统做自定义接入。
- Management API 适合管理和监控，不是主要对话通道。

### 2.1 聊天平台和 cc-connect 如何通信

聊天平台和 cc-connect 之间不是一套统一协议，而是由每个平台自己的机器人协议决定。cc-connect 在 `platform/*` 中为不同平台实现接入层，把平台事件转换成内部统一消息，再把内部回复转换回平台消息。

从实现方式看，主要有两类：

| 方式 | 谁主动连接 | 典型协议 | 说明 |
|------|------------|----------|------|
| 主动拉取或长连接 | cc-connect 主动连平台 | WebSocket、Long Polling、Gateway、Stream | 不需要公网地址，cc-connect 用平台 token 建立连接并持续接收事件 |
| 平台回调 | 平台主动请求 cc-connect | HTTP Webhook | 需要 cc-connect 暴露可访问的 HTTP 地址，平台把用户消息 POST 到回调地址 |

传输的信息可以分为入站和出站两类：

| 方向 | 传输内容 | 中间层如何使用 |
|------|----------|----------------|
| 平台 → cc-connect | 用户 ID、聊天/群组 ID、消息 ID、文本、图片、文件、语音、按钮点击、回复上下文等 | 生成内部消息，确定用户、会话、项目和要发送给 Agent 的内容 |
| cc-connect → 平台 | 文本回复、流式进度、卡片、按钮、图片、文件、权限确认提示、错误提示等 | 把 Agent 输出和中间状态展示回原聊天窗口 |

概念上，平台侧传来的消息会被整理成“谁在什么会话里说了什么，并带了哪些附件”。cc-connect 返回平台的消息则是“Agent 当前回复了什么，需要用户确认什么，或需要展示哪些结果”。

## 3. 中间层做什么

cc-connect 中间层的核心是 `core.Engine`。它把不同入口的差异屏蔽掉，对内提供统一的会话和消息处理流程。

主要职责：

| 能力 | 说明 |
|------|------|
| 消息归一化 | 把不同平台或外部入口的输入转换成统一消息 |
| 会话管理 | 维护用户会话、命名会话、历史记录和当前活跃会话 |
| 路由 | 根据项目、平台和会话把消息发给正确的 Agent |
| 权限处理 | 将 Agent 的工具执行请求转换成用户可确认的交互 |
| 回复渲染 | 把 Agent 输出转换成外部入口可展示的文本、卡片、按钮、图片或文件 |
| 能力降级 | 当某个入口不支持富消息时，降级为纯文本 |
| 管理能力 | 支持项目、设置、定时任务、Provider 等运行时管理 |

从概念上看，中间层既不绑定某个聊天平台，也不绑定某个 Agent。平台和 Agent 都是可替换的两侧连接点。

## 4. 中间层如何访问后端

cc-connect 直接访问的后端主要是 Agent。Agent 分为两类：一种是具体产品自己的 CLI Agent，另一种是遵循 Agent Client Protocol 的 ACP Agent。cc-connect 负责启动或连接 Agent、发送用户输入、接收 Agent 事件；模型 Provider、API 中转和项目文件通常是 Agent 的下游。

### 4.1 cc-connect 到 Agent 的连接

| Agent 类型 | cc-connect 如何连接 | 具体实现 | 传输内容 |
|------------|---------------------|----------|----------|
| CLI Agent | cc-connect 启动本地 CLI 子进程 | 按不同 CLI 的命令行协议传参，把 stdout 中的 JSON/流式事件解析成内部事件 | 用户 prompt、附件路径、权限响应；Agent 的文本、工具状态、权限请求、最终结果 |
| ACP Agent | cc-connect 作为 ACP Client 启动或连接 ACP Agent 进程 | 通过 newline-delimited JSON-RPC 2.0 over stdio 通信 | 标准化的会话、prompt、事件、工具调用和权限消息 |

当前主要 Agent 的连接方式如下，Claude Code、Codex 和 ACP 的细节见后续独立章节：

| Agent | cc-connect 启动方式 | 输入如何传给 Agent | 输出如何读回 |
|-------|---------------------|--------------------|--------------|
| Claude Code | 长驻 `claude --output-format stream-json --input-format stream-json --permission-prompt-tool stdio` 进程 | 往 stdin 写 `stream-json` 用户消息；权限结果也通过 stdio 返回 | 从 stdout 持续读取 `stream-json` 事件 |
| Codex | 每轮执行 `codex exec ... --json -`；续接时执行 `codex exec resume <thread_id> ... --json -` | prompt 通过 stdin 传入；图片通过 `--image`，工作目录通过 `--cd` 或进程 cwd | 从 stdout 读取 JSON lines |
| ACP | 启动配置中的 `command` + `args` | 通过 JSON-RPC 方法发送会话和 prompt | 从 stdout 读取 JSON-RPC 响应、通知和服务端请求 |
| Gemini CLI | 每轮执行 `gemini --output-format stream-json -p -`；续接时加 `--resume <session_id>` | prompt 通过 stdin 传入；附件先落盘并把路径写入 prompt | 从 stdout 读取 `stream-json` 事件 |
| Cursor Agent | 每轮执行 `agent --print --output-format stream-json --workspace <dir> -- <prompt>`；续接时加 `--resume <session_id>` | prompt 作为命令参数传入；文件附件先落盘并把路径写入 prompt | 从 stdout 读取 `stream-json` 事件 |
| OpenCode | 每轮执行 `opencode run --format json --dir <dir> <prompt>`；续接时加 `--session <session_id>` | prompt 作为命令参数传入；图片通过 `--file`，文件附件路径写入 prompt | 从 stdout 读取 JSON lines |
| Qoder | 每轮执行 `qodercli -p <prompt> -f stream-json -q -w <dir>`；续接时加 `-r <session_id>` | prompt 通过 `-p` 传入；文件附件先落盘并把路径写入 prompt | 从 stdout 读取 `stream-json` 事件 |

这部分是 cc-connect 和后端的主连接：

```
cc-connect Core Engine
    │
    ▼
Agent 适配层
    │
    ├── CLI Agent：按各 CLI 的命令行协议启动子进程并读取 stdout 事件
    │
    └── ACP Agent：newline-delimited JSON-RPC 2.0 over stdio
```

### 4.2 Agent 后面连接什么

Agent 收到 cc-connect 发来的输入后，通常继续连接两类下游：

| Agent 下游 | 谁发起连接 | 连接方式 | 用途 |
|------------|------------|----------|------|
| 项目文件系统 | Agent | 本地文件 I/O | 读取代码、编辑文件、运行测试、执行工具 |
| 模型 Provider / API 中转 | Agent | HTTPS API | 调用大模型或模型中转服务 |

完整链路可以概括为：

```
外部入口
    │
    ▼
cc-connect
    │
    ▼
Agent CLI / ACP Agent
    │
    ├── 本地项目文件
    └── 模型 Provider / API 中转
```

### 4.3 cc-connect 自己为什么访问文件系统

本地文件系统不算独立后端服务，但会被两类组件使用：

| 使用方 | 访问内容 | 用途 |
|--------|----------|------|
| cc-connect | 配置、会话状态、项目状态、平台收到的附件 | 保存运行状态，把图片/文件等附件落盘后交给 Agent 使用 |
| Agent | 项目工作目录里的代码和文件 | 阅读、编辑、运行测试或执行工具 |

## 5. Claude Code Agent

Claude Code 是长驻 CLI Agent。cc-connect 启动一个持续运行的 `claude` 子进程，并通过 Claude Code 的 `stream-json` + `stdio` 协议进行双向通信。

### 5.1 连接方式

cc-connect 构造的核心启动参数是：

```text
claude
  --output-format stream-json
  --input-format stream-json
  --permission-prompt-tool stdio
```

根据项目配置，还会追加模型、权限模式、恢复会话、允许/禁止工具、系统提示等参数，例如：

```text
--model <model>
--permission-mode <mode>
--resume <session_id>
--allowedTools <tools>
--disallowedTools <tools>
--append-system-prompt <prompt>
```

### 5.2 数据如何传输

| 方向 | 传输方式 | 内容 |
|------|----------|------|
| cc-connect → Claude | stdin 写入 `stream-json` | 用户文本、图片内容、文件路径提示、权限响应 |
| Claude → cc-connect | stdout 输出 `stream-json` | 文本增量、思考内容、工具调用、权限请求、结果、错误 |

Claude Code 的特点是同一个子进程可以承载持续对话。cc-connect 持有该进程的 stdin/stdout，并把 Claude 输出的事件转换成内部 `core.Event`，再交给 Engine 渲染回聊天平台或 Bridge。

### 5.3 下游访问

Claude Code 自己负责访问项目工作目录、执行工具和调用模型 Provider。cc-connect 主要负责：

- 启动 Claude Code 进程。
- 注入工作目录、模型、Provider 环境变量和权限模式。
- 把外部消息转换成 Claude Code 输入。
- 把 Claude Code 事件转换成外部入口可展示的回复。

## 6. Codex Agent

Codex 是按轮次启动的 CLI Agent。cc-connect 每次收到用户输入时启动一次 `codex exec` 子进程；如果已有 Codex thread，则使用 `codex exec resume` 续接上下文。

### 6.1 连接方式

新会话的核心命令形态是：

```text
codex exec --skip-git-repo-check --json --cd <work_dir> -
```

续接已有会话时使用：

```text
codex exec resume --skip-git-repo-check <thread_id> --json -
```

根据配置，还会追加：

```text
--model <model>
-c model_provider=<provider>
-c openai_base_url=<base_url>
-c model_reasoning_effort=<effort>
--full-auto
--dangerously-bypass-approvals-and-sandbox
--image <image_path>
```

### 6.2 数据如何传输

| 方向 | 传输方式 | 内容 |
|------|----------|------|
| cc-connect → Codex | prompt 通过 stdin 传入；图片通过 `--image` 参数传入；文件先落盘并把路径写入 prompt | 用户文本、图片路径、文件路径提示、模型和 Provider 参数 |
| Codex → cc-connect | stdout 输出 JSON lines | assistant 文本、工具事件、thread id、错误和结果 |

Codex 的特点是每轮执行一个 CLI 子进程。cc-connect 保存 Codex 返回的 thread id，用于下一轮 `resume`，从而维持多轮会话。

### 6.3 下游访问

Codex 子进程负责访问项目工作目录和调用模型服务。cc-connect 不直接调用 Codex 背后的模型 API，而是把 `model_provider`、`openai_base_url`、模型名、推理强度等配置传给 Codex CLI。

## 7. ACP Agent

ACP Agent 指遵循 Agent Client Protocol 的后端 Agent。cc-connect 在这里扮演 ACP Client：它启动配置中的 ACP Agent 命令，并通过标准 JSON-RPC 协议通信。

### 7.1 连接方式

ACP Agent 通过项目配置指定命令：

```toml
[projects.agent]
type = "acp"

[projects.agent.options]
command = "path-or-name-of-acp-agent"
args = []
```

运行时，cc-connect 执行：

```text
<command> <args...>
```

然后使用该进程的 stdin/stdout 建立：

```text
newline-delimited JSON-RPC 2.0 over stdio
```

### 7.2 会话握手

ACP 会话启动时，cc-connect 大致按下面顺序调用 RPC：

```text
initialize
authenticate        # 可选，取决于 auth_method
session/load        # 如果要恢复已有 ACP session 且后端支持
session/new         # 创建新 ACP session
session/set_mode    # 可选，用于应用权限模式
```

### 7.3 数据如何传输

| 方向 | 传输方式 | 内容 |
|------|----------|------|
| cc-connect → ACP Agent | JSON-RPC request，例如 `session/prompt` | session id、prompt blocks、工作目录、权限模式 |
| ACP Agent → cc-connect | JSON-RPC response / notification / server request | 文本更新、工具调用、权限请求、模式信息、错误 |

用户消息会通过 `session/prompt` 发送。文件和图片通常先保存到本地，再把路径写入 prompt，让 ACP Agent 自己读取。

### 7.4 下游访问

ACP Agent 自己决定如何访问模型服务、文件系统、终端或其他工具。cc-connect 只要求它通过 ACP 协议把会话事件、工具状态和权限请求返回。

## 8. 端到端消息流

一次典型对话可以概括为：

```
1. 用户从聊天平台、Bridge 或 Web UI 发起请求。
2. cc-connect 接收请求并归一化成内部消息。
3. Core Engine 找到对应项目和会话。
4. Engine 把消息发送给后端 Agent。
5. Agent 读取文件、执行工具或调用模型 Provider。
6. Agent 返回思考、工具状态、权限请求或最终结果。
7. Engine 把结果渲染成外部入口支持的格式。
8. 用户在原入口看到回复。
```

## 9. 连接方式汇总

| 方向 | 连接方式 | 主要用途 |
|------|----------|----------|
| 外部聊天平台 ↔ cc-connect | 平台协议，如 WebSocket、Webhook、Long Polling、Gateway | 用户对话入口 |
| 自定义外部系统 ↔ cc-connect | Bridge WebSocket + REST | 自定义平台、脚本、内部系统接入 |
| 管理工具 → cc-connect | Management API HTTP REST | 管理和监控 |
| cc-connect → CLI Agent | 启动本地子进程，按各 CLI 的命令参数传入 prompt/附件路径，并从 stdout 读取 JSON 或 `stream-json` 事件 | 驱动具体产品的 CLI Agent |
| cc-connect → ACP Agent | newline-delimited JSON-RPC 2.0 over stdio | 驱动兼容 ACP 的 Agent |
| Agent → 模型服务 | HTTPS API | Agent 调用模型或中转服务 |
| cc-connect / Agent → 文件系统 | 本地文件 I/O | cc-connect 保存状态和附件；Agent 读写项目代码 |

## 10. 部署和安全边界

部署时需要重点关注三类边界：

| 边界 | 建议 |
|------|------|
| 网络入口 | Bridge 和 Management API 应配置强 token；公网暴露时建议放在 HTTPS 反向代理后 |
| 凭证 | 平台 token 和模型 API Key 建议通过环境变量或配置管理注入，不要写入代码 |
| Agent 权限 | Agent 会读写项目目录并可能执行命令；多项目或多租户场景建议做 OS 用户隔离和权限限制 |

## 11. 继续阅读

- [使用指南](usage.zh-CN.md)
- [Bridge 平台协议规范](bridge-protocol.zh-CN.md)
- [管理 API 规范](management-api.zh-CN.md)
- [配置模板](../config.example.toml)
