# AHand 架构设计说明

## 1. 文档目的

本文档用于说明 AHand 的运行时架构、核心业务分块、典型调用链路以及后续扩展方式。重点关注以下问题：

1. AHand 的客户端、实际服务端、控制平面分别承担什么职责。
2. 一次任务如何从控制平面发起，经服务端投递到客户端，并在客户端执行。
3. 客户端如何执行命令、文件操作、浏览器操作以及交互式 PTY 任务。
4. 如果需要增加新的客户端类型，应如何扩展而不破坏现有架构。
5. 在本地联调和远端连接两种模式下，应如何启动 AHand 程序。

本文档不展开 protobuf 字段级定义；协议在本文中仅作为跨组件传输载体。

---

## 2. 总览

### 2.1 三个核心维度

AHand 的运行时可以拆分为三个逻辑维度：

| 维度 | 主要组件 | 职责 |
|---|---|---|
| 客户端 | `ahandctl`、`ahandd` | 本地启动、主动连接、策略判定、本地能力执行、结果回传 |
| 实际服务端 | `ahand-hub` 的 `/ws`、`device_gateway`、`ConnectionRegistry` | 设备接入、WebSocket 会话维护、消息投递、设备回报接收 |
| 控制平面 | `ahand-hub` HTTP API、`JobRuntime`、`JobDispatcher`、dashboard、存储层 | 请求接入、任务编排、状态持久化、输出流、审计与展示 |

这三个维度的标准闭环如下：

```text
控制平面
-> 实际服务端
-> 客户端
-> 实际服务端
-> 控制平面
```

### 2.2 核心设计原则

1. **客户端主动连接服务端**
   本地设备不要求暴露公网入口。`ahandd` 主动连接 hub 的 WebSocket 入口。

2. **控制状态集中在控制平面**
   任务、输出、审计、设备状态等对外可见事实由 hub 侧收敛。

3. **执行边界保留在客户端**
   命令执行、文件访问、浏览器自动化、PTY 交互均在本地 `ahandd` 内完成。

4. **通信层与业务层分离**
   `ConnectionRegistry` 负责消息投递，不负责解释业务语义；`JobRuntime` 等控制平面组件负责业务状态。

5. **新客户端类型应通过连接适配接入**
   新客户端类型应复用现有本地执行运行时，而不是复制一套任务、审批、文件、浏览器执行栈。

### 2.3 local / remote 启动方式

这里的 local / remote 是部署和调试语境下的启动方式，不是 protobuf 协议里的字段。代码内部的客户端连接模式主要是：

| 配置值 | 含义 |
|---|---|
| `ahand-cloud` | `ahandd` 直接连接 AHand hub 的 WebSocket 入口 |
| `openclaw-gateway` | `ahandd` 作为节点连接 OpenClaw Gateway |

#### 2.3.1 local 模式：本机联调

local 模式用于开发、调试或单机验证。控制端、服务端和客户端都运行在本机：

```text
本机浏览器 dashboard
-> 本机 dev-cloud / hub
-> 本机 ahandd
-> 本机 shell / 文件系统 / 浏览器 / PTY
```

最小联调方式：

```bash
pnpm install
pnpm dev:cloud
```

`pnpm dev:cloud` 会启动开发用 cloud server 和测试 dashboard。开发 cloud server 默认监听：

```text
HTTP:      http://localhost:3000
WebSocket: ws://localhost:3000/ws
```

然后在另一个终端启动客户端 daemon：

```bash
cargo run -p ahandd -- --url ws://localhost:3000/ws
```

也可以先写入本地配置，再通过 `ahandctl` 后台启动：

```bash
ahandctl configure
ahandctl start
ahandctl status
```

local 模式下的典型配置是：

```toml
mode = "ahand-cloud"
server_url = "ws://localhost:3000/ws"
default_session_mode = "trust"
```

如果需要同时启动开发 cloud、dashboard 和 daemon，可以使用：

```bash
pnpm dev
```

但 `ahandd` 没有显式 `--url` 时会优先读取 `~/.ahand/config.toml`。因此首次使用前需要通过 `ahandctl configure` 写入配置，或直接用 `cargo run -p ahandd -- --url ...` 指定本地 WebSocket 地址。

local 模式的重点是验证链路是否闭合：

```text
dashboard 创建任务
-> localhost:3000 接收请求
-> localhost:3000/ws 投递给本机 ahandd
-> ahandd 执行本机命令
-> 输出回到 dashboard
```

#### 2.3.2 remote 模式：客户端连接远端 hub

remote 模式用于真实部署或跨机器使用。hub / dashboard 运行在远端，客户端机器只运行 `ahandd`：

```text
远端 dashboard / 控制平面
-> 远端 ahand-hub /ws
-> 本机 ahandd
-> 本机 shell / 文件系统 / 浏览器 / PTY
```

客户端安装后通过配置指定远端 WebSocket：

```bash
ahandctl configure
ahandctl start
```

对应配置示例：

```toml
mode = "ahand-cloud"
server_url = "wss://hub.example.com/ws"
default_session_mode = "strict"

[hub]
bootstrap_token = "<device-bootstrap-token>"
```

也可以不写配置文件，直接以前台方式连接远端 hub：

```bash
ahandd --url wss://hub.example.com/ws
```

生产环境通常使用 `ahandctl start`，因为它会：

1. 查找已安装的 `ahandd`。
2. 将 daemon 放到后台运行。
3. 将日志写入 `~/.ahand/data/daemon.log`。
4. 写入 PID 文件，供 `ahandctl status` / `stop` / `restart` 使用。

remote 模式下，客户端仍然是主动出站连接，不需要远端服务端反向访问本机。只要本机能访问 `wss://hub.example.com/ws`，就可以在 NAT 或防火墙后运行。

