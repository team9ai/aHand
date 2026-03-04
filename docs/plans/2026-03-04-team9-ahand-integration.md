# Team9 × aHand 集成实施计划（修订版 v3）

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 让用户登录 Team9 桌面客户端后，无需任何手动配置，即可通过 Team9 频道与 OpenClaw AI 对话，AI 能远程控制和调用用户本地电脑上的工具。

**Architecture:** aHand 以现有的 `openclaw-gateway` 模式直连 OpenClaw 实例的 Gateway（Traefik 已将端口 18789 公开暴露），Tauri 客户端在用户登录后自动从 Team9 API 获取 gateway URL、写入 aHand 配置并启动守护进程。设备配对通过 Team9 API 自动审批，无需人工操作。OpenClaw 工具执行（`system.run` → `node.invoke.request`）完全不需要改动。

**Tech Stack:** Rust (tokio) · NestJS · TypeScript · Tauri 2 · React

---

## v3 相对 v2 的主要修订

| 问题 | v2 错误 | v3 修正 |
|------|---------|---------|
| config.toml 格式 | `auth_token` 写在顶层 | `auth_token` 移入 `[openclaw]` section |
| self-approve 安全 | 只验证 app 归属，不验证 requestId 有效性 | 调用 `listDevices` 确认 requestId 为 pending 状态再审批 |
| installedAppId 来源 | 未给出具体代码 | 调用 `applicationsApi.getInstalledApplications()` 按 `applicationId === 'openclaw'` 过滤 |
| 登录 Hook 注入点 | 错误指向不存在的 `auth.store.ts` | 注入到 `_authenticated.tsx` layout 和 `useLogout` |
| 设备配对重复审批 | 每次启动均轮询，不处理已配对情况 | 未找到 pending 请求时视为已配对，日志提示，不报错 |
| node_id 持久化 | 未处理，每次重启可能生成新 ID | 持久化到 `~/.ahand/team9-device-id`，写入 config.toml |
| Cargo.toml 依赖 | 冗余的 `tokio = ["full"]` | 移除 tokio（Tauri 已内置），只加 `dirs` |
| Phase 4 实现状态 | 计划修改 prepare.ts | `SenderName`/`SenderId` 已在 ctxPayload 中，仅需验证 |

---

## 为什么不是 "通过 Team9 服务端中转"（Path B）

原计划让 aHand 连接 Team9 服务端，再由 Team9 桥接到 OpenClaw。这是不必要的复杂度，原因如下：

1. **Gateway 已公开**：openclaw-hive 的 Traefik 配置已将每个实例的 18789 端口对外暴露，aHand 可以直接连接。
2. **工具执行链完整**：OpenClaw AI 调用 `system.run` → 走本地 gateway (`ws://127.0.0.1:18789`) → 路由到已配对的 aHand 节点。这条链路原生支持，**不需要任何修改**。
3. **唯一的问题只是"配置"**：用户不知道 gateway URL，不会操作 `ahandctl configure`。这用 Tauri 自动配置解决即可。

---

## 完整数据流

```
[用户] Team9 频道发消息
         │ Socket.io (bot token)
         ▼
[Team9 Server] 把消息推给 OpenClaw Bot
         │ WS event: new_message
         ▼
[OpenClaw 云端实例] AI 推理，决定调用本地工具
         │ ws://127.0.0.1:18789  (container 内部)
         ▼
[OpenClaw Gateway] 查找已配对节点，发 node.invoke.request
         │ wss://instance-id.openclaw.cloud:18789  (Traefik 暴露)
         ▼
[aHand daemon, 本地] 执行命令，流式返回 stdout/stderr
         │ WebSocket (OpenClaw Gateway 协议)
         ▼
[OpenClaw 实例] 收到输出，AI 继续推理
         │ HTTP POST /api/v1/im/channels/:id/messages
         ▼
[用户] 看到结果出现在 Team9 频道
```

---

## 涉及的代码库

