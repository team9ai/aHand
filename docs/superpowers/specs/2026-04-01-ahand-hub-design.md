# ahand-hub 设计规格

状态：已确认，待实现计划  
日期：2026-04-01

## 1. 背景

`aHand` 当前已经具备本地守护进程、协议定义、开发用云端服务和临时管理界面，但缺少一个可用于生产的控制中心服务。目标是在现有 monorepo 内新增一个 Rust 控制中心，使 agent 可以远程使用被注册到平台的电脑，执行命令、建立工作区、查询状态，并为后续的人类远程观察/接管、审批、审计和多协议兼容打下基础。

V1 的控制中心命名为 `ahand-hub`。

## 2. V1 目标与非目标

### 2.1 V1 目标

V1 只覆盖下面五类能力：

1. 设备注册与连接管理
2. 命令执行与流式输出
3. REST 管理 API
4. 审计日志
5. React Dashboard

### 2.2 V1 非目标

下面内容明确不进入 V1，实现时只需要保留扩展位：

1. 会话模式管理（`Inactive` / `Strict` / `Trust` / `AutoAccept`）
2. 审批流（approval request / response）
3. 浏览器自动化代理
4. 用户远程观察或接管设备
5. OpenClaw gateway node 协议兼容
6. 策略配置（Mode 5）
7. 组织/用户级多租户模型
8. 离线任务排队

## 3. 约束与明确决策

本规格基于以下已确认决策：

1. `ahand-hub` 放在 `aHand` monorepo 内，而不是独立仓库。
2. 架构采用“先单体后拆分”，但内部边界必须清晰。
3. 持久化使用 PostgreSQL，热数据和实时状态使用 Redis。
4. 设备认证以 Ed25519 为核心，同时支持 token 引导注册到密钥认证体系。
5. 对外接口采用 REST + WebSocket。
6. 初期以“设备”为隔离边界，不引入正式租户系统。
7. 内部调用通过预配置 service token，Dashboard 通过预共享密码换 JWT，外部使用业务后端签发的设备级 JWT。
8. crate 需要合理拆分，不接受把全部逻辑堆进一个 crate。
9. 测试是硬要求，Rust 和前端都以尽量接近 100% 覆盖率为目标。

## 4. 总体架构

V1 采用单进程部署，但在代码层显式拆成三个新 Rust crate 和一个前端应用：

```text
crates/
├── ahand-protocol/        # 现有 crate，继续作为协议类型来源
├── ahand-hub-core/        # 领域模型、状态机、trait、认证和业务规则
├── ahand-hub-store/       # PostgreSQL + Redis 适配层
└── ahand-hub/             # 二进制入口，REST、WebSocket、任务编排、后台任务

apps/
└── hub-dashboard/         # 新的 React/Next.js Dashboard
```

架构关系如下：

```text
Devices (ahandd)
    │
    │ WebSocket + protobuf envelope
    ▼
+-------------------------------+
|         ahand-hub             |
|                               |
|  +-------------------------+  |
|  | REST API / Dashboard WS |  |
|  +-------------------------+  |
|  | Device WS Gateway       |  |
|  +-------------------------+  |
|  | Runtime / Background     |  |
|  +------------+------------+  |
|               |               |
+---------------|---------------+
                |
                ▼
       +------------------+
       | ahand-hub-core   |
       +------------------+
                |
                ▼
       +------------------+
       | ahand-hub-store  |
       +------------------+
          |           |
          ▼           ▼
         PG         Redis
```

核心原则：

1. `ahand-hub` binary 必须保持薄，业务规则优先放在 `ahand-hub-core`。
2. 持久化和缓存实现全部下沉到 `ahand-hub-store`，避免网络层直接写 SQL 或 Redis 命令。
3. 所有状态流转都需要有明确状态机和可测试的边界。

## 5. Crate 拆分与职责

### 5.1 `crates/ahand-hub-core`

职责：

1. 定义领域模型：`Device`、`DeviceRegistration`、`DevicePresence`、`Job`、`JobStatus`、`AuditEntry`、`AuthContext`
2. 定义抽象 trait：`DeviceStore`、`JobStore`、`AuditStore`、`PresenceStore`、`TokenStore`
3. 实现业务服务：`DeviceManager`、`JobDispatcher`、`AuthService`、`AuditService`
4. 实现消息可靠传输所需的 `Outbox` 和重放逻辑
5. 统一错误模型和状态转换规则

约束：