#### 2.3.3 remote hub 的启动

如果需要自己启动远端 hub，`ahand-hub` 从环境变量读取配置，并依赖 PostgreSQL 与 Redis：

```bash
export AHAND_HUB_BIND_ADDR=0.0.0.0:8080
export AHAND_HUB_SERVICE_TOKEN=<service-token>
export AHAND_HUB_DASHBOARD_PASSWORD=<dashboard-password>
export AHAND_HUB_DEVICE_BOOTSTRAP_TOKEN=<device-bootstrap-token>
export AHAND_HUB_DEVICE_BOOTSTRAP_DEVICE_ID=<bootstrap-device-id>
export AHAND_HUB_JWT_SECRET=<jwt-secret>
export AHAND_HUB_DATABASE_URL=postgres://user:password@postgres:5432/ahand_hub
export AHAND_HUB_REDIS_URL=redis://redis:6379

cargo run -p ahand-hub
```

生产部署更常见的方式是使用 `deploy/hub` 下的 Docker 镜像和 compose 配置。该部署启动的是：

```text
ahand-hub
ahand-hub-dashboard
```

PostgreSQL 和 Redis 是外部依赖，需要在启动前可访问。

#### 2.3.4 remote gateway 模式：OpenClaw Gateway

如果客户端不是直接连接 AHand hub，而是通过 OpenClaw Gateway 接入，则配置为：

```toml
mode = "openclaw-gateway"
default_session_mode = "auto_accept"

[openclaw]
gateway_host = "gateway.example.com"
gateway_port = 18789
gateway_tls = true
node_id = "node-01"
display_name = "Developer Machine"
auth_token = "<gateway-token>"
```

启动方式仍然是：

```bash
ahandctl start
```

差异只在连接适配层：`ahandd` 会进入 `OpenClawGateway` 分支，创建 `OpenClawClient` 并连接 gateway；本地命令执行、文件操作、浏览器操作、审批和 PTY 运行时仍复用同一套客户端内部组件。

---

## 3. 分块业务逻辑

### 3.1 客户端

#### 3.1.1 业务职责

客户端负责本地执行边界，主要职责包括：

- 读取本地配置并启动 daemon
- 主动连接实际服务端
- 维护与服务端的长连接
- 接收任务、文件、浏览器、终端输入等请求
- 执行 session mode 与审批判定
- 调用本地能力模块执行请求
- 将输出、终态、审批请求等结果回传服务端

客户端由两个程序组成：

| 程序 | 作用 |
|---|---|
| `ahandctl` | 本地运维入口，用于配置、启动、停止、状态查询、升级、浏览器初始化 |
| `ahandd` | 长期运行 daemon，负责连接 hub 并执行远程请求 |

#### 3.1.2 启动逻辑

典型启动方式：

```bash
ahandctl configure
ahandctl start
```

`ahandd` 启动后的运行过程：

```text
读取配置
-> 初始化本地运行时对象
-> 建立 TCP 连接
-> 升级为 WebSocket
-> 完成设备握手
-> 启动 send task / heartbeat task / read loop
-> 断线后退避重连
```

客户端采用出站连接模型，因此实际服务端无需主动连接本地设备。

#### 3.1.3 本地运行时对象

`ahandd` 启动后会装配一组本地运行时对象：

| 对象 | 业务意义 |
|---|---|
| `SessionManager` | 管理 caller 的 session mode，决定请求允许、拒绝或需要审批 |
| `ApprovalManager` | 跟踪待审批请求，接收审批结果 |
| `JobRegistry` | 记录运行中任务、取消通道、交互式 stdin 通道与完成缓存 |
| `BrowserManager` | 执行本地浏览器自动化 |
| `FileManager` | 执行结构化文件操作，并做路径策略检查 |
| `RunStore` | 保存本地运行记录、输出和 trace |
| `Outbox` | 保存未确认出站消息，用于断线重放 |
| `executor` | 启动普通子进程或 PTY 子进程 |

#### 3.1.4 请求分发逻辑

客户端完成握手后进入 read loop。每条来自服务端的消息会按类型分发：

```text
WebSocket read loop
-> decode message
-> dispatch by payload type
    -> JobRequest
    -> CancelJob
    -> BrowserRequest
    -> FileRequest
    -> ApprovalResponse
    -> StdinChunk
    -> TerminalResize
```

以任务请求为例：

```text
JobRequest
-> 幂等检查
-> SessionManager.check(...)
    -> Deny
    -> NeedsApproval
    -> Allow
-> spawn_job(...)
-> executor::run_job(...) 或 executor::run_job_pty(...)
```

客户端不会在收到任务后直接执行系统命令；所有任务必须先经过本地 session / approval 判定。

---

### 3.2 实际服务端

#### 3.2.1 业务职责

实际服务端是 `ahand-hub` 中面向设备的在线通信层。它的核心职责是：

- 接收客户端 WebSocket 连接
- 完成设备握手与身份校验
- 维护 `device_id -> active connection` 映射
- 将控制平面的请求投递到目标客户端
- 接收客户端回报并上交控制平面
- 处理连接级确认、重放、失活检测

主要实现位置：

| 组件 | 作用 |
|---|---|
| `/ws` | 客户端 daemon 的 WebSocket 入口 |
| `device_gateway` | 设备接入、握手、读写循环 |
| `ConnectionRegistry` | 维护设备连接并提供消息发送接口 |

#### 3.2.2 设备接入逻辑

客户端连接 `/ws` 后，实际服务端执行以下流程：

```text
客户端连接 /ws
-> 服务端发送握手挑战
-> 客户端返回身份与能力信息
-> 服务端校验设备身份
-> 服务端更新设备信息
-> ConnectionRegistry 注册活动连接
-> 服务端返回握手接受消息
-> 标记设备 online
-> 启动连接附属任务
```