| 仓库 | 改动量 |
|------|--------|
| `team9/apps/server/` | 中 — 新增 2 个 API 端点 |
| `team9/apps/client/` | 中 — Tauri 自动启动逻辑 + UI |
| `openclaw-hive/openclaw/extensions/team9/` | 无 — 已实现，仅验证 |
| `aHand/` | 无 |

---

## Phase 1：Team9 Server — 暴露 Gateway 连接信息 API

用户登录后，Tauri 需要知道：①该用户的 OpenClaw 实例 gateway URL 是什么；②设备配对请求发出后自动审批。

### Task 1.1：为已安装的 OpenClaw 应用新增 "gateway-info" 端点

**Files:**
- Modify: `team9/apps/server/apps/gateway/src/applications/installed-applications.controller.ts`

在现有 controller 中加入（与 `createOpenClawAgent` 同级）：

```typescript
/**
 * Return the OpenClaw gateway WebSocket URL for the calling user's workspace.
 * Used by Tauri desktop client to auto-configure the local aHand daemon.
 *
 * Auth: any authenticated workspace member (controller-level AuthGuard + WorkspaceGuard).
 */
@Get(':id/openclaw/gateway-info')
async getOpenClawGatewayInfo(
  @Param('id') id: string,
  @CurrentTenantId() tenantId: string,
) {
  const app = await this.getVerifiedApp(id, tenantId, 'openclaw');
  const instancesId = (app.config as { instancesId?: string })?.instancesId;
  if (!instancesId) {
    throw new NotFoundException('No instance configured for this application');
  }

  // access_url is stored in secrets after installation
  const secrets = (app.secrets as { instanceResult?: { access_url?: string } }) ?? {};
  const accessUrl = secrets.instanceResult?.access_url;

  if (!accessUrl) {
    throw new ServiceUnavailableException('Gateway URL not available yet');
  }

  // Convert HTTP(S) access_url to WS(S) gateway URL.
  // e.g. https://instance-id.openclaw.cloud → wss://instance-id.openclaw.cloud:18789
  const gatewayUrl = accessUrl
    .replace(/^https:\/\//, 'wss://')
    .replace(/^http:\/\//, 'ws://')
    .replace(/\/$/, '') + ':18789';

  return {
    instanceId: instancesId,
    gatewayUrl,
    gatewayPort: 18789,
  };
}
```

> **注意：** `app.secrets` 的结构依赖 OpenClaw 安装流程写入的字段。实现前先检查一条已安装记录确认 `secrets.instanceResult.access_url` 存在：
> ```bash
> # 在 team9/apps/server 目录执行，查看已安装 openclaw 应用的 secrets 字段结构
> cd team9/apps/server && pnpm repl  # 或查询数据库
> ```

**Step 1:** 启动 Team9 服务，调用接口验证返回格式：
```bash
curl -H "Authorization: Bearer $TOKEN" \
  "http://localhost:3000/api/v1/installed-applications/$APP_ID/openclaw/gateway-info"
```
Expected:
```json
{ "instanceId": "xxx", "gatewayUrl": "wss://xxx.openclaw.cloud:18789", "gatewayPort": 18789 }
```

**Step 2:** Commit
```bash
git add apps/server/apps/gateway/src/applications/installed-applications.controller.ts
git commit -m "feat(team9-server): expose OpenClaw gateway URL endpoint for Tauri client"
```

---

### Task 1.2：设备配对自动审批端点

当 aHand daemon 发出配对请求时，Tauri 立即调用此端点自动审批，无需管理员手动点。

**Files:**
- Modify: `team9/apps/server/apps/gateway/src/applications/installed-applications.controller.ts`

在现有 `POST :id/openclaw/devices/approve`（需要 admin 权限）**上方**新增一个允许普通用户自审批自己设备的端点：

