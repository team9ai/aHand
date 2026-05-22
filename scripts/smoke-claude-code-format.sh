#!/bin/bash
# Smoke test AHand -> Claude Code -> normalized observation JSONL.
#
# This script starts a temporary local IPC daemon, runs a caller-provided prompt
# through Claude Code, and verifies that caller-facing stdout is converted to
# AHand AgentObservationRecord JSONL with outputFormat=claude-stream-json.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
SMOKE_CWD="${AHAND_SMOKE_CWD:-$ROOT_DIR}"
TARGET_FILE="${AHAND_SMOKE_FILE:-crates/ahand-protocol/src/lib.rs}"
TIMEOUT_MS="${AHAND_SMOKE_TIMEOUT_MS:-60000}"
PROMPT="${AHAND_SMOKE_PROMPT:-}"
REQUIRE_TOOL="${AHAND_SMOKE_REQUIRE_TOOL:-false}"
MCP_CONFIG_FILE="${AHAND_SMOKE_MCP_CONFIG_FILE:-}"
MCP_CONFIG_MODE="${AHAND_SMOKE_MCP_CONFIG_MODE:-}"
PERMISSION_MODE="${AHAND_SMOKE_PERMISSION_MODE:-}"

usage() {
  cat <<'EOF'
Usage: smoke-claude-code-format.sh [options] [CLAUDE_PATH]

Options:
  --cwd DIR             Working directory for the agent task.
  --file PATH           File path used by the default prompt.
  --prompt TEXT         Prompt to send to Claude Code.
  --prompt-file PATH    Read prompt text from a file.
  --timeout-ms MS       Job timeout in milliseconds.
  --claude-path PATH    Claude executable path.
  --mcp-config-file PATH
                        MCP config JSON body: { "mcpServers": ... }.
  --mcp-config-mode replace
                        Omit for default merge; use replace to ignore inherited servers.
  --permission-mode MODE
                        Claude permission mode, for example default or bypassPermissions.
  --require-tool        Require at least one tool_call observation.
  -h, --help            Show this help.

Environment aliases:
  AHAND_SMOKE_CWD, AHAND_SMOKE_FILE, AHAND_SMOKE_PROMPT,
  AHAND_SMOKE_TIMEOUT_MS, AHAND_SMOKE_REQUIRE_TOOL,
  AHAND_SMOKE_MCP_CONFIG_FILE, AHAND_SMOKE_MCP_CONFIG_MODE,
  AHAND_SMOKE_PERMISSION_MODE, CLAUDE_PATH
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --cwd) SMOKE_CWD="$2"; shift 2 ;;
    --file) TARGET_FILE="$2"; shift 2 ;;
    --prompt) PROMPT="$2"; shift 2 ;;
    --prompt-file) PROMPT="$(cat "$2")"; shift 2 ;;
    --timeout-ms) TIMEOUT_MS="$2"; shift 2 ;;
    --claude-path) CLAUDE_PATH="$2"; shift 2 ;;
    --mcp-config-file) MCP_CONFIG_FILE="$2"; shift 2 ;;
    --mcp-config-mode) MCP_CONFIG_MODE="$2"; shift 2 ;;
    --permission-mode) PERMISSION_MODE="$2"; shift 2 ;;
    --require-tool) REQUIRE_TOOL="true"; shift ;;
    -h|--help) usage; exit 0 ;;
    --*) echo "ERROR: unknown option $1" >&2; usage >&2; exit 1 ;;
    *) CLAUDE_PATH="$1"; shift ;;
  esac
done

if [ -z "$PROMPT" ]; then
  PROMPT="Use the Read tool to read ${TARGET_FILE} only. Do not edit files. In one short sentence, say whether the file is readable and name the first public constant defined after re-exports."