连接附属任务包括：

- 出站发送任务
- 在线状态刷新任务
- 失活检测任务

#### 3.2.3 消息投递逻辑

控制平面不直接写 WebSocket，而是通过 `ConnectionRegistry` 投递消息：

```text
控制平面
-> ConnectionRegistry.send(...)
-> 查找目标设备连接
-> 分配发送序号
-> 写入 outbox
-> 推入连接发送通道
-> WebSocket send task 写出
```

该设计将业务编排与网络 I/O 解耦。

#### 3.2.4 设备回报接收逻辑

客户端返回结果后，实际服务端只做连接层处理，并将业务消息交给控制平面：

```text
客户端回报
-> device gateway 接收
-> 连接级确认与去重
-> JobRuntime.handle_device_frame(...)
```

实际服务端不负责最终业务状态解释。

---

### 3.3 控制平面

#### 3.3.1 业务职责

控制平面面向 operator、dashboard 和外部 API 调用方，主要职责包括：

- 暴露 HTTP API
- 校验请求与权限
- 管理设备与任务
- 创建任务记录并推进任务状态
- 管理输出流与终态
- 提供 dashboard 查询和订阅能力
- 记录审计与事件

主要实现位置：

| 组件 | 作用 |
|---|---|
| `http/jobs.rs` | 任务创建、取消、输出流、stdin、resize |
| `http/files.rs` | 文件操作入口 |
| `http/browser.rs` | 浏览器操作入口 |
| `http/terminal.rs` | 浏览器终端 WebSocket bridge |
| `JobRuntime` | 任务运行时编排 |
| `JobDispatcher` | 任务创建与基础校验 |
| `OutputStream` | 任务输出流 |
| `JobStore / DeviceStore / AuditStore` | 业务状态持久化 |

#### 3.3.2 启动逻辑

`ahand-hub` 启动后，控制平面初始化如下：

```text
读取环境变量
-> 初始化 AppState
-> 连接 PostgreSQL / Redis
-> 装配 auth / store / events / output stream / webhook
-> 构建 Axum Router
-> 对外提供 HTTP API 和 WebSocket 入口
```

#### 3.3.3 请求编排逻辑

以创建任务为例：

```text
Dashboard / API
-> POST /api/jobs
-> http/jobs.rs handler
-> JobRuntime.create_job(...)
-> JobDispatcher.create_job(...)
-> 创建任务记录
-> 推进任务状态到 Sent
-> ConnectionRegistry.send(...)
```

控制平面决定“发什么”，实际服务端决定“如何送达”。

#### 3.3.4 结果收敛逻辑

客户端返回的任务事件在控制平面被解释为业务状态：

```text
JobEvent
-> OutputStream.push_stdout / push_stderr / push_progress
-> 任务状态推进到 Running

JobFinished
-> 任务状态推进到 Finished / Failed / Cancelled
-> 写入终态
-> 结束输出流
-> 触发事件与前端可见更新
```

因此，控制平面是系统对外可见的业务状态权威层。

---

### 3.4 客户端命令执行能力

#### 3.4.1 普通命令执行

通用命令执行由客户端的 `executor` 模块完成。控制平面提交任务后，客户端接收到 `JobRequest`，其中关键输入包括：

| 字段 | 含义 |
|---|---|
| `tool` | 要执行的二进制名称或路径 |
| `args` | 参数列表 |
| `cwd` | 工作目录 |
| `env` | 附加环境变量 |
| `timeout_ms` | 超时时间 |
| `interactive` | 是否使用 PTY |

普通任务执行路径如下：

```text
JobRequest(interactive=false)
-> resolve_tool(...)
-> tokio::process::Command
-> 设置 args / cwd / env
-> spawn child process
-> 分别读取 stdout / stderr
-> 回传 JobEvent
-> 等待退出 / 取消 / 超时
-> 回传 JobFinished
```

这就是 AHand 当前已经实现的**非 PTY 执行选项**。它适合执行一次性命令、脚本、构建任务、测试任务、文件生成任务或持续输出日志的进程，例如 `ls`、`git status`、`cargo test`、`npm run build`、`tail -f`。

非 PTY 执行与 PTY 执行的关键差异如下：

| 模式 | 触发条件 | 子进程看到的环境 | 输出模型 | 输入模型 | 适合场景 |
|---|---|---|---|---|---|
| 非 PTY 命令 | `interactive=false` | 普通 child process，不是 TTY | stdout / stderr 分离，按字节块流式回传 | 当前主链路不维护持续 stdin 通道 | 批处理命令、脚本、CI 类任务、普通日志输出 |
| PTY 命令 | `interactive=true` | 子进程绑定 pseudo-terminal | stdout / stderr 合并为终端字节流 | 支持按键输入和 resize | shell、REPL、`vim`、`k9s`、`top` 等交互式或 TUI 程序 |

因此，AHand 可以“不以 PTY 模式执行命令”，但这不等价于“完整终端交互”。非 PTY 模式更接近远程启动一个进程并订阅它的 stdout / stderr；如果需要在进程启动后持续发送按键、控制光标、调整窗口大小或运行全屏 TUI，则应使用 PTY 模式。

需要特别区分一种尚未独立实现的形态：**全双工流式 pipe 模式**。它的语义是 stdin、stdout、stderr 都是流，但子进程不需要看到 TTY，也不涉及光标移动、清屏、alternate screen 或窗口 resize：

```text
控制平面输入流
-> 子进程 stdin pipe

子进程 stdout pipe
子进程 stderr pipe
-> 控制平面输出流
```

