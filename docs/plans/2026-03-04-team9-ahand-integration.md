# Team9 × aHand 集成实施计划

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 让用户登录 Team9 桌面客户端后，无需任何手动配置，即可通过 Team9 频道与 OpenClaw AI 对话，并由 AI 控制和调用用户本地电脑上的工具。

**Architecture:** aHand 守护进程以 `ahand-cloud` 模式连接 Team9 服务器（而非直连 OpenClaw Gateway），Team9 服务端内嵌 `@ahand/sdk` 的 `AHandServer`，用用户现有的 JWT 认证，省去 Ed25519 配对流程。OpenClaw 通过调用 Team9 内部 API 向目标用户的本地机器下发 `JobRequest`，结果流式回传到频道。Tauri 桌面客户端在用户登录后自动拉起 aHand 守护进程并注入 token，用户完全无感知。

**Tech Stack:** Rust (tokio-tungstenite, serde) · NestJS (ws, @nestjs/jwt) · TypeScript (@ahand/sdk) · Tauri 2 · React + Zustand · protobuf (prost / @ahand/proto-ts)

---

## 整体数据流

```
[用户] → Team9 频道发消息
           ↓ Socket.io (Bot Token)
[Team9 Gateway] → 通知 OpenClaw Bot
           ↓ OpenClaw AI 推理
[OpenClaw] → POST /api/v1/ahand/exec {userId, tool, args, channelId}
           ↓ 查找 userId 对应的 DeviceConnection
[Team9 AHandWsService] → 发送 JobRequest (Protobuf over WebSocket)
           ↓ wss://team9-host/ws/ahand (JWT Auth)
[aHand daemon, 本地] → 执行命令，流式返回 stdout/stderr
           ↓ JobEvent / JobFinished (Protobuf)
[Team9 AHandWsService] → 收到结果，以 Bot 身份发送到频道
           ↓ Socket.io
[用户] ← 看到执行结果出现在频道里
```

---

## 涉及的代码库

| 路径 | 说明 |
|------|------|
| `aHand/` | 本地守护进程 + TypeScript SDK |
| `team9/apps/server/apps/gateway/` | NestJS 后端（主要修改区） |
| `team9/apps/client/` | React + Tauri 前端 |

---

## Phase 1：aHand — 为 ahand-cloud 模式增加 JWT 认证

当前 `ahand_client.rs` 连接时没有认证头，无法接入需要身份验证的 Team9 端点。

### Task 1.1：Config 增加顶层 `auth_token` 字段

**Files:**
- Modify: `crates/ahandd/src/config.rs`

**Step 1:** 在 `Config` 结构体内 `server_url` 字段之后，添加 `auth_token` 字段：

```rust
/// Bearer token sent as Authorization header during WebSocket upgrade.
/// Used when connecting to Team9 (ahand-cloud mode).
pub auth_token: Option<String>,
```

完整修改（在 `server_url` 下方插入）：

```rust
// In struct Config { ... }
/// WebSocket server URL (e.g. "wss://team9.example.com/ws/ahand")
#[serde(default = "default_server_url")]
pub server_url: String,

/// Bearer token for authenticating the WebSocket upgrade (ahand-cloud mode).
/// When set, sent as `Authorization: Bearer <token>` in the HTTP upgrade request.
pub auth_token: Option<String>,
```

**Step 2:** 确认 `Config::auth_token()` 访问方法存在（或直接访问 pub 字段即可）。

**Step 3:** 编译验证：
```bash
cd /Users/jiangtao/Desktop/shenjingyuan/aHand
cargo check -p ahandd
```
Expected: no errors

**Step 4:** Commit
```bash
git add crates/ahandd/src/config.rs
git commit -m "feat(ahandd): add auth_token field to Config for JWT auth"
```

---

### Task 1.2：ahand_client.rs 连接时携带 Authorization 头

**Files:**
- Modify: `crates/ahandd/src/ahand_client.rs`

**Step 1:** 找到 `connect()` 函数中的连接行（当前约第 75 行）：

```rust
let (ws_stream, _) = tokio_tungstenite::connect_async(url).await?;
```