```typescript
/**
 * Self-approve a pending device pairing for the calling user's own device.
 * Used by Tauri to auto-approve without requiring admin intervention.
 *
 * Security: verifies the requestId is a valid pending request before approving.
 * Any authenticated workspace member can call this, but only for pending requests
 * on their own workspace's OpenClaw instance.
 */
@Post(':id/openclaw/devices/self-approve')
async selfApproveOpenClawDevice(
  @Param('id') id: string,
  @CurrentTenantId() tenantId: string,
  @Body('requestId') requestId: string,
) {
  const app = await this.getVerifiedApp(id, tenantId, 'openclaw');
  const instancesId = (app.config as { instancesId?: string })?.instancesId;
  if (!instancesId) throw new NotFoundException('No instance configured');

  // Verify the requestId exists and is actually in pending state.
  // This prevents approving already-approved/rejected requests or invalid IDs.
  const devices = await this.openclawService.listDevices(instancesId);
  const target = devices?.find(
    (d) => d.request_id === requestId && d.status === 'pending',
  );
  if (!target) {
    throw new NotFoundException(
      'No pending device pairing request found with this ID',
    );
  }

  await this.openclawService.approveDevice(instancesId, requestId);
  return { approved: true, requestId };
}
```

**Step 1:** 编译验证：
```bash
cd team9/apps/server && pnpm build --filter @team9/gateway 2>&1 | tail -5
```
Expected: `Build completed successfully` 或无 TypeScript 错误。

**Step 2:** Commit
```bash
git commit -m "feat(team9-server): add self-approve endpoint for aHand device pairing"
```

---

### Task 1.3：前端新增 API 方法

**Files:**
- Modify: `team9/apps/client/src/services/api/applications.ts`

在 `applicationsApi` 对象中追加以下方法（放在 `rejectOpenClawDevice` 之后）：

```typescript
getOpenClawGatewayInfo: async (
  installedAppId: string,
): Promise<{ instanceId: string; gatewayUrl: string; gatewayPort: number }> => {
  const response = await http.get<{
    instanceId: string;
    gatewayUrl: string;
    gatewayPort: number;
  }>(`/v1/installed-applications/${installedAppId}/openclaw/gateway-info`);
  return response.data;
},

selfApproveOpenClawDevice: async (
  installedAppId: string,
  requestId: string,
): Promise<void> => {
  await http.post(
    `/v1/installed-applications/${installedAppId}/openclaw/devices/self-approve`,
    { requestId },
  );
},
```

**Commit:**
```bash
git commit -m "feat(client): add getOpenClawGatewayInfo and selfApproveOpenClawDevice API"
```

---

## Phase 2：Team9 Tauri 客户端 — 自动管理 aHand daemon

### Task 2.1：Tauri Cargo.toml 添加依赖

**Files:**
- Modify: `team9/apps/client/src-tauri/Cargo.toml`

只需追加 `dirs`（Tauri 2 已内置 tokio，不要重复添加）：

```toml
[dependencies]
tauri = { version = "2", features = [] }
tauri-plugin-opener = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
dirs = "5"
```

**Step 1:** 验证依赖可以解析：
```bash
cd team9/apps/client/src-tauri && cargo fetch 2>&1 | tail -5
```

---

### Task 2.2：创建 ahand.rs

**Files:**
- Create: `team9/apps/client/src-tauri/src/ahand.rs`