该模式适合 Claude Code、Codex、`python` 脚本交互、语言服务器、简单 REPL、持续消费 stdin 的 CLI 程序等“有持续输入输出，但不需要终端画布”的场景。它与 PTY 的区别是：不提供终端设备，不合并 stdout/stderr，不处理 terminal resize，也不要求浏览器终端模拟器解释 ANSI 重绘序列。

`pipe_stream` 模式会为任务注册 stdin channel，子进程使用普通 `stdin/stdout/stderr` pipe。`StdinChunk` 会写入 child stdin，stdout 和 stderr 仍按独立 chunk 回传，因此 Claude/Codex 这类可流式输出 agent 可以显式选择该模式运行。`pty` 继续保留给必须依赖真实 TTY 或全屏终端画布的程序。

该模式不复用 `interactive=true` 表示，因为它在现有语义中已经等价于 PTY。新协议使用独立执行模式：

```text
execution_mode = "batch"         # 当前 interactive=false
execution_mode = "pty"           # 当前 interactive=true
execution_mode = "pipe_stream"   # stdin/stdout/stderr 全双工 pipe
```

对应客户端实现要求：

1. 在非 PTY spawn 时设置 `stdin(Stdio::piped())`。
2. 为该 job 注册 stdin sender。
3. 将 `StdinChunk` 写入 child stdin，而不是 PTY master。
4. 继续保持 stdout / stderr 分离回传。
5. 忽略或拒绝 `TerminalResize`，因为 pipe-stream 子进程没有终端画布。

#### 3.4.2 进程中间结果可观测性

三种执行模式都必须保证运行中间结果可观测，而不是只在进程退出后返回最终结果。这里的“中间结果”包括：

- 子进程 stdout / stderr 的增量输出
- PTY 模式下的原始终端字节流
- `codex`、`claude` 等 agent CLI 在运行过程中写出的日志、状态、工具调用进度或 transcript
- daemon 侧 envelope trace 与 per-run artifact
- hub 侧任务输出流、任务状态、审计事件和 dashboard 实时展示

统一原则如下：

| 执行模式 | 必须可观测的内容 | 输出语义 | 控制平面展示方式 |
|---|---|---|---|
| `batch` | stdout、stderr、progress、finished | stdout / stderr 分离的追加流 | job output SSE / dashboard job output |
| `pty` | PTY master 原始字节流、finished | 终端字节流，stdout / stderr 不再可靠分离 | terminal WebSocket / xterm.js |
| `pipe_stream` | stdin 输入事件、stdout、stderr、progress、finished | 全双工 pipe，stdout / stderr 分离 | job output SSE；需要输入通道时绑定同一 job |

对 `codex`、`claude` 这类长时间运行的 agent 进程，不能只依赖最终 exit code。它们通常会同时产生两类可观测数据：

1. 面向用户的实时输出：进程写到 stdout / stderr 或 PTY 的内容，必须按 chunk 尽快进入 `JobEvent`，再进入 hub `OutputStream`。
2. 面向诊断的本地日志：进程写到文件系统中的 session log、JSONL transcript、debug log 等，必须有明确策略让用户或控制平面能定位和读取。

因此，执行层和控制平面应满足以下约束：

1. daemon 启动任务时继续写入 `RunStore` 的 `request.json`、`stdout`、`stderr`、`result.json`，并保留 `trace.jsonl` 作为 envelope 级诊断线索。
2. hub 接收 `JobEvent` 后继续写入 `OutputStream`；生产环境应使用 Redis-backed persistent output，使 dashboard 断线重连后仍可从 `Last-Event-ID` 或历史输出恢复。
3. dashboard 不应等任务结束才刷新输出；所有模式都必须显示 running 状态和实时输出。
4. 对 PTY 输出不得按行解析、裁剪、转义或改写 ANSI 控制序列；否则 `claude`、`codex`、`vim`、`top` 这类交互程序的中间状态会失真。
5. 对 `batch` 和 `pipe_stream` 输出应保留 stdout / stderr 通道差异，便于区分 agent 正常进度与错误诊断。
6. 如果 agent CLI 的关键中间状态只写入日志文件，而不写 stdout / stderr，则调用方应通过显式参数或环境变量把日志路径放到 job metadata / env 中，并在任务运行时用文件 API 或后续 artifact API 读取。
7. 后续若增加专门的 artifact/log tail 能力，应把它建模为 job 关联资源，而不是散落在 dashboard 私有逻辑中。

推荐的 agent 运行约定：

```text
job_id
-> AHAND_JOB_ID
-> AHAND_RUN_DIR=~/.ahand/data/runs/{job_id}
-> agent stdout/stderr -> JobEvent -> hub OutputStream
-> agent log/transcript files -> AHAND_RUN_DIR 或显式配置路径
```

这样做的目标是：无论 agent 以 `batch`、`pty` 还是未来的 `pipe_stream` 运行，操作者都能在 dashboard 看到实时进展；任务失败时，也能从 hub 输出、daemon run artifact 和 agent 自身日志三层还原过程。

#### 3.4.3 过程数据采集契约

为了保证整个流程可以复盘，AHand 需要把一次任务拆成可观测的数据面。每个阶段都应产生结构化过程数据，并且数据必须有明确读取路径。