1. 不直接依赖数据库或 Web 框架
2. 除时间、序列化和密码学外，不引入 IO 型依赖
3. 每个状态机转换都需要完整单元测试

### 5.2 `crates/ahand-hub-store`

职责：

1. 实现 `ahand-hub-core` 中定义的存储 trait
2. 提供 PostgreSQL 访问层和 migration
3. 提供 Redis 状态缓存、输出流和事件流
4. 封装 testcontainers 友好的测试辅助

约束：

1. 只做“存”和“取”，不承载业务决策
2. 对上暴露的是面向业务的接口，不泄漏 SQL 或 Redis 细节
3. migrations 由 `sqlx::migrate!` 管理并跟随 crate 版本

### 5.3 `crates/ahand-hub`

职责：

1. 组装配置、日志、存储、核心服务
2. 暴露 axum REST API
3. 维护设备 WebSocket 网关
4. 暴露 Dashboard 实时事件 WebSocket
5. 运行后台任务：心跳清理、审计批量写入、过期清理

约束：

1. handler 只做参数转换、鉴权和服务调用
2. 不在 handler 内拼装复杂业务流程
3. `main.rs` 只做启动编排

### 5.4 `apps/hub-dashboard`

职责：

1. 提供 Dashboard 登录、设备列表、任务列表、任务详情、审计日志
2. 消费 `ahand-hub` 的 REST API、SSE 和 Dashboard WebSocket
3. 复用 monorepo 的统一构建流程

## 6. 认证与身份模型

### 6.1 三类调用方

#### Service Token

用于内部服务，拥有管理员能力。典型使用方是 team9 自身微服务和内部自动化服务。

#### Dashboard JWT

用户通过预共享密码登录 Dashboard 后，由 `ahand-hub` 签发 JWT。V1 不做用户系统，只把 JWT 视作受信管理会话。

#### Device JWT

由业务后端签发，只允许访问自己的设备资源，用于外部接入场景。

### 6.2 设备认证

设备认证采用 Ed25519 为核心，token 只是注册或引导方式，不替代最终设备身份。

建议的握手方式：

1. 设备连入 `/ws`
2. 发送 `Hello` 消息
3. `Hello` 带认证信息：
   - `ed25519`：公钥 + 签名 + 签名时间戳
   - `bearer_token`：注册 token 或外部设备 JWT
4. `ahand-hub` 验签或验 token
5. 找到或创建设备记录
6. 标记在线并恢复未确认消息

首连绑定规则明确如下：

1. `POST /api/devices` 预注册时由 `ahand-hub` 生成一次性 bootstrap token
2. 设备首次连接时可携带 bootstrap token
3. 首次连接成功后，设备必须上送自己的 Ed25519 公钥
4. `ahand-hub` 将该公钥绑定到设备记录
5. 后续连接默认要求使用 Ed25519 签名
6. bootstrap token 使用一次后立即失效，不能长期作为设备主认证方式

签名内容固定为：

```text
ahand-hub|{device_id}|{signed_at_ms}
```

限制：

1. 签名有效窗口默认 5 分钟
2. 设备 `id` 取公钥派生值或注册时固定值，后续保持稳定
3. 不允许只靠匿名 `Hello` 建立业务连接

## 7. 设备连接与 WebSocket 设计

### 7.1 设备连接生命周期

```text
device -> connect /ws
device -> Hello(version, host, os, capabilities, last_ack, auth)
hub    -> verify auth
hub    -> upsert device record
hub    -> restore outbox with last_ack
hub    -> mark online in Redis
hub <-> exchange envelopes
hub <-> ping/pong heartbeat
disconnect -> keep replay buffer for retain window
timeout -> clean in-memory session and mark offline
```

### 7.2 在线状态

设备在线状态分两层：

1. 内存连接池：当前进程中真实活跃的 WebSocket 会话
2. Redis presence：带 TTL 的在线状态，用于 Dashboard 和多实例兼容准备

默认参数：

1. 心跳间隔：30 秒
2. 超时阈值：90 秒
3. 断线后 outbox 保留：10 分钟

### 7.3 Outbox 与可靠传输

V1 继续沿用 `seq/ack` 机制。每个设备会话维护一个有界 `Outbox`：

1. 下发消息时分配单调递增 `seq`
2. 对端回包带 `ack`
3. 服务端清理 `seq <= ack` 的缓存
4. 设备重连后用 `last_ack` 请求重放

`Outbox` 只缓存服务端下发给设备的消息。设备发给服务端的输出和状态事件写入 Redis/PG 后即视为已接收，不再做服务端重放。

