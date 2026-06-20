#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

NEXT_IMAGES="${1:-.env.images.next}"
ACTIVE_IMAGES=".env.images"
COMPOSE_ARGS=(--env-file .env --env-file .env.secrets --env-file .env.images -f compose.yml)
CANDIDATE_COMPOSE=""
PROMOTION_TMP=""
LOCK_DIR=""
PROMOTED=0
HAD_PREVIOUS=0
BACKUP_PATH=""

require_file() {
  local path="$1"
  if [[ ! -f "$path" ]]; then
    echo "missing required file: $path" >&2
    exit 1
  fi
}

absolute_path() {
  local path="$1"
  local dir
  dir="$(cd "$(dirname "$path")" && pwd)"
  echo "$dir/$(basename "$path")"
}

cleanup() {
  if [[ -n "$CANDIDATE_COMPOSE" && -f "$CANDIDATE_COMPOSE" ]]; then
    rm -f "$CANDIDATE_COMPOSE"
  fi
  if [[ -n "$PROMOTION_TMP" && -f "$PROMOTION_TMP" ]]; then
    rm -f "$PROMOTION_TMP"
  fi
  if [[ -n "$LOCK_DIR" && -d "$LOCK_DIR" ]]; then
    rmdir "$LOCK_DIR" 2>/dev/null || true
  fi
}

rollback_on_failure() {
  local status=$?
  trap - ERR

  if (( PROMOTED )); then
    echo "deploy failed after promoting $ACTIVE_IMAGES" >&2
    if (( HAD_PREVIOUS )); then
      echo "restoring previous $ACTIVE_IMAGES from $BACKUP_PATH" >&2
      cp "$BACKUP_PATH" "$ACTIVE_IMAGES"
      if ! docker compose "${COMPOSE_ARGS[@]}" up -d --remove-orphans; then
        echo "rollback compose up failed; inspect the host before retrying" >&2
      fi
    else
      echo "no previous $ACTIVE_IMAGES existed; promoted file left in place for inspection" >&2
    fi
  fi

  exit "$status"
}

acquire_lock() {
  mkdir -p .deploy-locks

  if command -v flock >/dev/null 2>&1; then
    exec 9>".deploy-locks/${DEPLOY_ENV}.lock"
    if ! flock -n 9; then
      echo "another deploy is already running for $DEPLOY_ENV" >&2
      exit 1
    fi
    return
  fi

  LOCK_DIR=".deploy-locks/${DEPLOY_ENV}.lockdir"
  if ! mkdir "$LOCK_DIR" 2>/dev/null; then
    echo "another deploy is already running for $DEPLOY_ENV" >&2
    exit 1
  fi
}

write_candidate_compose() {
  local env_path
  local secrets_path
  local images_path

  env_path="$(absolute_path .env)"
  secrets_path="$(absolute_path .env.secrets)"
  images_path="$(absolute_path "$NEXT_IMAGES")"
  CANDIDATE_COMPOSE="$(mktemp "$ROOT_DIR/.compose.candidate.XXXXXX.yml")"

  awk \
    -v env_path="$env_path" \
    -v secrets_path="$secrets_path" \
    -v images_path="$images_path" \
    '{
      if ($0 ~ /^[[:space:]]+- \.env$/) {
        sub(/\.env$/, env_path)
      } else if ($0 ~ /^[[:space:]]+- \.env\.secrets$/) {
        sub(/\.env\.secrets$/, secrets_path)
      } else if ($0 ~ /^[[:space:]]+- \.env\.images$/) {
        sub(/\.env\.images$/, images_path)
      }
      print
    }' compose.yml >"$CANDIDATE_COMPOSE"
}

require_file compose.yml
require_file .env
require_file .env.secrets
require_file "$NEXT_IMAGES"

set -a
source .env
set +a

: "${DEPLOY_ENV:?DEPLOY_ENV is required in .env}"

trap cleanup EXIT
acquire_lock
write_candidate_compose

CANDIDATE_COMPOSE_ARGS=(--env-file .env --env-file .env.secrets --env-file "$NEXT_IMAGES" -f "$CANDIDATE_COMPOSE")

docker compose "${CANDIDATE_COMPOSE_ARGS[@]}" config >/dev/null
if [[ "${SKIP_PULL:-0}" == "1" ]]; then
  echo "SKIP_PULL=1: using images already present on this Docker host"
else
  docker compose "${CANDIDATE_COMPOSE_ARGS[@]}" pull
fi

mkdir -p .deploy-history
if [[ -f "$ACTIVE_IMAGES" ]]; then
  HAD_PREVIOUS=1
  BACKUP_PATH=".deploy-history/$(date -u +%Y%m%dT%H%M%SZ)-$$.env.images"
  cp "$ACTIVE_IMAGES" "$BACKUP_PATH"
fi

trap rollback_on_failure ERR
PROMOTION_TMP="$(mktemp "$ROOT_DIR/.env.images.XXXXXX.tmp")"
cp "$NEXT_IMAGES" "$PROMOTION_TMP"
mv "$PROMOTION_TMP" "$ACTIVE_IMAGES"
PROMOTION_TMP=""
PROMOTED=1

docker compose "${COMPOSE_ARGS[@]}" up -d --remove-orphans

bash scripts/healthcheck.sh