**Step 2:** 替换为带认证头的版本：

```rust
// Build the WebSocket request, optionally adding an Authorization header.
let ws_stream = {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = url.into_client_request()?;
    if let Some(token) = config.auth_token.as_deref() {
        request.headers_mut().insert(
            tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
            format!("Bearer {token}").parse()?,
        );
    }
    let (ws, _) = tokio_tungstenite::connect_async(request).await?;
    ws
};
let (mut sink, mut stream) = ws_stream.split();
```

> `config` 需要在 `connect()` 的签名里传入或从闭包捕获。当前 `connect()` 已接收 `url: &str`，需额外传 `auth_token: Option<&str>`：

修改函数签名：
```rust
async fn connect(
    url: &str,
    auth_token: Option<&str>,   // ADD THIS
    device_id: &str,
    // ... 其余参数不变
) -> anyhow::Result<()>
```

在 `run()` 中调用处同步更新：
```rust
match connect(
    &config.server_url,
    config.auth_token.as_deref(),  // ADD THIS
    &device_id,
    // ...
).await
```

**Step 3:** 编译：
```bash
cargo check -p ahandd
```
Expected: no errors

**Step 4:** Commit
```bash
git add crates/ahandd/src/ahand_client.rs
git commit -m "feat(ahandd): pass Authorization header in WebSocket upgrade when auth_token set"
```

---

## Phase 2：Team9 Server — AHandModule

这是核心后端模块，负责：
1. 在 `/ws/ahand` 路径上接受来自 aHand daemon 的 WebSocket 连接
2. 用 JWT 验证身份，将连接绑定到 userId
3. 对外暴露 `getDevicesForUser(userId)` 和 `exec()` 方法

### Task 2.1：安装依赖

**Files:**
- Modify: `team9/apps/server/apps/gateway/package.json`

**Step 1:** 在 Team9 根目录安装 `ws` 和 `@ahand/sdk`（本地路径引用，生产环境改为 npm 包）：

```bash
cd /Users/jiangtao/Desktop/shenjingyuan/team9
pnpm --filter @team9/gateway add ws @types/ws
# @ahand/sdk 使用本地路径（开发阶段）
pnpm --filter @team9/gateway add @ahand/sdk@file:../../aHand/packages/sdk \
     @ahand/proto-ts@file:../../aHand/packages/proto-ts
```

> 如果路径引用有问题，可以先把 `packages/sdk/src/` 和 `packages/proto-ts/src/` 的文件复制到 `team9/apps/server/libs/ahand-sdk/` 下使用。

**Step 2:** 验证依赖可以 import：
```bash
cd team9/apps/server
pnpm build --filter @team9/gateway 2>&1 | head -20
```

---

### Task 2.2：创建 AHandWsService（核心服务）

**Files:**
- Create: `team9/apps/server/apps/gateway/src/ahand/ahand-ws.service.ts`

