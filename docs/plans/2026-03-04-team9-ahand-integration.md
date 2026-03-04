# Team9 × aHand 集成实施计划（修订版 v2）

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 让用户登录 Team9 桌面客户端后，无需任何手动配置，即可通过 Team9 频道与 OpenClaw AI 对话，AI 能远程控制和调用用户本地电脑上的工具。

**Architecture:** aHand 以现有的 `openclaw-gateway` 模式直连 OpenClaw 实例的 Gateway（Traefik 已将端口 18789 公开暴露），Tauri 客户端在用户登录后自动从 Team9 API 获取 gateway URL、写入 aHand 配置并启动守护进程。设备配对通过 Team9 API 自动审批，无需人工操作。OpenClaw 工具执行（`system.run` → `node.invoke.request`）完全不需要改动。

**Tech Stack:** Rust (tokio) · NestJS · TypeScript · Tauri 2 · React

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
| `team9/` | 中 — 新增 2 个 API 端点 |
| `team9/apps/client/` | 中 — Tauri 自动启动逻辑 |
| `openclaw-hive/openclaw/extensions/team9/` | 小 — 可选：传 senderId 给 AI |
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
 * Auth: any authenticated workspace member.
 */
@Get(':id/openclaw/gateway-info')
async getOpenClawGatewayInfo(
  @Param('id') id: string,
  @CurrentUser('sub') userId: string,
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

现有已有 `approveOpenClawDevice` 端点（需要管理员权限），将权限放宽为**本人可以自动审批自己的设备**：

找到现有的 `POST :id/openclaw/devices/approve`，在其上方新增一个允许普通用户自审批的端点：

```typescript
/**
 * Self-approve a pending device pairing for the calling user's own device.
 * Used by Tauri to auto-approve without requiring admin intervention.
 */
@Post(':id/openclaw/devices/self-approve')
async selfApproveOpenClawDevice(
  @Param('id') id: string,
  @CurrentUser('sub') userId: string,
  @CurrentTenantId() tenantId: string,
  @Body('requestId') requestId: string,
) {
  const app = await this.getVerifiedApp(id, tenantId, 'openclaw');
  const instancesId = (app.config as { instancesId?: string })?.instancesId;
  if (!instancesId) throw new NotFoundException('No instance configured');

  await this.openclawService.approveDevice(instancesId, requestId);
  return { approved: true, requestId };
}
```

**Step 1:** 编译验证：
```bash
cd team9/apps/server && pnpm build --filter @team9/gateway 2>&1 | tail -5
```

**Step 2:** Commit
```bash
git commit -m "feat(team9-server): add self-approve endpoint for aHand device pairing"
```

---

### Task 1.3：前端新增 API 方法

**Files:**
- Modify: `team9/apps/client/src/services/api/applications.ts`

```typescript
export const getOpenClawGatewayInfo = (appId: string) =>
  apiClient.get<{ instanceId: string; gatewayUrl: string; gatewayPort: number }>(
    `/v1/installed-applications/${appId}/openclaw/gateway-info`,
  );

export const selfApproveOpenClawDevice = (appId: string, requestId: string) =>
  apiClient.post(`/v1/installed-applications/${appId}/openclaw/devices/self-approve`, {
    requestId,
  });
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

```toml
[dependencies]
tauri = { version = "2", features = [] }
tauri-plugin-opener = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }
dirs = "5"
```

---

### Task 2.2：创建 ahand.rs

**Files:**
- Create: `team9/apps/client/src-tauri/src/ahand.rs`

```rust
//! Manages the local aHand daemon lifecycle.
//!
//! Workflow:
//! 1. Frontend calls `ahand_start` with the OpenClaw gateway URL.
//! 2. We write ~/.ahand/config.toml (openclaw-gateway mode).
//! 3. We spawn ahandd in the background.
//! 4. On logout / app exit, we kill the process.

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