### 7.4 Dashboard 实时推送

Dashboard 不直接消费 protobuf，而是连接 `/ws/dashboard`，接收 JSON 事件。事件类型至少包含：

1. `device.online`
2. `device.offline`
3. `job.created`
4. `job.running`
5. `job.finished`
6. `job.failed`
7. `job.cancelled`

## 8. 任务模型与执行流

### 8.1 任务状态机

```text
Pending -> Sent -> Running -> Finished
                        └-> Failed
                        └-> Cancelled

Pending -> Cancelled
Sent    -> Cancelled
Sent    -> Failed
Running -> Failed
```

非法状态跳转必须返回显式错误，不能静默覆盖。

### 8.2 任务提交流程

`POST /api/jobs` 的处理流程：

1. 鉴权并解析请求
2. 确认设备存在且在线
3. 在 PostgreSQL 创建 `jobs` 记录，状态为 `pending`
4. 记录审计事件 `job.created`
5. 通过设备 WebSocket 发送 `JobRequest`
6. 成功进入 `sent`
7. 返回 `202 Accepted`

V1 明确不支持离线排队。如果设备不在线，直接返回错误。

### 8.3 输出流

实时输出采用 Redis Streams：

```text
key: ahand:job:{job_id}:output
entry fields:
  type = stdout | stderr | finished | failed | cancelled
  data = chunk or json payload
```

设计要求：

1. Dashboard 打开任务详情时，先读取历史输出，再切实时订阅
2. REST 端点通过 SSE 暴露输出
3. 任务完成后，stream 设置 TTL，默认 1 小时
4. PostgreSQL 仅保存输出摘要，不保存全量流

### 8.4 取消与超时

取消流程：

1. 只有 `pending`、`sent`、`running` 可取消
2. `ahand-hub` 发送 `CancelJob`
3. 更新状态为 `cancelled`
4. 记录审计日志

超时流程：

1. 创建任务时启动本地超时计时器
2. 超时后下发 `CancelJob`
3. 若任务仍未正常结束，转为 `failed`，错误为 `timeout`

### 8.5 设备断线

V1 的策略是保守失败：

1. 如果任务还未开始运行，设备断线则任务失败
2. 如果任务处于 `running`，允许在 outbox 保留窗口内重连
3. 超过窗口仍未恢复则任务失败，错误为 `device disconnected`

## 9. REST API 设计

### 9.0 角色权限矩阵

| 角色 | 典型来源 | 权限边界 |
|------|----------|----------|
| `Admin` | service token | 完整设备管理、任务提交、任务取消、审计查看 |
| `DashboardUser` | Dashboard 登录 JWT | 设备和任务只读、审计查看、统计查看 |
| `Device` | 外部业务后端签发设备级 JWT | 只能访问自己的设备详情和与自己相关的受限资源 |

### 9.1 设备 API

```text
POST   /api/devices
GET    /api/devices
GET    /api/devices/:id
DELETE /api/devices/:id
GET    /api/devices/:id/capabilities
```

语义：

1. `POST /api/devices` 用于预注册设备，返回设备标识和后续连接所需信息
2. `GET /api/devices` 返回 PG 基础信息和 Redis 在线状态合并后的视图
3. `DELETE /api/devices/:id` 只允许管理员调用

权限：

1. `POST /api/devices` 仅 `Admin`
2. `GET /api/devices` 允许 `Admin`、`DashboardUser`
3. `GET /api/devices/:id` 允许 `Admin`、`DashboardUser`，以及访问自身记录的 `Device`
4. `DELETE /api/devices/:id` 仅 `Admin`

### 9.2 任务 API

```text
POST /api/jobs
GET  /api/jobs
GET  /api/jobs/:id
GET  /api/jobs/:id/output
POST /api/jobs/:id/cancel
```

约束：

1. `POST /api/jobs` 仅管理员可用
2. `GET /api/jobs` 和 `GET /api/jobs/:id` 允许 `Admin`、`DashboardUser`
3. `GET /api/jobs/:id/output` 使用 `text/event-stream`
4. `POST /api/jobs/:id/cancel` 仅 `Admin`
5. 列表 API 必须支持按 `device_id`、`status`、分页筛选

### 9.3 审计与系统 API

```text
GET  /api/audit-logs
POST /api/auth/login
GET  /api/auth/verify
GET  /api/health
GET  /api/stats
```

附加规则：