fi
if [[ "$SMOKE_CWD" != /* ]]; then
  SMOKE_CWD="$ROOT_DIR/$SMOKE_CWD"
fi

if [ -n "${CLAUDE_PATH:-}" ]; then
  CLAUDE="$CLAUDE_PATH"
else
  if [ -f "$HOME/.nvs/nvs.sh" ]; then
    set +u
    # shellcheck disable=SC1091
    source "$HOME/.nvs/nvs.sh"
    nvs use 22 >/dev/null 2>&1 || true
    set -u
  fi
  if command -v claude >/dev/null 2>&1; then
    CLAUDE="$(command -v claude)"
  else
    echo "ERROR: claude not found. Set CLAUDE_PATH=/absolute/path/to/claude." >&2
    exit 1
  fi
fi
MCP_ARGS=()
if [ -n "$MCP_CONFIG_FILE" ]; then
  MCP_ARGS+=(--mcp-config-file "$MCP_CONFIG_FILE")
fi
if [ -n "$MCP_CONFIG_MODE" ]; then
  MCP_ARGS+=(--mcp-config-mode "$MCP_CONFIG_MODE")
fi
if [ -n "$PERMISSION_MODE" ]; then
  MCP_ARGS+=(--permission-mode "$PERMISSION_MODE")
fi

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/ahand-claude-smoke.XXXXXX")"
SOCKET="$TMP_DIR/ahand.sock"
DATA_DIR="$TMP_DIR/data"
DAEMON_LOG="$TMP_DIR/ahandd.log"
OUT_FILE="$TMP_DIR/stdout.jsonl"

cleanup() {
  if [ -n "${DAEMON_PID:-}" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
    kill "$DAEMON_PID" 2>/dev/null || true
    wait "$DAEMON_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

cd "$ROOT_DIR"
cargo build -p ahandd -p ahandctl >/dev/null

"$ROOT_DIR/target/debug/ahandd" \
  --mode local \
  --debug-ipc \
  --ipc-socket "$SOCKET" \
  --data-dir "$DATA_DIR" >"$DAEMON_LOG" 2>&1 &
DAEMON_PID=$!

for _ in $(seq 1 100); do
  [ -S "$SOCKET" ] && break
  sleep 0.05
done
if [ ! -S "$SOCKET" ]; then
  echo "ERROR: daemon IPC socket was not created." >&2
  cat "$DAEMON_LOG" >&2 || true
  exit 1
fi

set +e
"$ROOT_DIR/target/debug/ahandctl" \
  --ipc "$SOCKET" \
  claude-code \
  --cwd "$SMOKE_CWD" \
  --timeout-ms "$TIMEOUT_MS" \
  --prompt "$PROMPT" \
  "${MCP_ARGS[@]}" \
  "$CLAUDE" >"$OUT_FILE" 2>&1
STATUS=$?
set -e

node - "$OUT_FILE" claude-code claude-stream-json "$STATUS" "$REQUIRE_TOOL" <<'NODE'
const fs = require("fs");
const [file, expectedAgent, expectedFormat, statusText, requireToolText] = process.argv.slice(2);
const status = Number(statusText);
const requireTool = requireToolText === "true";
const lines = fs.readFileSync(file, "utf8").split(/\r?\n/).filter(Boolean);
const records = [];
for (const line of lines) {
  if (!line.trim().startsWith("{")) continue;
  try {
    records.push(JSON.parse(line));
  } catch {
    // Non-JSON diagnostics from the child process are allowed in smoke output.
  }
}
function fail(message) {
  console.error(`ERROR: ${message}`);
  console.error(`Output saved at: ${file}`);
  process.exit(1);
}
if (status !== 0) fail(`ahandctl exited with ${status}`);
if (records.length === 0) fail("no JSON observation records were emitted");
if (!records.some((r) => r.schemaVersion === 1)) fail("missing schemaVersion=1 records");
if (!records.some((r) => r.agent?.agentKind === expectedAgent)) fail(`missing agentKind=${expectedAgent}`);
if (!records.some((r) => r.runtime?.outputFormat === expectedFormat)) fail(`missing outputFormat=${expectedFormat}`);
if (requireTool && !records.some((r) => r.kind === "tool_call_start" || r.kind === "tool_call_output")) {
  fail("missing Claude Code tool observations");
}
console.log(`ok: ${expectedAgent} emitted ${records.length} normalized records with outputFormat=${expectedFormat}`);
NODE

echo "stdout: $OUT_FILE"