```typescript
import {
  Injectable,
  Logger,
  OnModuleDestroy,
  OnModuleInit,
} from '@nestjs/common';
import { JwtService } from '@nestjs/jwt';
import { WebSocketServer, WebSocket } from 'ws';
import type { IncomingMessage } from 'http';
import type { Duplex } from 'stream';
import { AHandServer, DeviceConnection } from '@ahand/sdk';
import { decodeEnvelope } from '@ahand/sdk/codec';
import { env } from '@team9/shared';

/** DeviceConnection with the owning userId attached. */
interface TaggedConnection {
  conn: DeviceConnection;
  userId: string;
}

@Injectable()
export class AHandWsService implements OnModuleInit, OnModuleDestroy {
  private readonly logger = new Logger(AHandWsService.name);

  /** Raw ws server, runs in noServer mode — we handle upgrade manually. */
  private readonly wss = new WebSocketServer({ noServer: true });

  /** @ahand/sdk server that manages Protobuf connection lifecycle. */
  private readonly ahandServer = new AHandServer();

  /** Maps userId → Set<deviceId> for reverse lookup. */
  private readonly userDevices = new Map<string, Set<string>>();

  /** Maps deviceId → userId. */
  private readonly deviceUser = new Map<string, string>();

  constructor(private readonly jwtService: JwtService) {}

  onModuleInit() {
    // Listen for new device connections so we can map userId → device.
    this.ahandServer.on('device', (conn: DeviceConnection) => {
      const userId = this.deviceUser.get(conn.deviceId);
      if (!userId) return;

      let set = this.userDevices.get(userId);
      if (!set) {
        set = new Set();
        this.userDevices.set(userId, set);
      }
      set.add(conn.deviceId);

      this.logger.log(
        `device connected: deviceId=${conn.deviceId} userId=${userId} hostname=${conn.hostname}`,
      );

      conn.on('close' as any, () => {
        set!.delete(conn.deviceId);
        if (set!.size === 0) this.userDevices.delete(userId);
        this.deviceUser.delete(conn.deviceId);
        this.logger.log(`device disconnected: deviceId=${conn.deviceId}`);
      });
    });
  }

  onModuleDestroy() {
    this.wss.close();
  }

  /**
   * Called from main.ts on HTTP 'upgrade' events for path /ws/ahand.
   * Authenticates the token, then hands the socket to AHandServer.
   */
  handleUpgrade(req: IncomingMessage, socket: Duplex, head: Buffer): void {
    // Extract token from Authorization header or ?token= query param.
    const token = this.extractToken(req);
    if (!token) {
      socket.write('HTTP/1.1 401 Unauthorized\r\n\r\n');
      socket.destroy();
      return;
    }

    let userId: string;
    try {
      const payload = this.jwtService.verify<{ sub: string }>(token, {
        publicKey: env.JWT_PUBLIC_KEY,
        algorithms: ['ES256'],
      });
      userId = payload.sub;
    } catch {
      socket.write('HTTP/1.1 401 Unauthorized\r\n\r\n');
      socket.destroy();
      return;
    }

    this.wss.handleUpgrade(req, socket as any, head, (ws) => {
      // Tag: intercept the first message to capture deviceId before AHandServer does.
      // EventEmitter calls listeners in registration order, so 'once' fires first.
      ws.once('message', (raw: Buffer) => {
        try {
          const envelope = decodeEnvelope(new Uint8Array(raw));
          if (envelope.deviceId) {
            this.deviceUser.set(envelope.deviceId, userId);
          }
        } catch {
          // Ignore parse errors — AHandServer will close with 1002.
        }
      });

      this.ahandServer.handleSocket(ws as any);
    });
  }

  /** Return the first connected device for a userId, or undefined. */
  getDeviceForUser(userId: string): DeviceConnection | undefined {
    const ids = this.userDevices.get(userId);
    if (!ids || ids.size === 0) return undefined;
    const deviceId = ids.values().next().value!;
    return this.ahandServer.device(deviceId);
  }

  /** Return all connected devices for a userId. */
  getDevicesForUser(userId: string): DeviceConnection[] {
    const ids = this.userDevices.get(userId);
    if (!ids) return [];
    return [...ids]
      .map((id) => this.ahandServer.device(id))
      .filter((d): d is DeviceConnection => d !== undefined);
  }

  /** Is this user currently connected with at least one local device? */
  isUserConnected(userId: string): boolean {
    return (this.userDevices.get(userId)?.size ?? 0) > 0;
  }

  private extractToken(req: IncomingMessage): string | null {
    // 1. Authorization: Bearer <token>
    const auth = req.headers['authorization'];
    if (auth?.startsWith('Bearer ')) {
      return auth.slice(7);
    }
    // 2. ?token=<token> query param
    const url = req.url ?? '';
    const i = url.indexOf('?token=');
    if (i !== -1) {
      return decodeURIComponent(url.slice(i + 7).split('&')[0]);
    }
    return null;
  }
}
```

---

### Task 2.3：创建 AHandModule

**Files:**
- Create: `team9/apps/server/apps/gateway/src/ahand/ahand.module.ts`