1. `POST /api/auth/login` 校验预共享密码，返回 Dashboard JWT
2. Dashboard 前端默认通过同源 cookie 使用该 JWT
3. `/ws/dashboard` 必须要求 `DashboardUser` 或 `Admin` 认证，不允许匿名连接
4. `GET /api/health` 可匿名，其余接口必须认证

### 9.4 错误模型

所有错误统一为：

```json
{
  "error": {
    "code": "DEVICE_OFFLINE",
    "message": "Device abc123 is not currently connected"
  }
}
```

V1 至少保留这些错误码：

1. `UNAUTHORIZED`
2. `FORBIDDEN`
3. `VALIDATION_ERROR`
4. `DEVICE_NOT_FOUND`
5. `DEVICE_OFFLINE`
6. `JOB_NOT_FOUND`
7. `JOB_NOT_CANCELLABLE`
8. `INTERNAL_ERROR`

## 10. 数据模型

### 10.1 PostgreSQL

#### `devices`

字段：

1. `id`：主键，设备标识
2. `public_key`：Ed25519 公钥，可为空
3. `hostname`
4. `os`
5. `capabilities`：`TEXT[]`
6. `version`
7. `auth_method`：`ed25519` 或 `token`
8. `registered_at`
9. `last_seen_at`
10. `metadata`：扩展 JSON

#### `jobs`

字段：

1. `id`：UUID
2. `device_id`
3. `tool`
4. `args`
5. `cwd`
6. `env`
7. `timeout_ms`
8. `status`
9. `exit_code`
10. `error`
11. `output_summary`
12. `requested_by`
13. `created_at`
14. `started_at`
15. `finished_at`

#### `audit_logs`

字段：

1. `id`
2. `timestamp`
3. `action`
4. `resource_type`
5. `resource_id`
6. `actor`
7. `detail`
8. `source_ip`

#### `auth_tokens`

字段：

1. `id`：token hash
2. `name`
3. `role`
4. `created_at`
5. `expires_at`
6. `last_used_at`

### 10.2 Redis

```text
ahand:device:{device_id}:online
ahand:device:{device_id}:meta
ahand:devices:online
ahand:job:{job_id}:output
ahand:job:{job_id}:status
ahand:events
ahand:auth:jwt:{token_hash}
```

用途：

1. `device:*` 维护 presence 和展示所需元数据
2. `job:*` 维护实时输出和短期状态缓存
3. `events` 向 Dashboard WebSocket 扇出事件
4. `auth:*` 缓存 JWT 验证结果

## 11. 审计日志

### 11.1 事件范围

V1 需要至少记录：

1. `device.registered`
2. `device.connected`
3. `device.disconnected`
4. `device.deleted`
5. `job.created`
6. `job.sent`
7. `job.running`
8. `job.finished`
9. `job.failed`
10. `job.cancelled`
11. `auth.login_success`
12. `auth.login_failed`

### 11.2 写入策略

审计日志不能阻塞主业务流，采用异步批量写：

1. 业务线程把审计事件写入有界 channel
2. 后台 writer 按“100 条或 500ms”批量刷入 PostgreSQL
3. 刷写失败自动重试
4. 若持续失败，写入本地兜底文件并打告警日志

### 11.3 保留策略

默认保留 90 天。每天定时清理过期审计数据。

## 12. Dashboard 设计

Dashboard 采用 React 技术栈，新建在 `apps/hub-dashboard`，参考 `openclaw-hive` dashboard 的交互组织方式，但不复用其业务模型。

### 12.1 技术栈

1. Next.js
2. React
3. TypeScript
4. Tailwind CSS
5. shadcn/ui
6. TanStack Query

### 12.2 页面

```text
/login
/
/devices
/devices/[id]
/jobs
/jobs/[id]
/audit-logs
```

### 12.3 页面职责

#### Overview

展示在线设备数、离线设备数、运行中任务数、最近活动。

#### Devices

展示设备列表，支持按状态筛选和按 `hostname`/`device_id` 搜索。

#### Device Detail

展示设备基本信息、能力、公钥指纹、最近任务历史和实时在线状态。

#### Jobs

展示任务列表，支持按状态和设备筛选。

#### Job Detail

展示任务元信息、状态时间线和实时输出终端视图。

#### Audit Logs

展示审计日志列表，支持按动作、资源、时间过滤，并可展开查看结构化 `detail`。

### 12.4 实时能力

1. 总览、设备状态和任务状态通过 `/ws/dashboard` 实时更新
2. 任务输出通过 SSE 更新
3. React Query 定时轮询作为降级路径