| 阶段 | 必须采集的数据 | 当前/建议落点 | 读取路径 |
|---|---|---|---|
| 任务创建 | job id、device id、tool、args、cwd、env 摘要、timeout、execution mode、requested_by、created_at | hub `jobs` 表；daemon `request.json` | `GET /api/jobs/{id}`；daemon run artifact |
| 任务投递 | envelope msg_id、seq、ack、trace_id、device_id、payload type、投递时间 | daemon `trace.jsonl`；hub outbox / logs | daemon trace；hub service logs |
| 本地审批 | caller_uid、session mode、审批请求、审批结果、拒绝原因、超时 | audit log；daemon approval state | `GET /api/audit-logs`；daemon logs |
| 子进程启动 | resolved tool path、pid、cwd、环境变量白名单摘要、start_ms、execution mode | daemon run artifact；hub job started_at | daemon run artifact；`GET /api/jobs/{id}` |
| 实时输出 | stdout chunk、stderr chunk、progress、PTY raw bytes、chunk seq、timestamp | hub `OutputStream`；Redis job output；daemon stdout/stderr files | `GET /api/jobs/{id}/output` SSE；terminal WS；daemon run artifact |
| 交互输入 | stdin chunk、terminal resize、输入方向、关联 job id、timestamp | hub terminal bridge logs；daemon trace | terminal session diagnostics；daemon trace |
| agent 过程文件 | transcript、JSONL log、debug log、tool call log、checkpoint、workspace diff 摘要 | `AHAND_RUN_DIR` 或显式 agent log path | file API；后续 artifact/log API |
| 终态 | exit_code、error、finished_at、duration、output_summary、cancel/timeout/disconnect 原因 | hub `jobs` 表；`OutputStream.finished`；daemon `result.json` | `GET /api/jobs/{id}`；SSE finished event；daemon artifact |
| 审计 | job.sent、job.running、job.finished/failed/cancelled、文件/浏览器/审批操作 | hub audit store | `GET /api/audit-logs` |

采集粒度要求：

1. 输出 chunk 必须有顺序。hub `OutputStream` 已经给 SSE 事件分配递增 id，持久化输出应保留可恢复顺序。
2. 过程数据必须能关联到同一个 `job_id`；跨 WebSocket envelope 时使用 `trace_id` / `msg_id` 辅助定位。
3. 大输出不能只存在内存里。生产路径应启用 Redis-backed output，daemon 本地仍保留 run artifact 作为设备侧兜底。
4. 对敏感数据不能无差别采集。`env`、命令参数、agent transcript 和文件路径可能包含 token 或用户内容，dashboard 展示和持久化前需要脱敏策略或访问控制。
5. 对二进制或超大日志不要塞进 job summary。summary 只保存短摘要，完整内容走 output stream、artifact 或文件 API。
6. 如果 dashboard 展示“运行中”，它必须能同时展示最近输出、当前状态、开始时间、持续时间、最近更新时间和终止原因占位。
7. 如果 daemon 与 hub 断连，hub 应记录 disconnect 过程事件；daemon 本地 run artifact 仍应能说明子进程是否继续、被取消或已退出。

对 `codex` / `claude` 这类 agent，建议额外采集以下过程数据：

```text
agent_kind           # codex / claude / other
agent_session_id     # agent 自身 session id，如果能获取
agent_log_path       # transcript 或 JSONL 日志位置
agent_workspace      # agent 操作的工作目录
agent_model          # 模型名，如果由调用方显式传入
agent_phase          # planning / running / waiting_for_approval / applying_patch / testing / finished
agent_last_event_ms  # 最近一次输出或日志更新时间
```

这些字段不一定都进入 protobuf V1 主消息；短期可以通过 job env、metadata 或 run artifact 记录。长期更好的方式是新增 job metadata / artifact API，让 dashboard 能在同一个 job 页面展示：

- 实时输出流
- 结构化过程事件
- agent transcript / log 文件
- 终态摘要
- 审计记录

#### 3.4.4 可执行命令范围

当前实现中，客户端可执行命令不是固定内建列表，而由 `JobRequest.tool` 决定：

1. 普通 `tool` 字符串被视为 PATH 可解析的可执行名或明确路径。
2. 特殊值 `"shell"` 或 `"$SHELL"` 会解析为本地默认 shell，并以 login shell 方式启动。

因此，通用任务路径理论上可以执行宿主机上当前 daemon 用户可访问的任意可执行程序，例如：

- `sh`、`bash`、`ls`、`cat`
- `git`、`cargo`、`node`、`python`
- `curl`、`wget`、`ssh`
- `/usr/bin/env` 等明确路径程序

实际是否执行取决于：

- session mode
- 审批结果
- 宿主机程序是否存在
- 当前运行用户是否有权限

#### 3.4.5 PTY 执行

当 `interactive=true` 时，客户端使用 PTY 执行任务：

```text
JobRequest(interactive=true)
-> JobRegistry.register_interactive(...)
-> executor::run_job_pty(...)
-> portable_pty.openpty(...)
-> 子进程绑定 slave 端启动
-> daemon 持有 master 端
```

PTY 模型中：

| 对象 | 作用 |
|---|---|
| PTY slave | 作为子进程的终端设备 |
| PTY master | 由 daemon 持有，用于读输出、写输入、调整窗口大小 |

PTY 输出由客户端从 master 端读取，并作为任务输出回传；输入和窗口变化通过 `JobRegistry` 路由到该任务。

PTY 需要区分两类终端负载：

| 类型 | 示例 | 输出特征 | 处理要求 |
|---|---|---|---|
| 流式输出型 | `tail -f`、普通 shell 命令、REPL 文本输出 | 按时间追加文本，主要表现为新行或局部文本追加 | 可以按字节块连续传输，终端按收到顺序显示 |
| 全屏重绘型 | `vim`、`k9s`、`top`、`htop`、`less`、TUI 程序 | 大量 ANSI 控制序列、光标移动、清屏、局部重绘、alternate screen | 必须保持原始字节流，不应按行解析、裁剪、合并或重新格式化 |

PTY 的重点在于支持全屏重绘型程序。此类程序不是持续输出普通文本，而是在同一个终端画布上反复更新状态。例如：