/// Write ~/.ahand/config.toml for openclaw-gateway mode.
fn write_config(gateway_url: &str, auth_token: Option<&str>) -> std::io::Result<()> {
    let path = dirs::home_dir()
        .expect("no home dir")
        .join(".ahand")
        .join("config.toml");

    std::fs::create_dir_all(path.parent().unwrap())?;

    // auth_token is only written if the gateway requires it.
    let auth_line = auth_token
        .map(|t| format!("\nauth_token = \"{t}\""))
        .unwrap_or_default();

    // Parse host/port from the gateway URL.
    // e.g. wss://instance-id.openclaw.cloud:18789
    let (host, port, tls) = parse_gateway_url(gateway_url);

    let content = format!(
        r#"# Auto-generated by Team9 desktop app. Do not edit manually.
mode = "openclaw-gateway"
{auth_line}

[openclaw]
gateway_host = "{host}"
gateway_port = {port}
gateway_tls = {tls}
"#
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
pub fn start(gateway_url: &str, auth_token: Option<&str>) -> Result<(), String> {
    stop(); // kill any previous managed instance

    let binary = find_binary()
        .ok_or_else(|| "aHand is not installed. Please install it first.".to_string())?;

    write_config(gateway_url, auth_token).map_err(|e| e.to_string())?;

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

/// Is the daemon currently running?
pub fn is_running() -> bool {
    DAEMON.lock().unwrap().is_some()
}
```

---

### Task 2.3：注册 Tauri 命令

**Files:**
- Modify: `team9/apps/client/src-tauri/src/lib.rs`

```rust
mod ahand;

/// Start aHand daemon in openclaw-gateway mode.
/// Called by the React frontend after obtaining the gateway URL from Team9 API.
#[tauri::command]
fn ahand_start(gateway_url: String, auth_token: Option<String>) -> Result<(), String> {
    ahand::start(&gateway_url, auth_token.as_deref())
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

**Step 2:** Commit
```bash
git add apps/client/src-tauri/
git commit -m "feat(tauri): auto-start aHand daemon in openclaw-gateway mode on login"
```

---

### Task 2.4：React 侧在登录成功后启动 aHand

找到处理登录成功的 auth store 或 hook（搜索 `accessToken` 赋值处），加入以下逻辑：

**Files:**
- 修改 auth store 的登录成功回调（如 `apps/client/src/stores/auth.store.ts`）

```typescript
import { invoke } from '@tauri-apps/api/core';
import { getOpenClawGatewayInfo, selfApproveOpenClawDevice, getOpenClawDevices }
  from '../services/api/applications.js';

/**
 * After successful login, if running in Tauri desktop app:
 * 1. Fetch the user's OpenClaw gateway URL from Team9 API.
 * 2. Start aHand daemon pointing at that gateway.
 * 3. Poll for pending device pairing requests and auto-approve.
 */
async function startLocalDevice(installedAppId: string) {
  if (!(window as any).__TAURI__) return;

  try {
    // 1. Get gateway URL
    const info = await getOpenClawGatewayInfo(installedAppId);

    // 2. Start daemon
    await invoke('ahand_start', {
      gatewayUrl: info.gatewayUrl,
      authToken: null,
    });
    console.info('[aHand] daemon started, connecting to', info.gatewayUrl);

    // 3. Poll for pending pairing request and self-approve (up to 30s)
    for (let i = 0; i < 15; i++) {
      await new Promise((r) => setTimeout(r, 2000));
      const devices = await getOpenClawDevices(installedAppId);
      const pending = devices.find((d: any) => d.status === 'pending');
      if (pending) {
        await selfApproveOpenClawDevice(installedAppId, pending.request_id);
        console.info('[aHand] device pairing auto-approved:', pending.request_id);
        break;
      }
    }
  } catch (err) {
    // Non-fatal — app works without local device.
    console.warn('[aHand] failed to start local device:', err);
  }
}

// On logout:
async function stopLocalDevice() {
  if ((window as any).__TAURI__) {
    await invoke('ahand_stop').catch(() => {});
  }
}
```

> 查找 installedAppId：用户工作区安装了 OpenClaw 应用后，前端应有对应的 appId，从应用列表 API 获取即可（搜索 `getInstalledApplications` 相关调用）。

**Commit:**
```bash
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
      <span className={`h-2 w-2 rounded-full ${
        status === 'connected' ? 'bg-green-500' : 'bg-yellow-500 animate-pulse'
      }`} />
      {status === 'connected' ? '本地已连接' : '本地未连接'}
    </div>
  );
}
```

在侧边栏底部引入：
```tsx
import { LocalDeviceStatus } from '../layout/LocalDeviceStatus.js';
// In sidebar: <LocalDeviceStatus />
```

**Commit:**
```bash
git commit -m "feat(client): add local device connection status indicator"
```

---

## Phase 4：OpenClaw Team9 Extension — 传入 senderId（可选但推荐）

OpenClaw 的工具执行本身**不需要改动**。但为了让 AI 知道是哪个用户在说话（并在 system prompt 里包含上下文），建议传入 `senderId`。

### Task 4.1：在 ctxPayload 中注入 senderId

**Files:**
- Modify: `openclaw-hive/openclaw/extensions/team9/src/monitor/prepare.ts`

在 `prepareTeam9Message` 构建 `ctxPayload` 的地方，加入 `senderId` 和 `senderName`：

```typescript
// In the ctxPayload construction block, add:
const ctxPayload = {
  // ... existing fields ...
  SenderId: message.senderId,        // Team9 user ID who sent the message
  SenderName: message.senderName ?? message.senderId,
};
```

这样 AI 的 system prompt 模板里可以用 `{{SenderId}}` 和 `{{SenderName}}` 引用发消息的用户。

**Step 1:** 找到 ctxPayload 构建位置：
```bash
grep -n "ctxPayload\|CommandAuthorized\|FinalizedMsgContext" \
  openclaw-hive/openclaw/extensions/team9/src/monitor/prepare.ts
```

**Step 2:** 加入字段，编译验证：
```bash
cd openclaw-hive && npm run build 2>&1 | tail -10
```

**Step 3:** Commit
```bash
git commit -m "feat(team9-ext): pass senderId/senderName in message context for AI prompts"
```

---

## 端到端验证流程

**Step 1:** 在 Team9 工作区安装 OpenClaw 应用（已有功能）

**Step 2:** 在本地安装 aHand：
```bash
curl -fsSL https://github.com/team9ai/aHand/releases/latest/download/install.sh | bash
```

**Step 3:** 打开 Team9 桌面客户端，登录

**Step 4:** 观察日志（桌面客户端控制台）：
```
[aHand] daemon started, connecting to wss://xxx.openclaw.cloud:18789
[aHand] device pairing auto-approved: req_xxx
```

**Step 5:** 在 Team9 频道向 OpenClaw Bot 发送：
```
@bot 帮我看一下当前目录有哪些文件
```

**Step 6:** 观察频道，Bot 应回复 `ls` 命令的输出。

---

## 关键配置项汇总

| 配置 | 位置 | 值 |
|------|------|----|
| aHand 连接模式 | `~/.ahand/config.toml` | `mode = "openclaw-gateway"` |
| Gateway 地址 | `~/.ahand/config.toml` | 从 Team9 API 自动获取 |
| OpenClaw 工具执行 | openclaw-hive（不改） | `ws://127.0.0.1:18789`（容器内部） |
| 设备审批 | Team9 API | Tauri 自动调用 self-approve |

---

## 安全注意事项

1. **`self-approve` 端点**：只允许审批与该用户的 OpenClaw 实例配对的设备，不允许审批其他用户的。需在后端验证实例归属。
2. **exec 权限**：aHand 默认 `auto_accept`，意味着该实例的任何调用都可以执行。生产环境可通过 `exec-approvals.json` 限制允许的命令列表。
3. **Gateway TLS**：生产环境必须使用 `wss://`（Traefik 已配置 TLS），防止中间人攻击。