```typescript
import { Module } from '@nestjs/common';
import { JwtModule } from '@nestjs/jwt';
import { env } from '@team9/shared';
import { AHandWsService } from './ahand-ws.service.js';
import { AHandController } from './ahand.controller.js';

@Module({
  imports: [
    JwtModule.register({
      publicKey: env.JWT_PUBLIC_KEY,
      verifyOptions: { algorithms: ['ES256'] },
    }),
  ],
  providers: [AHandWsService],
  controllers: [AHandController],
  exports: [AHandWsService],
})
export class AHandModule {}
```

---

### Task 2.4：注册到 AppModule

**Files:**
- Modify: `team9/apps/server/apps/gateway/src/app.module.ts`

在 `imports` 数组中加入 `AHandModule`：

```typescript
import { AHandModule } from './ahand/ahand.module.js';

// In @Module({ imports: [ ... ] })
AHandModule,
```

---

### Task 2.5：在 main.ts 挂载 WebSocket 升级处理

**Files:**
- Modify: `team9/apps/server/apps/gateway/src/main.ts`

在 `app.listen(port)` **之前**加入：

```typescript
import { AHandWsService } from './ahand/ahand-ws.service.js';

// After: const app = await NestFactory.create(AppModule);
// Before: await app.listen(port, '0.0.0.0');

const httpServer = app.getHttpServer();
const ahandWs = app.get(AHandWsService);

httpServer.on('upgrade', (req, socket, head) => {
  if (req.url?.startsWith('/ws/ahand')) {
    ahandWs.handleUpgrade(req, socket, head);
  }
  // Socket.io handles its own upgrade path — do not interfere.
});

logger.log('aHand WebSocket endpoint registered at /ws/ahand');
```

**Step 1:** 启动服务验证：
```bash
cd team9/apps/server
pnpm start:dev:gateway 2>&1 | grep -E "aHand|error|Error" | head -20
```
Expected: `aHand WebSocket endpoint registered at /ws/ahand`

**Step 2:** Commit
```bash
git add apps/server/apps/gateway/src/ahand/ \
        apps/server/apps/gateway/src/app.module.ts \
        apps/server/apps/gateway/src/main.ts
git commit -m "feat(team9-server): add AHandModule — WebSocket endpoint for local device connections"
```

---

## Phase 3：Team9 Server — Execute API

OpenClaw 调用此 API 向用户本地机器下发命令，结果以 Bot 消息形式回传到指定频道。

### Task 3.1：创建 ExecJobDto

**Files:**
- Create: `team9/apps/server/apps/gateway/src/ahand/dto/exec-job.dto.ts`

```typescript
import { IsString, IsArray, IsOptional, IsNumber, IsObject } from 'class-validator';

export class ExecJobDto {
  /** Team9 userId of the target user whose local machine should run the command. */
  @IsString()
  targetUserId!: string;

  /** Executable name, e.g. "bash", "git", "rg". */
  @IsString()
  tool!: string;

  /** Argument list. */
  @IsArray()
  @IsString({ each: true })
  args: string[] = [];

  /** Channel where stdout/stderr will be posted as a Bot message. */
  @IsString()
  @IsOptional()
  channelId?: string;

  /** Timeout in milliseconds. Default: 60000. */
  @IsNumber()
  @IsOptional()
  timeoutMs?: number;

  /** Working directory on the remote machine. */
  @IsString()
  @IsOptional()
  cwd?: string;

  /** Extra environment variables. */
  @IsObject()
  @IsOptional()
  env?: Record<string, string>;
}
```

---

### Task 3.2：创建 AHandController

**Files:**
- Create: `team9/apps/server/apps/gateway/src/ahand/ahand.controller.ts`