```rust
//! Manages the local aHand daemon lifecycle.
//!
//! Workflow:
//! 1. Frontend calls `ahand_start` with the OpenClaw gateway URL and node_id.
//! 2. We write ~/.ahand/config.toml (openclaw-gateway mode).
//! 3. We spawn ahandd in the background.
//! 4. On logout / app exit, we kill the process.
//!
//! Node ID persistence:
//! A stable device ID is stored in ~/.ahand/team9-device-id so the same
//! node_id is reused across restarts. If the device was previously approved
//! by the OpenClaw gateway, it will reconnect without needing re-approval.

use std::process::{Child, Command};
use std::sync::Mutex;

static DAEMON: Mutex<Option<Child>> = Mutex::new(None);

/// Locate the aHand binary: ~/.ahand/bin/ahandd first, then PATH.
fn find_binary() -> Option<std::path::PathBuf> {
    if let Some(home) = dirs::home_dir() {
        let p = home.join(".ahand").join("bin").join("ahandd");
        if p.exists() {
            return Some(p);
        }
    }
    // Fallback: check PATH
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            let p = dir.join("ahandd");
            p.exists().then_some(p)
        })
    })
}

/// Get or create a stable device ID stored in ~/.ahand/team9-device-id.
/// Reusing the same node_id allows the gateway to recognize a previously
/// approved device without requiring re-approval.
pub fn get_or_create_node_id() -> String {
    let path = dirs::home_dir()
        .expect("no home dir")
        .join(".ahand")
        .join("team9-device-id");

    if let Ok(id) = std::fs::read_to_string(&path) {
        let trimmed = id.trim().to_string();
        if !trimmed.is_empty() {
            return trimmed;
        }
    }

    // Generate a stable ID from current time (good enough for a device identifier).
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let id = format!("team9-{secs:016x}{nanos:08x}");

    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let _ = std::fs::write(&path, &id);
    id
}

/// Write ~/.ahand/config.toml for openclaw-gateway mode.
///
/// IMPORTANT: auth_token belongs under [openclaw] section, NOT at the top level.
fn write_config(
    gateway_url: &str,
    auth_token: Option<&str>,
    node_id: &str,
) -> std::io::Result<()> {
    let path = dirs::home_dir()
        .expect("no home dir")
        .join(".ahand")
        .join("config.toml");

    std::fs::create_dir_all(path.parent().unwrap())?;

    let (host, port, tls) = parse_gateway_url(gateway_url);

    // Build optional auth_token line — placed inside [openclaw] section.
    let auth_line = auth_token
        .map(|t| format!("auth_token = \"{t}\"\n"))
        .unwrap_or_default();

    let content = format!(
        r#"# Auto-generated by Team9 desktop app. Do not edit manually.
mode = "openclaw-gateway"

[openclaw]
gateway_host = "{host}"
gateway_port = {port}
gateway_tls = {tls}
node_id = "{node_id}"
{auth_line}"#
    );

    std::fs::write(&path, content)?;
    Ok(())
}

fn parse_gateway_url(url: &str) -> (String, u16, bool) {
    let tls = url.starts_with("wss://");
    let without_scheme = url
        .trim_start_matches("wss://")
        .trim_start_matches("ws://");

    if let Some((host, port_str)) = without_scheme.rsplit_once(':') {
        let port = port_str.parse().unwrap_or(18789);
        return (host.to_string(), port, tls);
    }

    (without_scheme.to_string(), 18789, tls)
}

/// Start aHand daemon in openclaw-gateway mode.
/// Kills any previously managed instance first.
pub fn start(gateway_url: &str, auth_token: Option<&str>, node_id: &str) -> Result<(), String> {
    stop(); // kill any previous managed instance

    let binary = find_binary()
        .ok_or_else(|| "aHand is not installed. Please install it first.".to_string())?;

    write_config(gateway_url, auth_token, node_id).map_err(|e| e.to_string())?;

    let config_path = dirs::home_dir()
        .unwrap()
        .join(".ahand")
        .join("config.toml");

    let child = Command::new(&binary)
        .arg("--config")
        .arg(&config_path)
        .spawn()
        .map_err(|e| format!("failed to spawn ahandd: {e}"))?;

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

/// Is the daemon process currently alive?
/// Note: true means the process exists, not necessarily that it is connected
/// to the gateway. Check device status via Team9 API for actual connectivity.
pub fn is_running() -> bool {
    DAEMON.lock().unwrap().is_some()
}
```

---

### Task 2.3：注册 Tauri 命令

**Files:**
- Modify: `team9/apps/client/src-tauri/src/lib.rs`

替换整个文件内容：

