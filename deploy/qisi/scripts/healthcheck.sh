#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

if [[ ! -f .env ]]; then
  echo "missing required file: .env" >&2
  exit 1
fi

set -a
source .env
set +a

: "${AHAND_HUB_HOST_PORT:?AHAND_HUB_HOST_PORT is required in .env}"
: "${AHAND_HUB_DASHBOARD_HOST_PORT:?AHAND_HUB_DASHBOARD_HOST_PORT is required in .env}"

check_url() {
  local name="$1"
  local url="$2"
  local deadline=$((SECONDS + 120))

  while (( SECONDS < deadline )); do
    if curl -fsS "$url" >/dev/null; then
      echo "$name healthy: $url"
      return 0
    fi
    sleep 3
  done

  echo "$name did not become healthy: $url" >&2
  return 1
}

check_url "ahand-hub" "http://127.0.0.1:${AHAND_HUB_HOST_PORT}/api/health"
check_url "ahand-hub-dashboard" "http://127.0.0.1:${AHAND_HUB_DASHBOARD_HOST_PORT}/login"