```typescript
import {
  Controller,
  Post,
  Body,
  NotFoundException,
  ServiceUnavailableException,
  Logger,
} from '@nestjs/common';
import { UseGuards } from '@nestjs/common';
import { JwtAuthGuard } from '../auth/guards/jwt-auth.guard.js';
import { CurrentUser } from '../auth/decorators/current-user.decorator.js';
import { AHandWsService } from './ahand-ws.service.js';
import { ExecJobDto } from './dto/exec-job.dto.js';
import { MessagesService } from '../im/messages/messages.service.js';
import { BotService } from '../bot/bot.service.js';

@Controller('ahand')
export class AHandController {
  private readonly logger = new Logger(AHandController.name);

  constructor(
    private readonly ahandWs: AHandWsService,
    private readonly messagesService: MessagesService,
    private readonly botService: BotService,
  ) {}

  /**
   * Execute a local command on the target user's machine.
   * Called by OpenClaw when it needs to run a tool locally.
   *
   * Auth: the requesting bot must be authenticated (Bot Token or JWT).
   * Returns the full stdout/stderr once the command completes.
   */
  @UseGuards(JwtAuthGuard)
  @Post('exec')
  async execJob(
    @CurrentUser('sub') callerId: string,
    @Body() dto: ExecJobDto,
  ) {
    const device = this.ahandWs.getDeviceForUser(dto.targetUserId);
    if (!device) {
      throw new ServiceUnavailableException(
        `No local device connected for user ${dto.targetUserId}. ` +
          `Ask them to open the Team9 desktop app.`,
      );
    }

    const t0 = Date.now();
    this.logger.log(
      `exec: caller=${callerId} target=${dto.targetUserId} tool=${dto.tool} args=${dto.args.join(' ')}`,
    );

    const job = device.exec(dto.tool, dto.args, {
      cwd: dto.cwd,
      env: dto.env,
      timeoutMs: dto.timeoutMs ?? 60_000,
    });

    // Collect streaming output.
    const stdoutChunks: string[] = [];
    const stderrChunks: string[] = [];

    job.on('stdout', (chunk: string) => stdoutChunks.push(chunk));
    job.on('stderr', (chunk: string) => stderrChunks.push(chunk));

    const result = await job.result();

    const elapsed = Date.now() - t0;
    this.logger.log(
      `exec done: exit=${result.exitCode} ms=${elapsed} tool=${dto.tool}`,
    );

    const stdout = stdoutChunks.join('');
    const stderr = stderrChunks.join('');

    return {
      exitCode: result.exitCode,
      stdout,
      stderr,
      elapsedMs: elapsed,
    };
  }

  /** Check if a user currently has a local device connected. */
  @UseGuards(JwtAuthGuard)
  @Post('status')
  getStatus(@Body('userId') userId: string) {
    const connected = this.ahandWs.isUserConnected(userId);
    const devices = this.ahandWs.getDevicesForUser(userId).map((d) => ({
      deviceId: d.deviceId,
      hostname: d.hostname,
      os: d.os,
      capabilities: d.capabilities,
      connected: d.connected,
    }));
    return { connected, devices };
  }
}
```

**Step 1:** 编译验证：
```bash
cd team9/apps/server
pnpm build --filter @team9/gateway 2>&1 | tail -10
```
Expected: build 成功，无 TS 错误

**Step 2:** Commit
```bash
git add apps/server/apps/gateway/src/ahand/
git commit -m "feat(team9-server): add AHand execute API for OpenClaw to run local commands"
```

---

## Phase 4：Team9 Tauri 客户端 — 自动管理 aHand 守护进程

用户登录后 Tauri 自动在后台启动 aHand daemon，用户无需任何手动操作。

### Task 4.1：Tauri Cargo.toml 添加依赖

**Files:**
- Modify: `team9/apps/client/src-tauri/Cargo.toml`

在 `[dependencies]` 中添加：
```toml
tokio = { version = "1", features = ["full"] }
serde_json = "1"
dirs = "5"
```

---

### Task 4.2：创建 ahand.rs — 守护进程管理模块

**Files:**
- Create: `team9/apps/client/src-tauri/src/ahand.rs`

