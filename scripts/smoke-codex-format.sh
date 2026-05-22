#!/bin/bash
# Smoke test AHand -> Codex -> normalized observation JSONL.
#
# This script starts a temporary local IPC daemon, runs a caller-provided prompt
# through Codex, and verifies that caller-facing stdout is converted to
# AHand AgentObservationRecord JSONL with outputFormat=codex-jsonl.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
SMOKE_CWD="${AHAND_SMOKE_CWD:-$ROOT_DIR}"
TARGET_FILE="${AHAND_SMOKE_FILE:-crates/ahand-protocol/src/lib.rs}"
TIMEOUT_MS="${AHAND_SMOKE_TIMEOUT_MS:-60000}"
PROMPT="${AHAND_SMOKE_PROMPT:-}"
MCP_CONFIG_FILE="${AHAND_SMOKE_MCP_CONFIG_FILE:-}"
MCP_CONFIG_MODE="${AHAND_SMOKE_MCP_CONFIG_MODE:-}"
CODEX_BYPASS_APPROVALS="${AHAND_SMOKE_CODEX_BYPASS_APPROVALS:-false}"

usage() {
  cat <<'EOF'
Usage: smoke-codex-format.sh [options] [CODEX_PATH]

Options:
  --cwd DIR             Working directory for the agent task.
  --file PATH           File path used by the default prompt.
  --prompt TEXT         Prompt to send to Codex stdin.
  --prompt-file PATH    Read prompt text from a file.
  --timeout-ms MS       Job timeout in milliseconds.
  --codex-path PATH     Codex executable path.
  --mcp-config-file PATH
                        MCP config JSON body: { "mcpServers": ... }.
  --mcp-config-mode replace
                        Omit for default merge; use replace to ignore inherited servers.
  --dangerously-bypass-approvals-and-sandbox
                        Pass Codex's non-interactive approval/sandbox bypass flag.
  -h, --help            Show this help.

Environment aliases:
  AHAND_SMOKE_CWD, AHAND_SMOKE_FILE, AHAND_SMOKE_PROMPT,
  AHAND_SMOKE_TIMEOUT_MS, AHAND_SMOKE_MCP_CONFIG_FILE,
  AHAND_SMOKE_MCP_CONFIG_MODE, AHAND_SMOKE_CODEX_BYPASS_APPROVALS,
  CODEX_PATH
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --cwd) SMOKE_CWD="$2"; shift 2 ;;
    --file) TARGET_FILE="$2"; shift 2 ;;
    --prompt) PROMPT="$2"; shift 2 ;;
    --prompt-file) PROMPT="$(cat "$2")"; shift 2 ;;
    --timeout-ms) TIMEOUT_MS="$2"; shift 2 ;;
    --codex-path) CODEX_PATH="$2"; shift 2 ;;
    --mcp-config-file) MCP_CONFIG_FILE="$2"; shift 2 ;;
    --mcp-config-mode) MCP_CONFIG_MODE="$2"; shift 2 ;;
    --dangerously-bypass-approvals-and-sandbox) CODEX_BYPASS_APPROVALS="true"; shift ;;
    -h|--help) usage; exit 0 ;;
    --*) echo "ERROR: unknown option $1" >&2; usage >&2; exit 1 ;;
    *) CODEX_PATH="$1"; shift ;;
  esac
done

if [ -z "$PROMPT" ]; then
  PROMPT="Read ${TARGET_FILE} from disk only. Do not edit files. In one short sentence, say whether the file is readable and name the first public constant defined after re-exports."
fi
if [[ "$SMOKE_CWD" != /* ]]; then
  SMOKE_CWD="$ROOT_DIR/$SMOKE_CWD"
fi
MCP_ARGS=()
if [ -n "$MCP_CONFIG_FILE" ]; then
  MCP_ARGS+=(--mcp-config-file "$MCP_CONFIG_FILE")
fi
if [ -n "$MCP_CONFIG_MODE" ]; then
  MCP_ARGS+=(--mcp-config-mode "$MCP_CONFIG_MODE")
fi
CODEX_EXEC_ARGS=()
if [ "$CODEX_BYPASS_APPROVALS" = "true" ]; then
  CODEX_EXEC_ARGS+=(--dangerously-bypass-approvals-and-sandbox)
else
  CODEX_EXEC_ARGS+=(--sandbox read-only)
fi

if [ -n "${CODEX_PATH:-}" ]; then
  CODEX="$CODEX_PATH"
elif command -v codex >/dev/null 2>&1; then
  CODEX="$(command -v codex)"
elif [ -x /Applications/Codex.app/Contents/Resources/codex ]; then
  CODEX="/Applications/Codex.app/Contents/Resources/codex"
else
  echo "ERROR: codex not found. Set CODEX_PATH=/absolute/path/to/codex." >&2
  exit 1
fi

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/ahand-codex-smoke.XXXXXX")"
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
printf '%s\n' "$PROMPT" | "$ROOT_DIR/target/debug/ahandctl" \
  --ipc "$SOCKET" \
  exec \
  --execution-mode pipe_stream \
  --input-format text \
  --output-format codex-jsonl \
  --result-parser codex-jsonl \
  --cwd "$SMOKE_CWD" \
  --timeout-ms "$TIMEOUT_MS" \
  "${MCP_ARGS[@]}" \
  "$CODEX" -- exec \
    --ignore-rules \
    --skip-git-repo-check \
    "${CODEX_EXEC_ARGS[@]}" \
    --json \
    --cd "$SMOKE_CWD" \
    - >"$OUT_FILE" 2>&1
STATUS=$?
set -e

node - "$OUT_FILE" codex codex-jsonl "$STATUS" <<'NODE'
const fs = require("fs");
const [file, expectedAgent, expectedFormat, statusText] = process.argv.slice(2);
const status = Number(statusText);
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
if (!records.some((r) => ["llm_call_delta", "tool_call_output", "llm_call_end"].includes(r.kind))) {
  fail("missing useful observation kind");
}
console.log(`ok: ${expectedAgent} emitted ${records.length} normalized records with outputFormat=${expectedFormat}`);
NODE

echo "stdout: $OUT_FILE"