- `vim` 会切换到 alternate screen，移动光标，并按缓冲区状态重绘屏幕。
- `k9s` 会持续刷新 Kubernetes 资源列表、状态栏和快捷键区域。
- `top` / `htop` 会周期性更新已有行，而不是追加新日志。

因此，PTY 输出在系统内必须被视为**终端字节流**，而不是日志行。具体要求如下：

1. 客户端从 PTY master 读取到的字节应原样回传。
2. 实际服务端只负责转发和写入输出流，不应理解或改写 ANSI 控制序列。
3. 控制平面的 terminal bridge 应将输出作为二进制数据推给浏览器终端。
4. 浏览器侧必须由终端模拟器解释这些字节，而不是用普通文本框渲染。
5. resize 事件必须及时传回客户端，否则 `vim`、`k9s` 等程序无法正确重绘布局。

#### 终端画布大小

PTY 画布大小由“初始默认值 + 浏览器终端实际尺寸回传”共同决定。

客户端创建 PTY 时会先使用默认尺寸：

```text
cols = 80
rows = 24
```

该默认值只用于 PTY 创建瞬间，避免子进程在没有尺寸信息时无法启动。交互式终端真正可见的画布大小由浏览器侧终端容器决定：

```text
Browser terminal container
-> xterm.js / FitAddon 根据 DOM 尺寸和字体度量计算 cols / rows
-> 控制平面接收 resize
-> 实际服务端转发 TerminalResize
-> 客户端调用 PTY master resize
-> 子进程感知新的终端尺寸
```

在当前 dashboard 实现中，浏览器终端打开后会执行一次 fit，并发送初始 `cols` / `rows`；后续浏览器窗口或容器变化时再次计算并发送 resize。hub 侧将该尺寸转换为设备侧 `TerminalResize`，最终由客户端更新 PTY master 的 `rows` 和 `cols`。

对全屏重绘型程序而言，画布大小不是展示层细节，而是程序布局输入。`vim`、`k9s`、`top` 等程序会根据当前终端尺寸决定：

- 可显示的行数
- 每行可显示的列数
- 状态栏位置
- 分屏布局
- 列宽和截断策略
- 光标合法位置

因此，若浏览器侧画布尺寸没有及时同步到 PTY，常见问题包括：

- `vim` 底部状态栏位置错误
- `k9s` 表格列错位或显示不完整
- TUI 程序认为终端仍是 80x24
- resize 后屏幕残留旧内容
- 光标位置与浏览器显示不一致

该链路的正确抽象是：

```text
子进程终端画布
-> PTY master 原始字节流
-> device WebSocket
-> hub OutputStream
-> terminal WebSocket
-> Browser terminal emulator
```

---

### 3.5 文件操作能力

文件操作不通过 shell 命令模拟，而是走结构化文件请求路径：

```text
控制平面文件 API
-> 实际服务端投递 FileRequest
-> 客户端 handle_file_request(...)
-> FileManager.check_request_approval(...)
-> FileManager.handle(...)
-> 回传 FileResponse
```

`FileManager` 负责：

- 路径策略校验
- 危险路径审批升级
- 文件读写
- 列目录
- glob
- 删除、复制、移动等文件系统操作

文件能力的真实执行边界在客户端。

---

### 3.6 浏览器自动化能力

浏览器自动化由 `BrowserManager` 执行，不通过通用命令任务模拟：

```text
控制平面 browser API
-> 实际服务端投递 BrowserRequest
-> 客户端 handle_browser_request(...)
-> SessionManager.check(...)
-> BrowserManager.check_domain(...)
-> BrowserManager.execute(...)
-> 回传 BrowserResponse
```

该能力用于将浏览器会话、页面操作、截图/二进制数据等结果作为结构化响应返回。

---

### 3.7 交互式终端能力

交互式终端是 PTY 执行在控制平面与 dashboard 侧的完整封装。

需要注意，控制平面也可以创建 `interactive=false` 的普通任务。普通任务不走 terminal bridge，也不会创建 PTY；dashboard 或 API 只订阅任务输出流。只有当调用方需要“像终端一样”向正在运行的进程持续输入数据时，才进入本节描述的 PTY 终端链路。

数据路径：

```text
Dashboard 创建 interactive job
-> 控制平面创建任务记录
-> 实际服务端投递 interactive JobRequest
-> 客户端创建 PTY 并启动子进程
-> PTY 输出回传到 OutputStream
-> Dashboard 通过 /ws/terminal 订阅输出
```

用户输入路径：

```text
Browser terminal
-> /ws/terminal
-> terminal bridge
-> ConnectionRegistry.send(StdinChunk)
-> 客户端 JobRegistry.send_stdin(...)
-> PTY master write
-> 子进程读取输入
```

窗口调整路径：

```text
Browser terminal resize
-> 控制平面 resize 入口
-> ConnectionRegistry.send(TerminalResize)
-> 客户端 JobRegistry.send_stdin(... Resize)
-> PTY master resize
```

这里存在两段 WebSocket：

| WebSocket | 面向对象 | 职责 |
|---|---|---|
| `/ws/terminal` | Browser / Dashboard | 接收用户输入、窗口变化，并返回终端输出 |
| `/ws` | ahandd | 投递 StdinChunk / TerminalResize，并接收 PTY 输出 |

交互式终端必须按“终端协议流”处理，而不能按“日志文本流”处理。全屏 TUI 程序依赖以下终端语义：

- ANSI escape sequence
- 光标定位
- 局部行重绘
- 清屏
- alternate screen
- 终端尺寸
- 原始按键序列

因此，terminal bridge 的职责是桥接两端字节流：

```text
Browser -> hub:
  binary frame       -> StdinChunk
  resize JSON        -> TerminalResize

hub -> Browser:
  OutputStream bytes -> binary frame
  Finished           -> close frame
```