```rust
//! Manages the local aHand daemon lifecycle.
//!
//! On login: writes ~/.ahand/config.toml with the user's JWT, then spawns ahandd.
//! On logout / app exit: kills the daemon process.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Mutex;

/// Global handle to the spawned daemon process.
static DAEMON: Mutex<Option<Child>> = Mutex::new(None);

/// Path to the aHand binary. Checks ~/.ahand/bin/ahandd first, then PATH.
fn find_binary() -> Option<PathBuf> {
    // 1. ~/.ahand/bin/ahandd (installed by aHandctl)
    if let Some(home) = dirs::home_dir() {
        let candidate = home.join(".ahand").join("bin").join("ahandd");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    // 2. ahandd in PATH
    which_binary("ahandd")
}

fn which_binary(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            let candidate = dir.join(name);
            candidate.exists().then_some(candidate)
        })
    })
}

/// Writes ~/.ahand/config.toml with the Team9 server URL and user JWT.
fn write_config(server_url: &str, auth_token: &str) -> std::io::Result<()> {
    let config_path = dirs::home_dir()
        .expect("no home dir")
        .join(".ahand")
        .join("config.toml");

    std::fs::create_dir_all(config_path.parent().unwrap())?;

    let content = format!(
        r#"# Auto-generated by Team9 desktop app. Do not edit manually.
mode = "ahand-cloud"
server_url = "{server_url}"
auth_token = "{auth_token}"
default_session_mode = "auto_accept"
"#
    );

    std::fs::write(&config_path, content)?;
    Ok(())
}

/// Start the daemon. Safe to call multiple times — kills the old one first.
pub fn start(server_url: &str, auth_token: &str) -> Result<(), String> {
    stop(); // kill any previously managed instance

    let binary = find_binary().ok_or("aHand not installed. Please run the installer first.")?;

    write_config(server_url, auth_token).map_err(|e| e.to_string())?;

    let config_path = dirs::home_dir()
        .unwrap()
        .join(".ahand")
        .join("config.toml");

    let child = Command::new(&binary)
        .arg("--config")
        .arg(&config_path)
        .spawn()
        .map_err(|e| format!("failed to start ahandd: {e}"))?;

    *DAEMON.lock().unwrap() = Some(child);
    Ok(())
}

/// Stop the daemon if we started it.
pub fn stop() {
    if let Some(mut child) = DAEMON.lock().unwrap().take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Is the daemon currently running (did we start it)?
pub fn is_running() -> bool {
    DAEMON.lock().unwrap().is_some()
}
```

---

### Task 4.3：注册 Tauri 命令

**Files:**
- Modify: `team9/apps/client/src-tauri/src/lib.rs`

```rust
mod ahand;

#[tauri::command]
fn ahand_start(server_url: String, auth_token: String) -> Result<(), String> {
    ahand::start(&server_url, &auth_token)
}

#[tauri::command]
fn ahand_stop() {
    ahand::stop();
}

#[tauri::command]
fn ahand_is_running() -> bool {
    ahand::is_running()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            ahand_start,
            ahand_stop,
            ahand_is_running
        ])
        .on_window_event(|_win, event| {
            // Clean up daemon when the app window is destroyed.
            if let tauri::WindowEvent::Destroyed = event {
                ahand::stop();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

**Step 1:** 编译 Tauri 后端：
```bash
cd team9/apps/client
pnpm tauri build --debug 2>&1 | tail -20
```
Expected: 编译通过

**Step 2:** Commit
```bash
git add apps/client/src-tauri/src/ahand.rs \
        apps/client/src-tauri/src/lib.rs \
        apps/client/src-tauri/Cargo.toml
git commit -m "feat(tauri): auto-start aHand daemon on login, stop on app exit"
```

---

### Task 4.4：React 侧在登录成功后调用 Tauri 命令

找到 Team9 客户端的登录成功回调位置（通常在 auth store 或 login hook 里），加入如下调用：

**Files:**
- 搜索并修改处理登录成功的文件（如 `apps/client/src/stores/auth.store.ts` 或类似）

```typescript
import { invoke } from '@tauri-apps/api/core';
import { isTauri } from '@tauri-apps/api/core'; // or check window.__TAURI__

// After successful login, when accessToken is obtained:
async function onLoginSuccess(accessToken: string) {
  // ... existing logic (store token, redirect, etc.) ...

  // Start aHand daemon if running inside Tauri desktop app.
  if (typeof window !== 'undefined' && (window as any).__TAURI__) {
    const serverUrl = `${import.meta.env.VITE_API_WS_URL ?? 'wss://team9.example.com'}/ws/ahand`;
    try {
      await invoke('ahand_start', { serverUrl, authToken: accessToken });
      console.info('[aHand] daemon started');
    } catch (err) {
      console.warn('[aHand] could not start daemon:', err);
      // Non-fatal — app works without local device.
    }
  }
}

