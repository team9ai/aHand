# Codex MCP Injection Plan

**Goal:** 为 AHand 的 Codex 调用链路设计任务级 MCP 注入。Codex 不读取 Claude 风格 JSON 文件，也不是 ACP `session/new` 参数；它读取 `CODEX_HOME/config.toml` 中的 `[mcp_servers.*]`。因此 AHand 需要把统一 `mcpConfig` 转换为 per-job `CODEX_HOME/config.toml`，再以 `CODEX_HOME=<job codex home>` 启动 Codex。该设计仍使用三开关：`executionMode=pipe_stream`、`inputFormat=text`、`outputFormat=codex-jsonl`。

## 参考

- `docs/CODEX_MCP_INJECTION.md`
- `docs/plans/2026-05-13-codex-jsonl-result-parser.md`
- `docs/usage/claude-codex-pipe-stream.md`
- `docs/agent-stdio-formats.md`
- `docs/status/codex.md`

## 当前状态

- AHand 已有 Codex JSONL 输出解析：`outputFormat=codex-jsonl`。
- 当前 Codex 最常见调用仍是通用 `exec`：

```bash
ahandctl exec \
  --execution-mode pipe_stream \
  --input-format text \
  --output-format codex-jsonl \
  --result-parser codex-jsonl \
  codex exec --json "prompt"
```

- 当前没有 AHand-managed per-job `CODEX_HOME`。
- 参考文档说明 Codex 的 MCP 来源是 `config.toml` 的 `[mcp_servers.*]`，不是 `agent.mcp_config`。

## 公共输入契约

MCP config JSON 本体仍使用 Claude 风格公共 schema：

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

Codex adapter 负责转成 TOML：

```toml
[mcp_servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]

[mcp_servers.filesystem.env]
TOKEN = "secret"
```

`mcpConfig` 不改变 stdin/stdout 协议：

- `inputFormat=text` 继续把 prompt 写给 Codex stdin 或构造 Codex exec 输入。
- `outputFormat=codex-jsonl` 继续把 Codex stdout JSONL 转成 `AgentObservationRecord`。
- MCP 注入只改变 Codex 启动环境中的 `CODEX_HOME/config.toml`。

`mcpConfigMode` 是独立策略开关，不写进 `mcpConfig` JSON：

| Value | Meaning |
|---|---|
| absent | 默认。传了 `mcpConfig` 时读取 base `config.toml`，追加不同名 `[mcp_servers.*]`，同名由 job `mcpConfig` 覆盖；不传 `mcpConfig` 时不做 AHand 级 MCP 注入。 |
| `replace` | 生成的 per-job `config.toml` 只保留 job `mcpConfig` 中的 MCP servers。非 MCP 的 Codex 配置仍可从 base 继承。 |

不提供 `disabled` 模式。需要禁用 AHand 级 MCP 注入时不要传 `mcpConfig`。如果需要强制清空可继承的 base MCP servers，则传 MCP config 本体 `{ "mcpServers": {} }`，并在外层设置 `mcpConfigMode=replace`。

强制清空时，MCP config JSON 文件内容仍然只是：

```json
{
  "mcpServers": {}
}
```

策略通过外层 flag 表达：

```bash
ahandctl codex /path/to/codex \
  --cwd "$PWD" \
  --prompt "Inspect this repo" \
  --mcp-config-file ./empty-mcp.json \
  --mcp-config-mode replace
```

## 调用方式

第一阶段建议增加 Codex debug helper，而不是让用户手写 `CODEX_HOME`：

```bash
ahandctl codex /path/to/codex \
  --cwd "$PWD" \
  --prompt "Inspect this repo" \
  --mcp-config-file ./mcp.json
```

等价底层三开关：

```text
executionMode=pipe_stream
inputFormat=text
outputFormat=codex-jsonl
```

Control plane:

```json
{
  "deviceId": "dev-1",
  "executionMode": "pipe_stream",
  "inputFormat": "text",
  "outputFormat": "codex-jsonl",
  "executable": "/path/to/codex",
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
await client.spawn({
  deviceId,
  tool: "/path/to/codex",
  executionMode: "pipe_stream",
  inputFormat: "text",
  outputFormat: "codex-jsonl",
  prompt: "Inspect this repo",
  executable: "/path/to/codex",
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

- 和 Claude/Hermes 共用 `mcpConfig`。
- 和 Claude/Hermes 共用独立 `mcpConfigMode` 策略字段，不写进 `mcpConfig`。
- Hub 将 `mcpConfig` 校验后写入 `AHAND_AGENT_MCP_CONFIG`。
- Hub 将 `mcpConfigMode` 校验后写入 `AHAND_AGENT_MCP_CONFIG_MODE`，缺省为空，表示默认 merge。
- SDK `SpawnParams` 增加 `mcpConfig?: Record<string, unknown>` 和 `mcpConfigMode?: "replace"`。
- CLI 增加 `ahandctl codex` debug command，支持 `--mcp-config <json>` / `--mcp-config-file <path>` / `--mcp-config-mode replace`。
- `--mcp-config` / `--mcp-config-file` 的 JSON payload 是 MCP config 本体，不是 control-plane request body。
- 不新增第四个“运行后端选择”开关；Codex 由三开关组合识别。

### Phase 2: Codex Managed Home

- 新增 Codex input runner/helper，只在以下组合启用：

```text
executionMode=pipe_stream
inputFormat=text
outputFormat=codex-jsonl
executable=<codex path>
mcpConfig present
```

- 为每个 job 创建私有目录：

```text
<run-dir>/codex-home/
```

- 如果用户设置了 `CODEX_HOME`，读取其中的 `config.toml` 作为 base；否则读取 `~/.codex/config.toml`。
- 复制或生成 per-job `config.toml`。
- 设置子进程环境：

```text
CODEX_HOME=<run-dir>/codex-home
```

- 子进程不应该看到 `AHAND_AGENT_MCP_CONFIG`。

### Phase 3: JSON to TOML Converter

- 新增 converter：

```rust
fn codex_mcp_toml(config: &serde_json::Value) -> Result<toml_edit::DocumentMut, String>
```

- 使用 TOML parser/editor，不用字符串拼接。
- 输出 `[mcp_servers.<name>]`。
- `command`、`args`、`env` 的 schema 和公共输入契约一致。
- server name 写入 TOML table key 前必须校验或正确 quote，避免 TOML injection。

### Phase 4: Merge Priority

优先级从高到低：

1. AHand job `mcpConfig`
2. per-job generated daemon settings
3. copied user `CODEX_HOME/config.toml`

合并规则：

- 默认模式下，如果 base config 已有同名 `[mcp_servers.<name>]`，job `mcpConfig` 覆盖该 server；不同名 server 保留并追加。
- `replace` 模式下，删除 base config 中已有 `[mcp_servers.*]`，只写 job `mcpConfig` 的 servers。
- 未传 `mcpConfig` 时不做 AHand 级 MCP 注入。
- 覆盖发生时写脱敏 audit record，只记录 server name，不记录 secret。

### Phase 5: Codex Command Shape

目标命令形态：

```text
CODEX_HOME=<run-dir>/codex-home codex exec --json -
```

- prompt 由 `inputFormat=text` 写入 stdin。
- stdout 由 `outputFormat=codex-jsonl` 转成 observation JSONL。
- raw child stdout 继续写 run artifact。
- 如果用户显式手写 `args`，第一阶段不自动注入 MCP；MCP 注入只在 Codex helper/managed shape 下启用，避免误改任意 raw process。

### Phase 6: Observation and Redaction

输出脱敏 audit/observation：

```json
{
  "kind": "audit",
  "action": "mcp_config_injected",
  "agent": "codex",
  "serverNames": ["filesystem"],
  "serverCount": 1,
  "target": "CODEX_HOME/config.toml",
  "configSha256": "..."
}
```

- 不把完整 `config.toml` 写入 caller-facing stdout。
- 如果需要调试 artifact，写 `codex-mcp-metadata.json`，只包含 server names、hash、base config source、是否发生覆盖。
- `TOKEN`、`DATABASE_URL` 等 env value 不进入普通日志。

### Phase 7: Tests

- Converter unit tests：valid JSON 转 TOML、多个 server、env、quoted table key、bad args、bad env。
- Mode tests：默认模式覆盖同名并保留不同名，`replace` 删除 base MCP servers，未传 `mcpConfig` 不写 job MCP servers。
- Runner test：fake Codex 读取 `CODEX_HOME/config.toml`，确认 `[mcp_servers.*]` 存在。
- Runner test：没有 `mcpConfig` 时不创建 managed `CODEX_HOME`，保留现有 generic exec 行为。
- Parser integration test：Codex JSONL stdout 仍能被 `outputFormat=codex-jsonl` 转成目标 observation JSONL。
- Redaction test：secret 不出现在 stdout、stderr、observations、audit 明文里。

## 接受标准

- Codex 可以通过 AHand job 级 `mcpConfig` 获得 MCP server。
- 不要求用户修改全局 `~/.codex/config.toml`。
- AHand 不把 `mcpConfig` 当作 stdin/stdout format；它只影响 Codex 启动环境。
- 旧的 raw `exec` 行为保持不变。
- 新实现不依赖 `format` 或任何第四个“运行后端选择”字段。