## 13. 配置与运行

配置来源按优先级覆盖：

1. CLI 参数
2. 环境变量
3. `config.toml`

环境变量前缀统一为：

```text
AHAND_HUB__
```

关键配置项：

1. HTTP/WS 监听地址和端口
2. PostgreSQL URL 和连接池大小
3. Redis URL 和连接池大小
4. Dashboard 密码 hash
5. JWT secret、TTL 和外部 JWT 验证信息
6. service token 列表
7. 心跳、outbox 和输出流 TTL
8. 审计批量写入和保留天数

启动流程固定为：

1. 加载配置
2. 初始化 tracing
3. 连接 PostgreSQL 并执行 migration
4. 连接 Redis
5. 构建 store 和 core service
6. 启动后台任务
7. 启动 axum server
8. 处理优雅退出

## 14. CI/CD 与构建

V1 需要新增面向 `ahand-hub` 的 CI：

1. `cargo fmt --check`
2. `cargo clippy --workspace -- -D warnings`
3. `cargo llvm-cov --workspace --fail-under-lines 100`
4. Dashboard 的 `pnpm test --coverage`

部署形式：

1. 单容器或单进程部署 `ahand-hub`
2. 依赖外部 PostgreSQL 和 Redis
3. Dashboard 可单独部署，也可后续嵌入 `ahand-hub` 静态资源服务

V1 不要求现在就拆服务，但 crate 边界必须保证未来能把 API 网关、存储适配层和实时层拆出去。

## 15. 测试策略

### 15.1 `ahand-hub-core`

目标是接近 100% 行覆盖率。所有状态机、鉴权、Outbox、审计事件生成必须用纯单元测试覆盖。

重点测试：

1. 设备注册、重复注册、非法认证
2. 设备上线/下线
3. 任务创建、取消、超时、非法状态跳转
4. JWT、service token、Ed25519 验签
5. Outbox 的缓存、ack 清理、重连重放、缓冲区溢出

### 15.2 `ahand-hub-store`

使用 testcontainers 跑真实 PostgreSQL + Redis 集成测试。

重点测试：

1. migrations 可从空库完整执行
2. `devices`/`jobs`/`audit_logs` 的 CRUD 和筛选
3. presence TTL
4. Redis Streams 的写入、读取、TTL
5. 审计批量写入和过期清理

### 15.3 `ahand-hub`

重点做 API 和 WebSocket 集成测试。

重点测试：

1. 各类 token 的鉴权
2. 设备 API 的权限和响应
3. 提交任务到设备并收到流式输出
4. 取消任务
5. 断线重连和消息重放
6. Dashboard WebSocket 事件推送

### 15.4 Dashboard

1. 组件和 hook 使用 Vitest + Testing Library
2. API 使用 MSW 模拟
3. 关键页面流程需要覆盖登录、设备列表、任务详情输出展示

## 16. 对现有协议和代码的影响

V1 需要对现有仓库做这些结构性补充：

1. 更新 workspace `Cargo.toml`，纳入新 crate
2. 扩展现有 protobuf `Hello` 相关消息以承载认证信息
3. 为 `ahandd` 增加与 `ahand-hub` 握手所需的认证和重连字段对齐
4. 新增 `apps/hub-dashboard`
5. 新增 `ahand-hub` 的 CI 配置、Docker 构建和 migrations

对现有 `ahandctl`、`apps/dev-cloud`、`packages/sdk` 的开发用途保留，但它们不再是生产控制中心的基础。

## 17. 后续 TODO

这些项目明确进入后续路线图，而不是 V1 的验收条件：

1. 审批流和会话模式
2. 浏览器自动化代理
3. 用户远程观察和接管设备
4. OpenClaw 协议兼容层
5. 更细粒度权限系统
6. 正式的组织/用户租户模型
7. 离线任务排队和调度
8. 多实例横向扩展时的连接路由和 leader 选举

## 18. 验收标准

V1 完成时至少满足下面结果：

1. 设备可通过认证后的 WebSocket 长连接注册到 `ahand-hub`
2. 内部服务可通过 REST API 向在线设备下发命令
3. 命令输出可以实时流式查看
4. 所有关键操作都有审计日志
5. Dashboard 可查看设备、任务和审计信息
6. 测试在 CI 中默认运行，并对核心逻辑设置严格覆盖率门槛

这份规格只描述“做什么”和“边界是什么”，不展开实现任务拆解。实现前应基于本文件再写一份正式 implementation plan。