对于流式输出型程序，浏览器终端看到的是不断追加的输出；对于 `vim`、`k9s` 这类全屏重绘型程序，浏览器终端根据收到的控制序列更新同一个屏幕缓冲区。两类程序共享同一条 PTY 数据通道，差异只在于终端模拟器如何解释字节流。

画布尺寸同步是该链路的一部分。浏览器终端负责根据实际 DOM 容器计算字符网格大小，控制平面只转发该尺寸，客户端将尺寸应用到 PTY；任何一层不应自行猜测全屏 TUI 的最终布局。

---

### 3.8 审批与安全控制

审批由客户端发起，控制平面展示，最终仍由客户端决定是否继续执行：

```text
客户端发现请求需要审批
-> ApprovalManager.submit(...)
-> 客户端发送 ApprovalRequest
-> 实际服务端转交控制平面
-> dashboard 展示审批
-> 控制平面返回 ApprovalResponse
-> 客户端继续执行或拒绝
```

当前主执行路径中的准入控制包括：

- session mode：`Inactive`、`Strict`、`Trust`、`AutoAccept`
- approval flow
- 文件路径策略
- 浏览器域名策略
- 宿主环境权限

代码中还存在 `PolicyConfig.allowed_tools`、`denied_tools`、`allowed_domains` 与 `PolicyChecker`，但当前主启动路径中命令准入主要依赖 `SessionManager` 与审批流程。

---

## 4. Case Study

### 4.1 Case Study：执行一次普通命令

#### 场景

Dashboard 请求某台设备执行：

```text
tool = "git"
args = ["status"]
cwd = "/workspace/project"
interactive = false
```

#### 流程

```text
1. Dashboard 调用 POST /api/jobs
2. 控制平面创建 Job 记录
3. JobRuntime 将 Job 状态推进到 Sent
4. ConnectionRegistry 将 JobRequest 投递给目标设备
5. ahandd 收到 JobRequest
6. SessionManager 判定是否允许执行
7. JobRegistry 注册任务
8. executor 使用 tokio::process::Command 启动 git
9. ahandd 读取 stdout / stderr 并回传 JobEvent
10. git 退出后 ahandd 回传 JobFinished
11. hub 将输出写入 OutputStream，并将任务推进到终态
12. Dashboard 展示输出和最终状态
```

#### 关键点

- 命令真正执行在客户端本地。
- 控制平面只负责创建任务和保存结果。
- 实际服务端只负责消息投递与接收。

---

### 4.2 Case Study：启动一个交互式 shell

#### 场景

Dashboard 请求设备启动交互式 shell：

```text
tool = "shell"
interactive = true
```

#### 流程

```text
1. 控制平面创建 interactive job
2. 实际服务端投递 JobRequest
3. 客户端识别 interactive=true
4. JobRegistry 注册 stdin sender
5. executor::run_job_pty(...) 分配 PTY
6. shell 绑定 PTY slave 启动
7. ahandd 持有 PTY master
8. terminal WebSocket 将用户输入转成 StdinChunk
9. ahandd 将输入写入 PTY master
10. shell 输出经 PTY master 被 ahandd 读取
11. 输出回传 OutputStream
12. Browser terminal 展示输出
```

#### 关键点

- `shell` / `"$SHELL"` 会解析为本地默认 shell。
- PTY 输出是终端字节流，stdout 与 stderr 不再严格分离。
- 用户输入和 resize 都进入已经运行的 PTY 任务，而不是新建任务。

---

### 4.3 Case Study：运行 `vim` 或 `k9s` 这类全屏 TUI

#### 场景

Dashboard 请求设备启动一个会重绘屏幕的 TUI 程序：

```text
tool = "vim"
args = ["src/main.rs"]
interactive = true
```

或：

```text
tool = "k9s"
interactive = true
```

#### 流程

```text
1. 控制平面创建 interactive job
2. 实际服务端投递 JobRequest
3. 客户端创建 PTY
4. vim / k9s 绑定 PTY slave 启动
5. 程序输出 ANSI 控制序列、清屏、光标移动、局部重绘等字节
6. ahandd 从 PTY master 原样读取字节
7. 输出经设备 WebSocket 回传到 OutputStream
8. terminal bridge 将输出作为 binary frame 发给浏览器终端
9. 浏览器终端模拟器解释控制序列并更新屏幕缓冲区
10. 用户按键经 /ws/terminal 转成 StdinChunk
11. ahandd 将按键字节写回 PTY master
12. resize 经 TerminalResize 回到客户端并触发 PTY resize
```

#### 关键点

- 不能把 `vim` / `k9s` 输出当作按行日志处理。
- 不能在 hub 上过滤 ANSI 控制序列，否则浏览器终端无法正确重绘。
- resize 是该类程序正确显示的必要输入，不是附加能力。
- terminal bridge 的职责是透明桥接字节流，而不是解释终端内容。
- 浏览器侧必须使用终端模拟器维护屏幕缓冲区。

---

### 4.4 Case Study：执行文件读取

#### 场景

控制平面请求读取目标设备上的一个文件。

#### 流程

```text
1. 控制平面接收文件 API 请求
2. 实际服务端投递 FileRequest
3. 客户端 handle_file_request(...)
4. FileManager 执行 allowlist / denylist / dangerous_paths 检查
5. 如需审批，进入 ApprovalManager
6. 通过后执行实际文件读取
7. 客户端返回 FileResponse
8. 控制平面将结果返回调用方
```

#### 关键点

- 文件操作不通过 `cat` 等 shell 命令实现。
- 路径策略由客户端执行。
- 危险路径可以触发审批，而不是由控制平面直接绕过。

---

### 4.5 Case Study：浏览器自动化