```rust
mod ahand;

/// Start aHand daemon in openclaw-gateway mode.
/// Called by the React frontend after obtaining the gateway URL from Team9 API.
#[tauri::command]
fn ahand_start(
    gateway_url: String,
    auth_token: Option<String>,
    node_id: String,
) -> Result<(), String> {
    ahand::start(&gateway_url, auth_token.as_deref(), &node_id)
}

#[tauri::command]
fn ahand_stop() {
    ahand::stop();
}

/// Returns true if the daemon process is alive.
/// Does NOT guarantee gateway connectivity — use Team9 API for that.
#[tauri::command]
fn ahand_is_running() -> bool {
    ahand::is_running()
}

/// Returns the stable device ID for this machine.
/// Used by the frontend to identify our pending pairing request.
#[tauri::command]
fn ahand_get_node_id() -> String {
    ahand::get_or_create_node_id()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            ahand_start,
            ahand_stop,
            ahand_is_running,
            ahand_get_node_id,
        ])
        .on_window_event(|_win, event| {
            if let tauri::WindowEvent::Destroyed = event {
                ahand::stop();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

**Step 1:** 编译验证：
```bash
cd team9/apps/client && pnpm tauri build --debug 2>&1 | tail -10
```
Expected: 无编译错误，生成 debug 二进制。

**Step 2:** Commit
```bash
git add apps/client/src-tauri/
git commit -m "feat(tauri): auto-start aHand daemon in openclaw-gateway mode on login"
```

---

### Task 2.4：React 侧在登录成功后启动 aHand

实现分两部分：①创建 hook；②在 authenticated layout 和 logout 中调用。

#### Part A：创建 useAHandAutoConnect hook

**Files:**
- Create: `team9/apps/client/src/hooks/useAHandAutoConnect.ts`

```typescript
import { useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { applicationsApi } from '../services/api/applications.js';

/**
 * Automatically starts the aHand daemon after the user logs into the desktop app.
 *
 * Flow:
 * 1. Find the installed OpenClaw app in the current workspace.
 * 2. Fetch the OpenClaw gateway URL from Team9 API.
 * 3. Start aHand daemon with the gateway URL and a stable node_id.
 * 4. Poll for a pending device pairing request (up to 30s) and auto-approve it.
 *    If no pending request is found, the device was likely already approved
 *    from a previous session — this is normal and not an error.
 *
 * This hook is a no-op when not running inside the Tauri desktop app.
 */
export function useAHandAutoConnect() {
  const started = useRef(false);

  useEffect(() => {
    // Only run once per session and only in Tauri desktop app.
    if (started.current || !(window as any).__TAURI__) return;
    started.current = true;

    void startLocalDevice();

    return () => {
      // Stop daemon when the authenticated layout unmounts (user logs out).
      invoke('ahand_stop').catch(() => {});
    };
  }, []);
}

async function findOpenClawAppId(): Promise<string | null> {
  const apps = await applicationsApi.getInstalledApplications();
  const openclawApp = apps.find(
    (app) => app.applicationId === 'openclaw' && app.isActive,
  );
  return openclawApp?.id ?? null;
}

async function startLocalDevice(): Promise<void> {
  try {
    // 1. Find the OpenClaw installed app in this workspace.
    const installedAppId = await findOpenClawAppId();
    if (!installedAppId) {
      console.info('[aHand] No active OpenClaw app found in this workspace — skipping');
      return;
    }

    // 2. Get gateway URL from Team9 API.
    const info = await applicationsApi.getOpenClawGatewayInfo(installedAppId);

    // 3. Get the stable node ID for this device (persisted across restarts).
    const nodeId = await invoke<string>('ahand_get_node_id');

    // 4. Start the daemon.
    await invoke('ahand_start', {
      gatewayUrl: info.gatewayUrl,
      authToken: null,
      nodeId,
    });
    console.info('[aHand] daemon started, connecting to', info.gatewayUrl);

    // 5. Poll for a pending pairing request (up to 30s) and auto-approve.
    //    If the device was already approved in a previous session, no pending
    //    request will appear — that is expected and not an error.
    let approved = false;
    for (let i = 0; i < 15; i++) {
      await new Promise<void>((r) => setTimeout(r, 2000));
      const devices = await applicationsApi.getOpenClawDevices(installedAppId);
      const pending = devices.find((d) => d.status === 'pending');
      if (pending) {
        await applicationsApi.selfApproveOpenClawDevice(
          installedAppId,
          pending.request_id,
        );
        console.info('[aHand] device pairing auto-approved:', pending.request_id);
        approved = true;
        break;
      }
    }

    if (!approved) {
      // No pending request within 30s — device is likely already paired.
      console.info('[aHand] No pending pairing request found — device may already be paired');
    }
  } catch (err) {
    // Non-fatal — app works without local device.
    console.warn('[aHand] failed to start local device:', err);
  }
}
```

#### Part B：在 authenticated layout 中调用 hook

**Files:**
- Modify: `team9/apps/client/src/routes/_authenticated.tsx`

找到 authenticated layout 的组件函数（包含 `GlobalTopBar`, `MainSidebar` 等的那个组件），在其顶部加入 hook 调用：

```typescript
import { useAHandAutoConnect } from '../hooks/useAHandAutoConnect.js';

// 在 authenticated layout 组件内部（函数顶部）：
export function AuthenticatedLayout() {
  useAHandAutoConnect(); // auto-start aHand daemon on login

  // ... 其余现有代码不变 ...
}
```

#### Part C：在 logout 时停止 daemon

**Files:**
- Modify: `team9/apps/client/src/hooks/useAuth.ts`

找到 `useLogout` 的 `useMutation` 定义，在 `onSuccess` 回调中追加 daemon 停止逻辑：

```typescript
import { invoke } from '@tauri-apps/api/core';

// 在 useLogout 的 onSuccess 中追加（现有清理代码之后）：
onSuccess: () => {
  // ... 现有的 store 清理代码 ...

  // Stop aHand daemon on logout (desktop app only).
  if ((window as any).__TAURI__) {
    invoke('ahand_stop').catch(() => {});
  }
},
```

**Commit:**
```bash
git add apps/client/src/hooks/useAHandAutoConnect.ts \
        apps/client/src/routes/_authenticated.tsx \
        apps/client/src/hooks/useAuth.ts
git commit -m "feat(client): auto-start and pair aHand daemon after login"
```

---

## Phase 3：Team9 React 客户端 — 本地设备状态 UI

### Task 3.1：useAHandStatus hook

**Files:**
- Create: `team9/apps/client/src/hooks/useAHandStatus.ts`

```typescript
import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';

export type AHandStatus = 'connected' | 'disconnected' | 'not-desktop';

/**
 * Polls whether the aHand daemon process is alive.
 *
 * LIMITATION: 'connected' means the daemon process exists, not that it has
 * successfully paired with the OpenClaw gateway. For precise connectivity
 * status, query the Team9 devices API and look for an 'approved' device.
 */
export function useAHandStatus(): AHandStatus {
  const [status, setStatus] = useState<AHandStatus>('not-desktop');

  useEffect(() => {
    if (!(window as any).__TAURI__) return;

    const check = async () => {
      try {
        const running = await invoke<boolean>('ahand_is_running');
        setStatus(running ? 'connected' : 'disconnected');
      } catch {
        setStatus('disconnected');
      }
    };

    void check();
    const id = setInterval(check, 5000);
    return () => clearInterval(id);
  }, []);

  return status;
}
```

### Task 3.2：LocalDeviceStatus 组件

**Files:**
- Create: `team9/apps/client/src/components/layout/LocalDeviceStatus.tsx`

```tsx
import { useAHandStatus } from '../../hooks/useAHandStatus.js';

export function LocalDeviceStatus() {
  const status = useAHandStatus();
  if (status === 'not-desktop') return null;

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

在侧边栏底部引入：
```tsx
import { LocalDeviceStatus } from '../layout/LocalDeviceStatus.js';
// In sidebar bottom: <LocalDeviceStatus />
```

**Commit:**
```bash
git commit -m "feat(client): add local device connection status indicator"
```

---

## Phase 4：OpenClaw Team9 Extension — senderId 验证

### Task 4.1：验证 senderId 是否已实现

在执行任何代码改动之前，先验证当前状态：

**Step 1:** 检查 `prepare.ts` 中 ctxPayload 的构建，确认 `SenderId`/`SenderName` 是否已传入：
```bash
grep -n "SenderId\|SenderName\|senderId\|senderName" \
  openclaw-hive/openclaw/extensions/team9/src/monitor/prepare.ts
```

**Step 2:** 如果上面的 grep 输出包含 `SenderId: message.senderId` 和 `SenderName: message.senderName`，则**本 Phase 已完成**，无需任何改动。

**Step 3（仅在未实现时执行）:** 在 `ctxPayload` 构建块中加入：

```typescript
// In the ctxPayload construction block, add:
SenderId: message.senderId,
SenderName: message.senderName ?? message.senderId,
```

**Step 4（如有改动）:** 编译验证并 commit：
```bash
cd openclaw-hive && npm run build 2>&1 | tail -10
git commit -m "feat(team9-ext): pass senderId/senderName in message context for AI prompts"
```

---

## 端到端验证流程

**Step 1:** 在 Team9 工作区安装 OpenClaw 应用（已有功能）

**Step 2:** 在本地安装 aHand：
```bash
curl -fsSL https://ahand.sh/install.sh | bash
# 验证安装：
~/.ahand/bin/ahandd --version
```

**Step 3:** 打开 Team9 桌面客户端，登录

**Step 4:** 观察日志（桌面客户端开发者控制台）：
```
[aHand] daemon started, connecting to wss://xxx.openclaw.cloud:18789
[aHand] device pairing auto-approved: req_xxx
# 或（已配对场景）：
[aHand] No pending pairing request found — device may already be paired
```

**Step 5:** 在 Team9 频道向 OpenClaw Bot 发送：
```
@bot 帮我看一下当前目录有哪些文件
```

**Step 6:** 观察频道，Bot 应回复 `ls` 命令的输出。

**Step 7（异常验证）:** 关闭 aHand 并重新登录，确认重新配对不报错。

---

## 关键配置项汇总

| 配置 | 位置 | 值 |
|------|------|----|
| aHand 连接模式 | `~/.ahand/config.toml` | `mode = "openclaw-gateway"` |
| Gateway 地址 | `~/.ahand/config.toml` `[openclaw]` section | 从 Team9 API 自动获取 |
| node_id | `~/.ahand/team9-device-id` + config.toml `[openclaw]` | Tauri 生成并持久化 |
| OpenClaw 工具执行 | openclaw-hive（不改） | `ws://127.0.0.1:18789`（容器内部） |
| 设备审批 | Team9 API | Tauri 自动调用 self-approve |

---

## 安全注意事项

1. **`self-approve` 端点**：调用 `listDevices` 验证 `requestId` 确实处于 pending 状态，防止审批已处理的或无效的请求。当前实现验证了租户归属（通过 `getVerifiedApp`）和 requestId 有效性，但无法验证该设备确实属于调用用户本人（设备配对请求中不含 userId）——这在 v1 可以接受。

2. **exec 权限**：aHand 默认 `auto_accept`，意味着该实例的任何调用都可以执行。生产环境可通过 `exec-approvals.json` 限制允许的命令列表。

3. **Gateway TLS**：生产环境必须使用 `wss://`（Traefik 已配置 TLS），防止中间人攻击。

4. **node_id 安全性**：`~/.ahand/team9-device-id` 存储明文 ID，不含任何密钥。即使泄露也只能用于识别设备标识，不影响安全性。