// On logout:
async function onLogout() {
  // ... existing logout logic ...
  if ((window as any).__TAURI__) {
    await invoke('ahand_stop').catch(() => {});
  }
}
```

> 具体注入位置取决于 auth store 的实现，搜索 `accessToken` 赋值处或 `login` action 即可找到。

**Step 1:** 在 `VITE_API_WS_URL` 环境变量里设置 WebSocket 域名（与 HTTP API 域名相同，协议改为 `wss://`）。在 `apps/client/.env.local` 中：
```
VITE_API_WS_URL=wss://your-team9-domain.com
```

**Step 2:** Commit
```bash
git commit -m "feat(client): invoke ahand_start/stop Tauri commands on login/logout"
```

---

## Phase 5：Team9 React 客户端 — 本地设备状态显示

让用户能看到本地设备是否已连接，降低迷茫感（万一 daemon 没启动）。

### Task 5.1：创建 useAHandStatus hook

**Files:**
- Create: `team9/apps/client/src/hooks/useAHandStatus.ts`

```typescript
import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';

export type AHandStatus = 'connected' | 'disconnected' | 'not-desktop';

export function useAHandStatus(): AHandStatus {
  const [status, setStatus] = useState<AHandStatus>('not-desktop');

  useEffect(() => {
    if (!(window as any).__TAURI__) {
      setStatus('not-desktop');
      return;
    }

    let interval: ReturnType<typeof setInterval>;

    const check = async () => {
      try {
        const running = await invoke<boolean>('ahand_is_running');
        setStatus(running ? 'connected' : 'disconnected');
      } catch {
        setStatus('disconnected');
      }
    };

    void check();
    interval = setInterval(check, 5000);
    return () => clearInterval(interval);
  }, []);

  return status;
}
```

---

### Task 5.2：创建 LocalDeviceStatus 组件

**Files:**
- Create: `team9/apps/client/src/components/layout/LocalDeviceStatus.tsx`

```tsx
import { useAHandStatus } from '../../hooks/useAHandStatus.js';

export function LocalDeviceStatus() {
  const status = useAHandStatus();

  if (status === 'not-desktop') return null; // Web 版不显示

  return (
    <div className="flex items-center gap-1.5 px-3 py-1 text-xs text-muted-foreground">
      <span
        className={`h-2 w-2 rounded-full ${
          status === 'connected' ? 'bg-green-500' : 'bg-yellow-500 animate-pulse'
        }`}
      />
      {status === 'connected' ? '本地已连接' : '本地未连接'}
    </div>
  );
}
```

**Step 1:** 在侧边栏底部（或用户头像旁）引入此组件：
```tsx
import { LocalDeviceStatus } from '../layout/LocalDeviceStatus.js';
// In your sidebar component:
<LocalDeviceStatus />
```

**Step 2:** Commit
```bash
git commit -m "feat(client): add LocalDeviceStatus indicator in sidebar"
```

---

## Phase 6：集成验证（端到端测试）

### Task 6.1：手动端到端测试

**前置条件：**
- Team9 服务端已部署（或本地 `pnpm dev`）
- aHand daemon 已编译 (`cargo build -p ahandd`)
- 已有一个 Team9 账号 + OpenClaw 实例已安装在工作区

**测试步骤：**

**Step 1:** 启动 Team9 服务：
```bash
cd team9 && pnpm dev
```

**Step 2:** 手动用 curl 模拟 aHand daemon 连接（验证 WebSocket 端点和 JWT 认证）：
```bash
# 获取 JWT
TOKEN=$(curl -s -X POST http://localhost:3000/api/v1/auth/login \
  -H "Content-Type: application/json" \
  -d '{"email":"test@example.com","password":"..."}' | jq -r .accessToken)

# 连接 aHand WebSocket（应返回 101 Switching Protocols）
wscat -c "ws://localhost:3000/ws/ahand" \
  -H "Authorization: Bearer $TOKEN"
```
Expected: WebSocket 握手成功