#### 场景

控制平面请求设备执行一次浏览器动作，例如页面访问、截图或 DOM 操作。

#### 流程

```text
1. 控制平面接收 browser API 请求
2. 实际服务端投递 BrowserRequest
3. 客户端 handle_browser_request(...)
4. SessionManager 做执行判定
5. BrowserManager 做域名策略检查
6. BrowserManager.execute(...) 调用本地浏览器自动化能力
7. 客户端回传 BrowserResponse
8. 控制平面返回结构化结果
```

#### 关键点

- 浏览器能力是独立能力路径，不是通用命令路径。
- 浏览器执行仍发生在客户端。
- 控制平面负责发起和展示结果。

---

### 4.6 Case Study：审批请求

#### 场景

客户端处于 `Strict` 模式，控制平面下发一个任务。

#### 流程

```text
1. 客户端收到 JobRequest
2. SessionManager 返回 NeedsApproval
3. ApprovalManager 创建待审批项
4. 客户端发送 ApprovalRequest
5. 实际服务端转交控制平面
6. Dashboard 展示审批信息
7. 用户批准或拒绝
8. 控制平面发送 ApprovalResponse
9. 客户端继续执行或返回 JobRejected
```

#### 关键点

- 审批的最终执行点在客户端。
- 控制平面提供交互入口，但不绕过客户端本地策略。

---

## 5. 状态归属与故障恢复

### 5.1 状态归属

| 状态类型 | 所属维度 | 说明 |
|---|---|---|
| 本地配置 | 客户端 | 控制 daemon 连接与本地能力开关 |
| 运行中任务注册表 | 客户端 | 用于取消、stdin、幂等缓存 |
| PTY stdin sender | 客户端 | 将输入路由到运行中的交互任务 |
| 当前活动连接 | 实际服务端 | `device_id -> active connection` |
| outbox / ack | 客户端与实际服务端 | 用于消息确认和断线重放 |
| 任务记录 | 控制平面 | 对外可见的任务事实 |
| 输出流 | 控制平面 | dashboard / API 订阅或查询的输出 |
| 审计记录 | 控制平面 | 操作审计与事件记录 |

### 5.2 故障恢复

| 故障 | 恢复方式 |
|---|---|
| 客户端断线 | `ahandd` 重连循环重新建立连接 |
| WebSocket 僵尸连接 | 实际服务端失活检测清理连接 |
| 任务消息未确认 | outbox 与 ack 机制降低消息丢失风险 |
| 客户端重连 | 客户端恢复在线会话，控制平面仍以持久化状态为准 |
| 任务已终结 | 控制平面终态为准，客户端不重建业务事实 |

---

## 6. 扩展设计

### 6.1 新增客户端类型

#### 扩展目标

新增客户端类型时，应保持以下架构约束：

1. 控制平面不直接依赖具体客户端实现。
2. 实际服务端继续承担通信层职责。
3. 本地执行边界仍保留在客户端。
4. 设备侧行为语义保持一致。

#### 当前扩展边界

当前代码已经存在多连接模式基础：

| 扩展点 | 当前实现 | 作用 |
|---|---|---|
| 连接模式选择 | `config::ConnectionMode` | 选择客户端上游连接模式 |
| hub 协议客户端 | `ahand_client::run(...)` | 连接 `ahand-hub` |
| 替代网关客户端 | `openclaw::OpenClawClient::run()` | 连接 OpenClaw Gateway |
| 共享本地运行时 | `JobRegistry`、`SessionManager`、`ApprovalManager`、`FileManager`、`BrowserManager` | 执行与安全能力复用 |

推荐新增方式：

```text
新增 ConnectionMode
-> 新增连接适配器
-> 复用现有本地运行时
-> 必要时扩展服务端能力声明与设备元数据
```

#### 实施步骤

1. 定义新客户端类型与接入目标。
2. 增加配置结构和 `ConnectionMode` 分支。
3. 实现新的连接适配器。
4. 将上游请求转换为现有本地处理入口。
5. 复用任务、审批、文件、浏览器、PTY 执行链路。
6. 如需区分能力，扩展设备元数据和能力声明。
7. 验证控制平面任务、输出、审批、终态语义不变。

#### 设计约束

新增客户端类型时不应：

- 复制本地执行栈
- 在控制平面增加大量客户端类型特定业务分支
- 将审批终裁迁移出客户端
- 让多个层次同时承担实际服务端职责

新增客户端类型应被视为**连接适配问题**，而不是重新设计任务执行体系。

---

### 6.2 新增客户端能力

如果需要增加新的客户端能力，例如新的结构化操作类型，应优先采用以下模式：

```text
控制平面新增 API
-> 实际服务端新增投递路径或复用设备消息路径
-> 客户端新增 handler
-> 客户端新增 manager / executor
-> 结果回传控制平面
```

新增能力应明确：

- 是否属于通用命令能力
- 是否需要独立策略
- 是否需要审批升级
- 是否需要输出流
- 是否需要 dashboard 订阅
- 是否需要持久化状态

如果该能力只是执行宿主机上的可执行程序，应优先使用通用 `JobRequest`。如果该能力需要结构化策略、结构化结果或特殊生命周期，则应设计为独立能力路径，例如现有的文件操作和浏览器自动化。

---

## 7. 结论

AHand 的架构可以概括为：

```text
控制平面负责任务与状态
实际服务端负责连接与投递
客户端负责判定与执行
```

该分层使系统能够同时满足：

- 本地执行边界可控
- 设备无需暴露公网入口
- 业务状态在控制平面统一收敛
- 交互式终端、文件、浏览器等能力可以按独立业务路径扩展
- 新客户端类型可以通过连接适配方式接入，而不破坏现有执行模型
