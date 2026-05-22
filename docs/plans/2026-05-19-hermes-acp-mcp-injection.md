# Hermes ACP MCP Injection Plan

**Goal:** 在 AHand 的 Hermes ACP runner 中支持 MCP 注入。调用方仍传统一 `mcpConfig`，AHand 将 Claude 风格 `mcpServers` object 转成 Hermes ACP `session/new.params.mcpServers` array，并通过现有 `pipe_stream` stdin/stdout JSON-RPC 会话发送给 Hermes。该能力使用 `executionMode=pipe_stream`、`inputFormat=hermes-acp-json-rpc`、`outputFormat=hermes-acp-json-rpc`，不新增第四个“运行后端选择”开关。

## 参考

- `docs/HERMES_MCP_INJECTION.md`
- `docs/HERMES_DATA_EXCHANGE.md`
- `docs/HERMES_INTEGRATION.md`
- `docs/agent-stdio-formats.md`
- `docs/status/hermes.md`

## 当前状态

- AHand 已有 Hermes ACP 专用 runner：`crates/ahandd/src/agent/hermes_acp.rs`。
- 当前 `session/new` 参数固定为：

```json
{
  "cwd": "...",
  "mcpServers": []
}
```

- `session/resume` 当前只发送 `cwd` 和 `sessionId`。
- Hermes ACP formatter 已经负责把 stdout JSON-RPC、content、diff、provider error、permission/policy 行为归一成 observation record。

## 公共输入契约

MCP config JSON 本体和 Claude 一致：

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
      "env": {
        "TOKEN": "secret"
      }
    }
  }
}
```

`--mcp-config-file` 读取到的 JSON 就是这个 MCP config 本体，不包含 `mcpConfig` 或 `mcpConfigMode` 外壳。Control plane / SDK 里的 `mcpConfig` 字段值也是这个 MCP config 本体。

短期 env bridge：

```text
AHAND_AGENT_MCP_CONFIG=<json string>
AHAND_AGENT_MCP_CONFIG_MODE=replace
```

Hermes runner 不应该从全局 Hermes 配置隐式探测 MCP。AHand 在 ACP 模式下明确指定 Hermes path/env，然后启动或确认进程活着，之后直接发送 prompt 并收取结果；MCP 注入是这次 `session/new` 的参数之一。

`mcpConfigMode` 是独立策略开关，不写进 `mcpConfig` JSON：

| Value | Meaning |
|---|---|
| absent | 默认。传了 `mcpConfig` 时把 base Hermes MCP servers 和 job `mcpConfig` 合并，不同名追加，同名由 job 覆盖；不传 `mcpConfig` 时不做 AHand 级 MCP 注入。 |
| `replace` | `session/new.mcpServers` 只使用 job `mcpConfig` 转换结果。 |

不提供 `disabled` 模式。需要禁用 AHand 级 MCP 注入时不要传 `mcpConfig`。如果需要强制清空可继承的 base MCP servers，则传 MCP config 本体 `{ "mcpServers": {} }`，并在外层设置 `mcpConfigMode=replace`。

强制清空时，MCP config JSON 文件内容仍然只是：

```json
{
  "mcpServers": {}
}
```

策略通过外层 flag 表达：

```bash
ahandctl hermes /path/to/hermes \
  --cwd "$PWD" \
  --prompt "Inspect this repo" \
  --mcp-config-file ./empty-mcp.json \
  --mcp-config-mode replace
```

## Hermes ACP 转换

输入：

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/repo"],
      "env": { "TOKEN": "secret" }
    }
  }
}
```

目标候选输出：

```json
{
  "mcpServers": [
    {
      "name": "filesystem",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/repo"],
      "env": { "TOKEN": "secret" }
    }
  ]
}
```

实现前必须用 Hermes 当前 ACP schema 或 fake Hermes fixture 确认 item 字段。如果 Hermes 要求字段名不同，转换器只改 Hermes adapter，不改变 AHand 公共 `mcpConfig` 输入。

排序规则：

- 为了可测和可复现，按 server name 字典序输出 `mcpServers` array。
- 未传 MCP 时输出 `mcpServers: []`，保持现有行为。
- `env` 值只进入 Hermes ACP request，不进入 observation/audit 明文。

## 调用方式

CLI debug path:

```bash
ahandctl hermes /path/to/hermes \
  --cwd "$PWD" \
  --prompt "Inspect this repo" \
  --mcp-config-file ./mcp.json
```

Control plane:

```json
{
  "deviceId": "dev-1",
  "executionMode": "pipe_stream",
  "inputFormat": "hermes-acp-json-rpc",
  "outputFormat": "hermes-acp-json-rpc",
  "executable": "/path/to/hermes",
  "prompt": "Inspect this repo",
  "mcpConfig": {
    "mcpServers": {
      "filesystem": {
        "command": "npx",
        "args": ["-y", "@modelcontextprotocol/server-filesystem", "/repo"]
      }
    }
  }
}
```

SDK:

```ts
await client.spawnAgent({
  deviceId,
  executable: "/path/to/hermes",
  inputFormat: "hermes-acp-json-rpc",
  outputFormat: "hermes-acp-json-rpc",
  prompt: "Inspect this repo",
  mcpConfig: {
    mcpServers: {
      filesystem: {
        command: "npx",
        args: ["-y", "@modelcontextprotocol/server-filesystem", "/repo"],
      },
    },
  },
});
```

## 实现阶段

### Phase 1: API and Env Bridge

- 和 Claude plan 共用 `mcpConfig` 字段，不为 Hermes 增加独立字段名。
- 和 Claude plan 共用独立 `mcpConfigMode` 策略字段，不写进 `mcpConfig`。
- Hub 校验后将 JSON 序列化到 `AHAND_AGENT_MCP_CONFIG`。
- Hub 校验 `mcpConfigMode` 后写入 `AHAND_AGENT_MCP_CONFIG_MODE`，缺省为空，表示默认 merge。
- `ahandctl hermes` 增加 `--mcp-config <json>`、`--mcp-config-file <path>` 和 `--mcp-config-mode replace`。
- `--mcp-config` / `--mcp-config-file` 的 JSON payload 是 MCP config 本体，不是 control-plane request body。
- SDK `spawnAgent` 透传 `mcpConfig` 和 `mcpConfigMode`。

### Phase 2: Hermes Config Loading

- `HermesAcpConfig` 增加 `mcp_config`。
- `HermesAcpConfig` 增加 `mcp_config_mode`，空值表示默认 merge。
- 从 `AHAND_AGENT_MCP_CONFIG` 读取并校验 JSON。
- 从 `AHAND_AGENT_MCP_CONFIG_MODE` 读取策略开关。
- malformed config 失败时输出 normalized error record，不泄漏原文。
- 用户 env 中的 `AHAND_AGENT_MCP_CONFIG` 和 `AHAND_AGENT_MCP_CONFIG_MODE` 不能透传到 Hermes 子进程。

### Phase 3: MCP Converter

- 新增独立 converter，例如 `mcp_config.rs` 或 Hermes runner 内部 helper：

```rust
fn hermes_mcp_servers(config: &serde_json::Value) -> Result<Vec<serde_json::Value>, String>
```

- converter 只接受 AHand 公共 schema。
- converter 输出 Hermes ACP `mcpServers` array。
- 对缺失 `mcpServers` 输出空数组。
- 对 server schema 错误返回可读错误，不包含 secret。
- 默认模式先加载 base Hermes MCP servers，再和 job servers 合并；同名由 job 覆盖。
- `replace` 模式只转换 job servers。
- 未传 `mcpConfig` 时不做 AHand 级 MCP 注入。

### Phase 4: session/new Injection

- 将当前固定空数组替换为转换后的数组：

```json
{
  "cwd": "...",
  "mcpServers": convertedServers,
  "model": "..."
}
```

- `session/resume` 的行为需要显式定义：
  - 第一阶段不允许 resume 时同时传新的 `mcpConfig`，因为 session 的 MCP server 集合通常属于 session creation state。
  - 如果用户传了 `sessionId` 和非空 `mcpConfig`，AHand 返回明确错误：`mcpConfig is only supported for new Hermes sessions`。
  - 后续如果 Hermes ACP 支持 resume 后更新 MCP，再单独扩展。

### Phase 5: Observation, Raw, and Redaction

- Hermes raw ACP stdout 继续写入 artifact。
- AHand 发出的 ACP request 如果写入 debug artifact，需要对 `mcpServers[].env` 做 redaction，或只写 metadata。
- 输出 audit/observation：

```json
{
  "kind": "audit",
  "action": "mcp_config_injected",
  "agent": "hermes-acp",
  "serverNames": ["filesystem"],
  "serverCount": 1,
  "target": "session/new.mcpServers"
}
```

- permission/policy 行为继续按 `docs/HERMES_DATA_EXCHANGE.md` 输出 observation/audit record；MCP 注入 audit record 使用同一个 observation JSONL 通道。

### Phase 6: Tests

- Converter unit tests：empty、valid single server、valid multiple servers sorted、bad command、bad args、bad env。
- Converter unit tests：默认模式追加/覆盖、`replace` 不继承 base、未传 `mcpConfig` 不注入。
- Runner test：无 config 时 `session/new.mcpServers` 仍为 `[]`。
- Runner test：有 config 时 fake Hermes 收到转换后的 `session/new.mcpServers`。
- Runner test：`sessionId + mcpConfig` 返回明确错误。
- Redaction test：`TOKEN=secret` 不出现在 observations、stderr、普通 log、debug metadata。
- Smoke script：指定目录和 prompt，运行 fake Hermes，确认 outputFormat 能转换成目标 observation JSONL。

## 接受标准

- Hermes 新 session 可以收到 AHand 注入的 MCP servers。
- `session/new.mcpServers` 的字段优先级和 `docs/HERMES_DATA_EXCHANGE.md` 的 formatter 规则不冲突。
- `outputFormat=hermes-acp-json-rpc` 仍是唯一 caller-facing normalized stdout。
- `inputFormat=raw` / `outputFormat=raw` 不受影响。
- 不新增第四个“运行后端选择”开关。