**Step 3:** 配置本地 aHand daemon 连接 Team9：
```toml
# ~/.ahand/config.toml
mode = "ahand-cloud"
server_url = "ws://localhost:3000/ws/ahand"
auth_token = "<your-JWT>"
default_session_mode = "auto_accept"
```

**Step 4:** 启动 aHand daemon：
```bash
ahandd --config ~/.ahand/config.toml
```
Expected: 日志显示 `connected, sending Hello`

**Step 5:** 模拟 OpenClaw 调用 execute API：
```bash
curl -X POST http://localhost:3000/api/v1/ahand/exec \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "targetUserId": "<your-user-id>",
    "tool": "echo",
    "args": ["hello from local machine!"]
  }'
```
Expected:
```json
{
  "exitCode": 0,
  "stdout": "hello from local machine!\n",
  "stderr": "",
  "elapsedMs": 45
}
```

**Step 6:** Commit
```bash
git commit -m "test: add e2e integration notes for Team9 × aHand local device execution"
```

---

## Phase 7（可选后续）：OpenClaw 调用本地工具的胶水层

当 OpenClaw AI 决定调用 `system.run` 时，它应该调用 Team9 的 `/api/v1/ahand/exec`，而非直连 OpenClaw gateway。这部分在 OpenClaw 内部实现，主要是：

1. OpenClaw 收到消息时，获取发送者的 `userId`（从 `TEAM9_BASE_URL` + bot token 可以查询）
2. 调用 `POST {TEAM9_BASE_URL}/api/v1/ahand/exec` 时携带自己的 bot token 认证
3. 将返回的 stdout 作为 AI 工具调用结果继续推理
4. 将结果以流式消息形式发回频道

> 这部分需要在 OpenClaw 侧实现，已超出 aHand/Team9 代码库范围，属于 OpenClaw 运行时的配置工作。

---

## 实施优先级与依赖关系

```
Phase 1 (aHand JWT auth)
   └── Phase 2 (Team9 AHandModule)
          └── Phase 3 (Execute API)
                └── Phase 4 (Tauri auto-start)
                       └── Phase 5 (UI status badge)
                              └── Phase 6 (E2E 验证)
                                     └── Phase 7 (OpenClaw 内部接入, 可选)
```

每个 Phase 完成后都可以独立验证，无需等待后续 Phase 完成。

---

## 关键配置项汇总

| 配置 | 位置 | 值 |
|------|------|-----|
| Team9 WebSocket 端点 | `main.ts` upgrade handler | `/ws/ahand` |
| aHand 连接模式 | `~/.ahand/config.toml` | `mode = "ahand-cloud"` |
| aHand JWT token | `~/.ahand/config.toml` | `auth_token = "eyJ..."` |
| aHand 会话模式 | `~/.ahand/config.toml` | `default_session_mode = "auto_accept"` |
| 环境变量（Team9） | `.env` | `JWT_PUBLIC_KEY=...` |
| 环境变量（前端） | `.env.local` | `VITE_API_WS_URL=wss://...` |

---

## 安全注意事项

1. **JWT 短期有效性**：aHand 使用用户 JWT 建立 WebSocket 连接。JWT 可能过期（通常 15min-1h）。需在 Phase 4 中加入 token 刷新逻辑——Tauri 监听 `accessToken` 刷新事件，重新调用 `ahand_start` 更新配置并重启 daemon。
2. **`auto_accept` 模式**：计划中使用 `auto_accept` 简化体验，但这意味着任何通过验证的 Team9 调用都可以在用户电脑上执行命令。**建议后续加入频道/用户级别的白名单控制**（利用 aHand 现有的 `policy` 系统）。
3. **命令注入防范**：`ExecJobDto.tool` 和 `args` 应通过 aHand 的 `policy.allowed_tools` 白名单过滤，在 aHand daemon 侧执行，而非 Team9 服务端。
4. **HTTPS/WSS 必需**：生产环境必须使用 `wss://`（TLS），避免 JWT 在传输中泄露。
