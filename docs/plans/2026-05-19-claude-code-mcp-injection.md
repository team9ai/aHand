# Claude Code MCP Injection Plan

**Goal:** 在 AHand 的 Claude Code runner 中支持任务级 MCP 注入。调用方通过统一的 `mcpConfig` 传入 Claude 风格 MCP JSON，AHand 在本次 job 启动前写入受控临时文件，并以 `claude --mcp-config <file> --strict-mcp-config` 启动 Claude Code。该能力只依赖现有三开关模型：`executionMode=pipe_stream`、`inputFormat=claude-stream-json`、`outputFormat=claude-stream-json`。

## 参考

- `docs/CLAUDE_CODE_MCP_INJECTION.md`
- `docs/CLAUDE_CODE_DATA_EXCHANGE.md`
- `docs/agent-stdio-formats.md`
- `docs/status/claude-code.md`

## 当前状态

- AHand 已有 Claude Code 专用入口：`inputFormat=claude-stream-json` 和 `outputFormat=claude-stream-json`。
- `crates/ahandd/src/agent/claude_code.rs` 已经固定传入 `--strict-mcp-config`，但当前没有 `--mcp-config <path>`。
- Hub control plane 当前通过 env bridge 传递 `AHAND_AGENT_EXECUTABLE`、`AHAND_AGENT_PROMPT`、`AHAND_AGENT_MODEL`、`AHAND_AGENT_PERMISSION_MODE` 等字段。
- 协议层 `JobRequest` 没有 MCP 专用字段；短期最小改造应沿用现有 env bridge，长期可再升级 proto 字段。

## 公共输入契约

MCP config JSON 本体：

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

规则：

- `--mcp-config-file` 读取到的 JSON 就是上面的 MCP config 本体，不包含 `mcpConfig` 或 `mcpConfigMode` 外壳。
- Control plane / SDK 里的 `mcpConfig` 字段值就是这个 MCP config 本体。
- MCP config 必须是 JSON object。
- `mcpServers` 缺失时按空配置处理；存在时必须是 object。
- server 名称必须是非空字符串。
- 每个 server 至少要有非空 `command`。
- `args` 必须是字符串数组；缺失时视为 `[]`。
- `env` 必须是 string-to-string object；缺失时视为 `{}`。
- 原始 MCP config 可能包含 secret，不能写入普通 log、stderr、observation record 或 audit payload。

短期 env bridge：

```text
AHAND_AGENT_MCP_CONFIG=<json string>
```

长期 typed API：

```ts
mcpConfig?: Record<string, unknown>
mcpConfigMode?: "replace"
```

`mcpConfig` 不属于 `inputFormat` 或 `outputFormat`，它是 agent 启动上下文。三开关仍然只表达 transport、stdin 转换和 stdout 解析。

`mcpConfigMode` 是独立策略开关，不写进 `mcpConfig` JSON：

| Value | Meaning |
|---|---|
| absent | 默认。传了 `mcpConfig` 时读取可用 base MCP 配置，追加不同名 server，同名 server 由 job `mcpConfig` 覆盖；不传 `mcpConfig` 时不做 AHand 级 MCP 注入。 |
| `replace` | 只使用 job `mcpConfig`，不继承 base MCP 配置。 |

不提供 `disabled` 模式。需要禁用 AHand 级 MCP 注入时不要传 `mcpConfig`。如果需要强制清空可继承的 base MCP servers，则传 MCP config 本体 `{ "mcpServers": {} }`，并在外层设置 `mcpConfigMode=replace`。

强制清空时，MCP config JSON 文件内容仍然只是 config 本体：

```json
{
  "mcpServers": {}
}
```

CLI 策略通过外层 flag 表达：

```bash
ahandctl claude-code /path/to/claude \
  --cwd "$PWD" \
  --prompt "Inspect this repo" \
  --mcp-config-file ./empty-mcp.json \
  --mcp-config-mode replace
```

## 调用方式

CLI debug path:

```bash
ahandctl claude-code /path/to/claude \
  --cwd "$PWD" \
  --prompt "Inspect this repo" \
  --mcp-config-file ./mcp.json
```

Control plane:

```json
{
  "deviceId": "dev-1",
  "executionMode": "pipe_stream",
  "inputFormat": "claude-stream-json",
  "outputFormat": "claude-stream-json",
  "executable": "/path/to/claude",
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
  executable: "/path/to/claude",
  inputFormat: "claude-stream-json",
  outputFormat: "claude-stream-json",
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

- 在 `CreateJobRequest` 增加 `mcp_config: Option<serde_json::Value>`，serde 名称为 `mcpConfig`。
- 在 `CreateJobRequest` 增加 `mcp_config_mode: Option<String>`，serde 名称为 `mcpConfigMode`。
- 在 SDK `SpawnParams` 和 `SpawnAgentParams` 增加 `mcpConfig?: Record<string, unknown>` 和 `mcpConfigMode?: "replace"`。
- Hub 对 `mcpConfig` 做 JSON object 校验，通过后序列化到 `AHAND_AGENT_MCP_CONFIG`。
- Hub 对 `mcpConfigMode` 做 enum 校验，通过后序列化到 `AHAND_AGENT_MCP_CONFIG_MODE`；缺省为空，表示默认 merge。
- Hub 必须覆盖用户 env 里伪造的 `AHAND_AGENT_MCP_CONFIG`，和现有 `AHAND_INPUT_FORMAT` 覆盖策略一致。
- Hub 必须覆盖用户 env 里伪造的 `AHAND_AGENT_MCP_CONFIG_MODE`。
- `ahandctl claude-code` 增加 `--mcp-config <json>`、`--mcp-config-file <path>` 和 `--mcp-config-mode replace`；两个 config 来源不能同时出现。
- `--mcp-config` / `--mcp-config-file` 的 JSON payload 是 MCP config 本体，不是 control-plane request body。

### Phase 2: Daemon Config Loading

- `ClaudeCodeConfig` 增加 `mcp_config: Option<serde_json::Value>` 或保存已校验的 raw JSON string。
- `ClaudeCodeConfig` 增加 `mcp_config_mode`，空值表示默认 merge。
- 从 `AHAND_AGENT_MCP_CONFIG` 读取并校验 JSON。
- 从 `AHAND_AGENT_MCP_CONFIG_MODE` 读取策略开关。
- malformed config 直接 reject job，错误信息只说明 schema 问题，不回显原始 JSON。
- `is_filtered_claude_env` 增加 `AHAND_AGENT_MCP_CONFIG` 和 `AHAND_AGENT_MCP_CONFIG_MODE`，避免用户 env 透传进 Claude 子进程。

### Phase 3: Temp File Injection

- 在启动 Claude 前创建私有临时 MCP 文件。
- `replace` 模式下文件内容为 job `mcpConfig`。
- 默认模式下先读取可用 base Claude MCP 配置，再与 job `mcpConfig` 合并后写入文件；不同名 server 追加，同名 server 由 job 覆盖。
- 未传 `mcpConfig` 时不创建 MCP 文件，不传 `--mcp-config`。
- 文件内容为 Claude 风格 JSON object，不做 Claude schema 之外的转换。
- 文件权限应尽量设为 `0600`；父目录使用系统安全 temp 或 run-private temp。
- `spawn_claude` 在已有参数基础上追加：

```text
--mcp-config <temp-file>
--strict-mcp-config
```

- 当前已经有 `--strict-mcp-config`，需要保证启用 MCP 注入时它和 `--mcp-config` 一起出现。
- 任务结束、取消、spawn 失败时清理临时文件。

### Phase 4: Observation and Audit

- 输出一条脱敏 observation/audit record，说明 MCP 已注入：

```json
{
  "kind": "audit",
  "action": "mcp_config_injected",
  "agent": "claude-code",
  "serverNames": ["filesystem"],
  "serverCount": 1,
  "configSha256": "..."
}
```

- 不输出 `env` 值、完整 args 中疑似 token 的内容、临时文件内容。
- `claude-events.jsonl` 继续保存 Claude raw stdout；MCP config 不进入 raw stdout。

### Phase 5: Tests

- Hub control-plane test：`mcpConfig` 被序列化到 `AHAND_AGENT_MCP_CONFIG`，用户 env 伪造值被覆盖。
- Hub control-plane test：`mcpConfigMode` 被序列化到 `AHAND_AGENT_MCP_CONFIG_MODE`，用户 env 伪造值被覆盖。
- SDK test：`spawnAgent` 能发送 `mcpConfig`。
- SDK test：`spawnAgent` 能发送独立 `mcpConfigMode`。
- CLI test：`--mcp-config-file` 读取 JSON，`--mcp-config` 与 `--mcp-config-file` 冲突时报错，`--mcp-config-mode` 只接受 `replace`。
- Daemon unit test：无 config 不传 `--mcp-config`；有 config 传 `--mcp-config <file>` 且文件 JSON 正确。
- Daemon unit test：默认模式追加不同名 server，同名 server 覆盖；`replace` 不继承 base；未传 `mcpConfig` 不传 `--mcp-config`。
- Daemon unit test：malformed JSON / bad schema 失败且错误不泄漏 secret。
- Fake Claude smoke test：fake executable 记录 argv，确认存在 `--mcp-config` 和 `--strict-mcp-config`。

## 接受标准

- Claude Code job 在 `executionMode=pipe_stream`、`inputFormat=claude-stream-json`、`outputFormat=claude-stream-json` 下可以注入 MCP。
- 调用方不需要读取或修改全局 Claude 配置。
- `mcpConfig` 的 secret 不出现在 stdout、stderr、普通日志、observation 或 audit 明文里。
- `inputFormat=raw` / `outputFormat=raw` 不受影响。
- 旧 `format` 字段仍仅作为兼容 alias；新实现不增加任何第四个“运行后端选择”开关。
