# Qisi aHand CN Deployment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a qisi/Aliyun deployment path so hub-related CI updates deploy to the CN aHand stack in addition to the existing global AWS/t9 deployment.

**Architecture:** Keep the current AWS/t9 deployment untouched. Add an independent `deploy/qisi/` Compose deployment and a branch-based GitHub Actions workflow that builds the existing `deploy/hub/Dockerfile` targets, pushes to the qisi registry, copies image tags to qisi/qisi-dev, and runs a rollback-capable host deploy script.

**Tech Stack:** Docker, Docker Compose, Caddy, GitHub Actions, qisi zot registry, SSH/SCP, Bash, existing Rust `ahand-hub`, existing Next.js `ahand-hub-dashboard`.

---

## File Structure

- Create `deploy/qisi/compose.yml`: shared Compose topology for one CN environment.
- Create `deploy/qisi/env/dev.env.example`: non-secret qisi-dev dev settings.
- Create `deploy/qisi/env/staging.env.example`: non-secret qisi-dev staging settings.
- Create `deploy/qisi/env/production.env.example`: non-secret qisi production settings.
- Create `deploy/qisi/env/secrets.env.example`: required secret keys plus commented optional OSS/S3 keys.
- Create `deploy/qisi/caddy/dev.Caddyfile`: dev public routes to loopback ports.
- Create `deploy/qisi/caddy/staging.Caddyfile`: staging public routes to loopback ports.
- Create `deploy/qisi/caddy/production.Caddyfile`: production public routes to loopback ports.
- Create `deploy/qisi/scripts/deploy.sh`: host-side locked deploy, image promotion, healthcheck, rollback.
- Create `deploy/qisi/scripts/healthcheck.sh`: host-side hub and dashboard health checks.
- Create `.github/workflows/qisi-deploy.yml`: CI image build, registry push, SSH sync, host deploy.

The existing `deploy/hub/Dockerfile` is reused. Do not create qisi-specific Dockerfiles in this implementation.

---

### Task 1: Add Qisi Runtime Files

**Files:**
- Create: `deploy/qisi/compose.yml`
- Create: `deploy/qisi/env/dev.env.example`
- Create: `deploy/qisi/env/staging.env.example`
- Create: `deploy/qisi/env/production.env.example`
- Create: `deploy/qisi/env/secrets.env.example`
- Create: `deploy/qisi/caddy/dev.Caddyfile`
- Create: `deploy/qisi/caddy/staging.Caddyfile`
- Create: `deploy/qisi/caddy/production.Caddyfile`
- Create: `deploy/qisi/scripts/deploy.sh`
- Create: `deploy/qisi/scripts/healthcheck.sh`

- [ ] **Step 1: Create directories**

Run:

```bash
mkdir -p deploy/qisi/env deploy/qisi/caddy deploy/qisi/scripts
```

Expected: directories exist and `git status --short deploy/qisi` shows untracked `deploy/qisi/`.

- [ ] **Step 2: Create Compose file**

Create `deploy/qisi/compose.yml` with this content:

```yaml
name: ahand-hub-${DEPLOY_ENV}

services:
  hub:
    image: ${AHAND_HUB_IMAGE}
    container_name: ahand-hub-${DEPLOY_ENV}-hub
    restart: unless-stopped
    env_file:
      - .env
      - .env.secrets
      - .env.images
    environment:
      AHAND_HUB_BIND_ADDR: ${AHAND_HUB_BIND_ADDR:-0.0.0.0:1515}
      AHAND_HUB_DASHBOARD_ALLOWED_ORIGINS: ${AHAND_HUB_DASHBOARD_ALLOWED_ORIGINS}
      AHAND_HUB_LOG_FORMAT: ${AHAND_HUB_LOG_FORMAT:-json}
      AHAND_HUB_LOG_LEVEL: ${AHAND_HUB_LOG_LEVEL:-info}
      AHAND_HUB_AUDIT_RETENTION_DAYS: ${AHAND_HUB_AUDIT_RETENTION_DAYS:-90}
      AHAND_HUB_AUDIT_FALLBACK_PATH: ${AHAND_HUB_AUDIT_FALLBACK_PATH:-/var/lib/ahand-hub/audit-fallback.jsonl}
      AHAND_HUB_WEBHOOK_MAX_RETRIES: ${AHAND_HUB_WEBHOOK_MAX_RETRIES:-8}
      AHAND_HUB_WEBHOOK_TIMEOUT_MS: ${AHAND_HUB_WEBHOOK_TIMEOUT_MS:-5000}
      GIT_SHA: ${GIT_SHA:-unknown}
      SENTRY_ENVIRONMENT: ${SENTRY_ENVIRONMENT:-production}
      SENTRY_RELEASE: ${SENTRY_RELEASE:-unknown}
    ports:
      - "127.0.0.1:${AHAND_HUB_HOST_PORT}:1515"
    volumes:
      - hub-audit-data:/var/lib/ahand-hub
    healthcheck:
      test: ["CMD", "curl", "-fsS", "http://127.0.0.1:1515/api/health"]
      interval: 10s
      timeout: 5s
      retries: 12
      start_period: 20s

  dashboard:
    image: ${AHAND_HUB_DASHBOARD_IMAGE}
    container_name: ahand-hub-${DEPLOY_ENV}-dashboard
    restart: unless-stopped
    depends_on:
      hub:
        condition: service_healthy
    env_file:
      - .env
      - .env.images
    environment:
      AHAND_HUB_BASE_URL: http://hub:1515
      NODE_ENV: production
      PORT: "1516"
      SENTRY_ENVIRONMENT: ${SENTRY_ENVIRONMENT:-production}
      SENTRY_RELEASE: ${SENTRY_RELEASE:-unknown}
    ports:
      - "127.0.0.1:${AHAND_HUB_DASHBOARD_HOST_PORT}:1516"
    healthcheck:
      test: ["CMD-SHELL", "node -e \"fetch('http://127.0.0.1:1516/login').then(r=>process.exit(r.ok?0:1)).catch(()=>process.exit(1))\""]
      interval: 10s
      timeout: 5s
      retries: 12
      start_period: 20s

volumes:
  hub-audit-data:
```

- [ ] **Step 3: Create dev env example**

Create `deploy/qisi/env/dev.env.example` with this content:

```dotenv
DEPLOY_ENV=dev
APP_ENV=development
NODE_ENV=production
SENTRY_ENVIRONMENT=dev

PUBLIC_HUB_DOMAIN=ahand-hub.dev.coffice.qisiai.top
PUBLIC_DASHBOARD_DOMAIN=admin.ahand.dev.coffice.qisiai.top
AHAND_HUB_PUBLIC_URL=https://ahand-hub.dev.coffice.qisiai.top
AHAND_HUB_DASHBOARD_PUBLIC_URL=https://admin.ahand.dev.coffice.qisiai.top

AHAND_HUB_HOST_PORT=5815
AHAND_HUB_DASHBOARD_HOST_PORT=5816

AHAND_HUB_BIND_ADDR=0.0.0.0:1515
AHAND_HUB_DASHBOARD_ALLOWED_ORIGINS=https://admin.ahand.dev.coffice.qisiai.top
AHAND_HUB_LOG_FORMAT=json
AHAND_HUB_LOG_LEVEL=info
AHAND_HUB_AUDIT_RETENTION_DAYS=90
AHAND_HUB_AUDIT_FALLBACK_PATH=/var/lib/ahand-hub/audit-fallback.jsonl
AHAND_HUB_WEBHOOK_MAX_RETRIES=8
AHAND_HUB_WEBHOOK_TIMEOUT_MS=5000
```

- [ ] **Step 4: Create staging env example**

Create `deploy/qisi/env/staging.env.example` with this content:

```dotenv
DEPLOY_ENV=staging
APP_ENV=staging
NODE_ENV=production
SENTRY_ENVIRONMENT=staging

PUBLIC_HUB_DOMAIN=ahand-hub.staging.coffice.qisiai.top
PUBLIC_DASHBOARD_DOMAIN=admin.ahand.staging.coffice.qisiai.top
AHAND_HUB_PUBLIC_URL=https://ahand-hub.staging.coffice.qisiai.top
AHAND_HUB_DASHBOARD_PUBLIC_URL=https://admin.ahand.staging.coffice.qisiai.top

AHAND_HUB_HOST_PORT=4815
AHAND_HUB_DASHBOARD_HOST_PORT=4816

AHAND_HUB_BIND_ADDR=0.0.0.0:1515
AHAND_HUB_DASHBOARD_ALLOWED_ORIGINS=https://admin.ahand.staging.coffice.qisiai.top
AHAND_HUB_LOG_FORMAT=json
AHAND_HUB_LOG_LEVEL=info
AHAND_HUB_AUDIT_RETENTION_DAYS=90
AHAND_HUB_AUDIT_FALLBACK_PATH=/var/lib/ahand-hub/audit-fallback.jsonl
AHAND_HUB_WEBHOOK_MAX_RETRIES=8
AHAND_HUB_WEBHOOK_TIMEOUT_MS=5000
```

- [ ] **Step 5: Create production env example**

Create `deploy/qisi/env/production.env.example` with this content:

```dotenv
DEPLOY_ENV=production
APP_ENV=production
NODE_ENV=production
SENTRY_ENVIRONMENT=production

PUBLIC_HUB_DOMAIN=ahand-hub.coffice.qisiai.top
PUBLIC_DASHBOARD_DOMAIN=admin.ahand.coffice.qisiai.top
AHAND_HUB_PUBLIC_URL=https://ahand-hub.coffice.qisiai.top
AHAND_HUB_DASHBOARD_PUBLIC_URL=https://admin.ahand.coffice.qisiai.top

AHAND_HUB_HOST_PORT=3815
AHAND_HUB_DASHBOARD_HOST_PORT=3816

AHAND_HUB_BIND_ADDR=0.0.0.0:1515
AHAND_HUB_DASHBOARD_ALLOWED_ORIGINS=https://admin.ahand.coffice.qisiai.top
AHAND_HUB_LOG_FORMAT=json
AHAND_HUB_LOG_LEVEL=info
AHAND_HUB_AUDIT_RETENTION_DAYS=90
AHAND_HUB_AUDIT_FALLBACK_PATH=/var/lib/ahand-hub/audit-fallback.jsonl
AHAND_HUB_WEBHOOK_MAX_RETRIES=8
AHAND_HUB_WEBHOOK_TIMEOUT_MS=5000
```

- [ ] **Step 6: Create secrets env example**

Create `deploy/qisi/env/secrets.env.example` with this content:

```dotenv
AHAND_HUB_SERVICE_TOKEN=
AHAND_HUB_DASHBOARD_PASSWORD=
AHAND_HUB_DEVICE_BOOTSTRAP_TOKEN=
AHAND_HUB_DEVICE_BOOTSTRAP_DEVICE_ID=
AHAND_HUB_JWT_SECRET=
AHAND_HUB_DATABASE_URL=
AHAND_HUB_REDIS_URL=

# Optional webhook integration. If AHAND_HUB_WEBHOOK_URL is set,
# AHAND_HUB_WEBHOOK_SECRET must also be set.
AHAND_HUB_WEBHOOK_URL=
AHAND_HUB_WEBHOOK_SECRET=

# Optional Sentry DSN for the hub service.
SENTRY_DSN=

# Optional Aliyun OSS through the S3-compatible hub path. Leave these commented
# until OSS is ready. An empty AHAND_HUB_S3_BUCKET value still enables S3 config.
# AHAND_HUB_S3_BUCKET=
# AHAND_HUB_S3_REGION=cn-shanghai
# AHAND_HUB_S3_ENDPOINT=https://oss-cn-shanghai.aliyuncs.com
# AHAND_HUB_S3_THRESHOLD_BYTES=1048576
# AHAND_HUB_S3_URL_EXPIRATION_SECS=3600
# AWS_ACCESS_KEY_ID=
# AWS_SECRET_ACCESS_KEY=
```

- [ ] **Step 7: Create Caddy snippets**

Create `deploy/qisi/caddy/dev.Caddyfile` with this content:

```caddyfile
ahand-hub.dev.coffice.qisiai.top {
	reverse_proxy 127.0.0.1:5815
}

admin.ahand.dev.coffice.qisiai.top {
	reverse_proxy 127.0.0.1:5816
}
```

Create `deploy/qisi/caddy/staging.Caddyfile` with this content:

```caddyfile
ahand-hub.staging.coffice.qisiai.top {
	reverse_proxy 127.0.0.1:4815
}

admin.ahand.staging.coffice.qisiai.top {
	reverse_proxy 127.0.0.1:4816
}
```

Create `deploy/qisi/caddy/production.Caddyfile` with this content:

```caddyfile
ahand-hub.coffice.qisiai.top {
	reverse_proxy 127.0.0.1:3815
}

admin.ahand.coffice.qisiai.top {
	reverse_proxy 127.0.0.1:3816
}
```

- [ ] **Step 8: Create healthcheck script**

Create `deploy/qisi/scripts/healthcheck.sh` with this content:

```bash
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
```

- [ ] **Step 9: Create deploy script**

Create `deploy/qisi/scripts/deploy.sh` with this content:

```bash
#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

NEXT_IMAGES="${1:-.env.images.next}"
ACTIVE_IMAGES=".env.images"
COMPOSE_ARGS=(--env-file .env --env-file .env.secrets --env-file .env.images -f compose.yml)
CANDIDATE_COMPOSE=""
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

cp "$NEXT_IMAGES" "$ACTIVE_IMAGES"
PROMOTED=1
trap rollback_on_failure ERR

docker compose "${COMPOSE_ARGS[@]}" up -d --remove-orphans

bash scripts/healthcheck.sh
```

- [ ] **Step 10: Make scripts executable**

Run:

```bash
chmod +x deploy/qisi/scripts/deploy.sh deploy/qisi/scripts/healthcheck.sh
```

Expected: `ls -l deploy/qisi/scripts/*.sh` shows executable bits.

- [ ] **Step 11: Validate shell scripts**

Run:

```bash
bash -n deploy/qisi/scripts/deploy.sh
bash -n deploy/qisi/scripts/healthcheck.sh
```

Expected: both commands exit 0 with no output.

- [ ] **Step 12: Validate Compose config with temp env files**

Run:

```bash
tmpdir="$(mktemp -d)"
cp deploy/qisi/compose.yml "$tmpdir/compose.yml"
cp deploy/qisi/env/dev.env.example "$tmpdir/.env"
cp deploy/qisi/env/secrets.env.example "$tmpdir/.env.secrets"
cat > "$tmpdir/.env.images" <<'EOF'
AHAND_HUB_IMAGE=registry.image.coffice.qisiai.top/coffice/ahand/ahand-hub:dev-test
AHAND_HUB_DASHBOARD_IMAGE=registry.image.coffice.qisiai.top/coffice/ahand/ahand-hub-dashboard:dev-test
GIT_SHA=test
SENTRY_RELEASE=test
EOF
docker compose \
  --env-file "$tmpdir/.env" \
  --env-file "$tmpdir/.env.secrets" \
  --env-file "$tmpdir/.env.images" \
  -f "$tmpdir/compose.yml" \
  config >/dev/null
rm -rf "$tmpdir"
```

Expected: `docker compose config` exits 0. It may warn about unset optional blank values only if the env example is edited incorrectly; the checked-in examples should avoid warnings.

- [ ] **Step 13: Validate Caddy snippets if Caddy is installed**

Run:

```bash
if command -v caddy >/dev/null 2>&1; then
  caddy adapt --config deploy/qisi/caddy/dev.Caddyfile >/dev/null
  caddy adapt --config deploy/qisi/caddy/staging.Caddyfile >/dev/null
  caddy adapt --config deploy/qisi/caddy/production.Caddyfile >/dev/null
else
  echo "caddy not installed locally; snippets will be validated on qisi/qisi-dev"
fi
```

Expected: exit 0. If Caddy is absent, the command prints the skip line and exits 0.

- [ ] **Step 14: Commit runtime files**

Run:

```bash
git add deploy/qisi/compose.yml \
  deploy/qisi/env/dev.env.example \
  deploy/qisi/env/staging.env.example \
  deploy/qisi/env/production.env.example \
  deploy/qisi/env/secrets.env.example \
  deploy/qisi/caddy/dev.Caddyfile \
  deploy/qisi/caddy/staging.Caddyfile \
  deploy/qisi/caddy/production.Caddyfile \
  deploy/qisi/scripts/deploy.sh \
  deploy/qisi/scripts/healthcheck.sh
git commit -m "deploy: add qisi ahand compose runtime"
```

Expected: commit succeeds and includes only `deploy/qisi/**`.

---

### Task 2: Add Qisi Deploy Workflow

**Files:**
- Create: `.github/workflows/qisi-deploy.yml`

- [ ] **Step 1: Create workflow file**

Create `.github/workflows/qisi-deploy.yml` with this content:

```yaml
name: Qisi aHand Deploy

on:
  push:
    branches: [dev, staging, main]
    paths:
      - "apps/hub-dashboard/**"
      - "crates/ahand-hub/**"
      - "crates/ahand-hub-core/**"
      - "crates/ahand-hub-store/**"
      - "crates/ahand-protocol/**"
      - "proto/**"
      - "Cargo.lock"
      - "package.json"
      - "pnpm-lock.yaml"
      - "pnpm-workspace.yaml"
      - "turbo.json"
      - "deploy/hub/Dockerfile"
      - "deploy/qisi/**"
      - ".github/workflows/qisi-deploy.yml"
  workflow_dispatch:

concurrency:
  group: qisi-ahand-deploy-${{ github.ref_name }}
  cancel-in-progress: false

env:
  REGISTRY: registry.image.coffice.qisiai.top
  IMAGE_NAMESPACE: coffice/ahand

jobs:
  resolve-target:
    runs-on: ubuntu-latest
    outputs:
      env_name: ${{ steps.target.outputs.env_name }}
      deploy_dir: ${{ steps.target.outputs.deploy_dir }}
    steps:
      - id: target
        shell: bash
        run: |
          case "${GITHUB_REF_NAME}" in
            dev)
              echo "env_name=dev" >> "$GITHUB_OUTPUT"
              echo "deploy_dir=/opt/ahand-hub/dev" >> "$GITHUB_OUTPUT"
              ;;
            staging)
              echo "env_name=staging" >> "$GITHUB_OUTPUT"
              echo "deploy_dir=/opt/ahand-hub/staging" >> "$GITHUB_OUTPUT"
              ;;
            main)
              echo "env_name=production" >> "$GITHUB_OUTPUT"
              echo "deploy_dir=/opt/ahand-hub/production" >> "$GITHUB_OUTPUT"
              ;;
            *)
              echo "unsupported branch: ${GITHUB_REF_NAME}" >&2
              exit 1
              ;;
          esac

  build-images:
    needs: resolve-target
    runs-on: ubuntu-latest
    permissions:
      contents: read
    strategy:
      fail-fast: false
      matrix:
        include:
          - image: ahand-hub
            target: hub
          - image: ahand-hub-dashboard
            target: dashboard
    steps:
      - uses: actions/checkout@v6

      - uses: docker/setup-buildx-action@v4

      - uses: docker/login-action@v4
        with:
          registry: ${{ env.REGISTRY }}
          username: ${{ secrets.QISI_REGISTRY_USERNAME }}
          password: ${{ secrets.QISI_REGISTRY_PASSWORD }}

      - uses: docker/build-push-action@v7
        with:
          context: .
          file: deploy/hub/Dockerfile
          target: ${{ matrix.target }}
          push: true
          tags: |
            ${{ env.REGISTRY }}/${{ env.IMAGE_NAMESPACE }}/${{ matrix.image }}:${{ needs.resolve-target.outputs.env_name }}-${{ github.sha }}
            ${{ env.REGISTRY }}/${{ env.IMAGE_NAMESPACE }}/${{ matrix.image }}:${{ needs.resolve-target.outputs.env_name }}
          cache-from: type=gha,scope=qisi-ahand-${{ matrix.image }}
          cache-to: type=gha,scope=qisi-ahand-${{ matrix.image }},mode=max

  deploy:
    needs: [resolve-target, build-images]
    runs-on: ubuntu-latest
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@v6

      - name: Resolve SSH host
        shell: bash
        run: |
          case "${GITHUB_REF_NAME}" in
            dev|staging)
              echo "SSH_HOST=${{ secrets.QISI_DEV_SSH_HOST }}" >> "$GITHUB_ENV"
              ;;
            main)
              echo "SSH_HOST=${{ secrets.QISI_SSH_HOST }}" >> "$GITHUB_ENV"
              ;;
            *)
              echo "unsupported branch: ${GITHUB_REF_NAME}" >&2
              exit 1
              ;;
          esac

      - name: Configure SSH
        shell: bash
        run: |
          install -m 700 -d ~/.ssh
          printf '%s\n' "${{ secrets.QISI_SSH_PRIVATE_KEY }}" > ~/.ssh/id_ed25519
          chmod 600 ~/.ssh/id_ed25519
          printf '%s\n' "${{ secrets.QISI_KNOWN_HOSTS }}" > ~/.ssh/known_hosts
          chmod 600 ~/.ssh/known_hosts

      - name: Write image env
        shell: bash
        run: |
          cat > .env.images.next <<EOF
          AHAND_HUB_IMAGE=${{ env.REGISTRY }}/${{ env.IMAGE_NAMESPACE }}/ahand-hub:${{ needs.resolve-target.outputs.env_name }}-${{ github.sha }}
          AHAND_HUB_DASHBOARD_IMAGE=${{ env.REGISTRY }}/${{ env.IMAGE_NAMESPACE }}/ahand-hub-dashboard:${{ needs.resolve-target.outputs.env_name }}-${{ github.sha }}
          GIT_SHA=${{ github.sha }}
          SENTRY_RELEASE=${{ github.sha }}
          EOF

      - name: Sync deploy files
        shell: bash
        run: |
          ssh -i ~/.ssh/id_ed25519 "${{ secrets.QISI_SSH_USER }}@$SSH_HOST" \
            "mkdir -p '${{ needs.resolve-target.outputs.deploy_dir }}/scripts'"
          scp -i ~/.ssh/id_ed25519 deploy/qisi/compose.yml \
            "${{ secrets.QISI_SSH_USER }}@$SSH_HOST:${{ needs.resolve-target.outputs.deploy_dir }}/compose.yml"
          scp -i ~/.ssh/id_ed25519 deploy/qisi/scripts/deploy.sh deploy/qisi/scripts/healthcheck.sh \
            "${{ secrets.QISI_SSH_USER }}@$SSH_HOST:${{ needs.resolve-target.outputs.deploy_dir }}/scripts/"
          scp -i ~/.ssh/id_ed25519 .env.images.next \
            "${{ secrets.QISI_SSH_USER }}@$SSH_HOST:${{ needs.resolve-target.outputs.deploy_dir }}/.env.images.next"

      - name: Deploy on host
        shell: bash
        run: |
          ssh -i ~/.ssh/id_ed25519 "${{ secrets.QISI_SSH_USER }}@$SSH_HOST" \
            "cd '${{ needs.resolve-target.outputs.deploy_dir }}' && bash scripts/deploy.sh .env.images.next"
```

- [ ] **Step 2: Validate workflow syntax with Ruby YAML parser**

Run:

```bash
ruby -e 'require "yaml"; YAML.load_file(".github/workflows/qisi-deploy.yml"); puts "yaml ok"'
```

Expected: prints `yaml ok` and exits 0.

- [ ] **Step 3: Validate workflow with actionlint if available**

Run:

```bash
if command -v actionlint >/dev/null 2>&1; then
  actionlint .github/workflows/qisi-deploy.yml
else
  echo "actionlint not installed; YAML parser check is the local baseline"
fi
```

Expected: exit 0. If actionlint is absent, the command prints the skip line and exits 0.

- [ ] **Step 4: Verify path filters include global hub deploy paths plus qisi files**

Run:

```bash
rg -n '"crates/ahand-hub/\*\*"|"crates/ahand-hub-core/\*\*"|"crates/ahand-hub-store/\*\*"|"crates/ahand-protocol/\*\*"|"proto/\*\*"|"deploy/hub/Dockerfile"|"deploy/qisi/\*\*"' .github/workflows/qisi-deploy.yml
```

Expected: output includes every listed path. This confirms qisi deploys are automatically triggered for the same hub-related update surface as the global deploy path, plus qisi deployment asset changes.

- [ ] **Step 5: Commit workflow**

Run:

```bash
git add .github/workflows/qisi-deploy.yml
git commit -m "ci: deploy ahand hub to qisi"
```

Expected: commit succeeds and includes only `.github/workflows/qisi-deploy.yml`.

---

### Task 3: Run Local Build And Config Verification

**Files:**
- Verify: `deploy/qisi/compose.yml`
- Verify: `deploy/hub/Dockerfile`
- Verify: `.github/workflows/qisi-deploy.yml`

- [ ] **Step 1: Validate qisi Compose config against dev env**

Run:

```bash
tmpdir="$(mktemp -d)"
cp deploy/qisi/compose.yml "$tmpdir/compose.yml"
cp deploy/qisi/env/dev.env.example "$tmpdir/.env"
cp deploy/qisi/env/secrets.env.example "$tmpdir/.env.secrets"
cat > "$tmpdir/.env.images" <<'EOF'
AHAND_HUB_IMAGE=registry.image.coffice.qisiai.top/coffice/ahand/ahand-hub:dev-local
AHAND_HUB_DASHBOARD_IMAGE=registry.image.coffice.qisiai.top/coffice/ahand/ahand-hub-dashboard:dev-local
GIT_SHA=local
SENTRY_RELEASE=local
EOF
docker compose \
  --env-file "$tmpdir/.env" \
  --env-file "$tmpdir/.env.secrets" \
  --env-file "$tmpdir/.env.images" \
  -f "$tmpdir/compose.yml" \
  config >/tmp/ahand-qisi-compose.rendered.yml
rm -rf "$tmpdir"
```

Expected: command exits 0 and `/tmp/ahand-qisi-compose.rendered.yml` exists.

- [ ] **Step 2: Confirm dashboard rendered config does not receive `.env.secrets`**

Run:

```bash
ruby -ryaml -e '
  cfg = YAML.load_file("/tmp/ahand-qisi-compose.rendered.yml")
  dashboard = cfg.fetch("services").fetch("dashboard")
  entries = Array(dashboard.fetch("env_file", [])).map { |entry|
    entry.is_a?(Hash) ? entry.fetch("path") : entry.to_s
  }
  if entries.any? { |entry| entry.include?(".env.secrets") }
    abort "dashboard env_file includes .env.secrets"
  end
  puts "dashboard env_file excludes .env.secrets"
'
```

Expected: prints `dashboard env_file excludes .env.secrets` and exits 0.

- [ ] **Step 3: Build the hub image target**

Run:

```bash
docker build --target hub -f deploy/hub/Dockerfile -t ahand-hub:qisi-smoke .
```

Expected: image builds successfully. This can take several minutes.

- [ ] **Step 4: Build the dashboard image target**

Run:

```bash
docker build --target dashboard -f deploy/hub/Dockerfile -t ahand-hub-dashboard:qisi-smoke .
```

Expected: image builds successfully. This can take several minutes.

- [ ] **Step 5: Re-run existing hub CI checks that are cheap locally**

Run:

```bash
cargo fmt -p ahand-protocol -p ahand-hub-core -p ahand-hub-store -p ahand-hub --check
pnpm --filter @ahand/hub-dashboard lint
```

Expected: both commands exit 0. If dependencies are missing, run `pnpm install --frozen-lockfile` first and retry the dashboard lint.

- [ ] **Step 6: Commit verification note if no code changed**

Do not create a commit in this step. Record the command outputs in the final implementation summary. If any verification command required a source change, create a separate focused commit for that fix after re-running the failing command.

---

### Task 4: Bootstrap Hosts And Validate Remote Prerequisites

**Files:**
- Read: `deploy/qisi/env/dev.env.example`
- Read: `deploy/qisi/env/staging.env.example`
- Read: `deploy/qisi/env/production.env.example`
- Read: `deploy/qisi/env/secrets.env.example`
- Read: `deploy/qisi/caddy/dev.Caddyfile`
- Read: `deploy/qisi/caddy/staging.Caddyfile`
- Read: `deploy/qisi/caddy/production.Caddyfile`

- [ ] **Step 1: Verify qisi-dev prerequisites**

Run:

```bash
ssh qisi-dev 'docker --version && docker compose version && caddy version'
```

Expected: command exits 0 and prints Docker, Docker Compose, and Caddy versions.

- [ ] **Step 2: Verify qisi prerequisites**

Run:

```bash
ssh qisi 'docker --version && docker compose version && caddy version'
```

Expected: command exits 0 and prints Docker, Docker Compose, and Caddy versions.

- [ ] **Step 3: Create qisi-dev directories**

Run:

```bash
ssh qisi-dev 'mkdir -p /opt/ahand-hub/dev/scripts /opt/ahand-hub/staging/scripts'
```

Expected: command exits 0.

- [ ] **Step 4: Create qisi production directory**

Run:

```bash
ssh qisi 'mkdir -p /opt/ahand-hub/production/scripts'
```

Expected: command exits 0.

- [ ] **Step 5: Install non-secret env files if missing**

Run:

```bash
scp deploy/qisi/env/dev.env.example qisi-dev:/opt/ahand-hub/dev/.env
scp deploy/qisi/env/staging.env.example qisi-dev:/opt/ahand-hub/staging/.env
scp deploy/qisi/env/production.env.example qisi:/opt/ahand-hub/production/.env
```

Expected: commands exit 0. If a host already has `.env`, compare first with the exact file, for example `ssh qisi-dev 'cat /opt/ahand-hub/dev/.env'`, and preserve any operator changes.

- [ ] **Step 6: Create host-local secrets files**

Do not copy real secret values into git. On each host, create `.env.secrets` from `deploy/qisi/env/secrets.env.example` and fill the required values:

```bash
scp deploy/qisi/env/secrets.env.example qisi-dev:/opt/ahand-hub/dev/.env.secrets
scp deploy/qisi/env/secrets.env.example qisi-dev:/opt/ahand-hub/staging/.env.secrets
scp deploy/qisi/env/secrets.env.example qisi:/opt/ahand-hub/production/.env.secrets
```

Expected: files exist on the hosts. Before the first deploy, an operator must replace the blank values for `AHAND_HUB_SERVICE_TOKEN`, `AHAND_HUB_DASHBOARD_PASSWORD`, `AHAND_HUB_DEVICE_BOOTSTRAP_TOKEN`, `AHAND_HUB_DEVICE_BOOTSTRAP_DEVICE_ID`, `AHAND_HUB_JWT_SECRET`, `AHAND_HUB_DATABASE_URL`, and `AHAND_HUB_REDIS_URL` with real environment-specific values.

- [ ] **Step 7: Validate Caddy snippets on qisi-dev**

Run:

```bash
scp deploy/qisi/caddy/dev.Caddyfile qisi-dev:/tmp/ahand-dev.Caddyfile
scp deploy/qisi/caddy/staging.Caddyfile qisi-dev:/tmp/ahand-staging.Caddyfile
ssh qisi-dev 'caddy adapt --config /tmp/ahand-dev.Caddyfile >/dev/null && caddy adapt --config /tmp/ahand-staging.Caddyfile >/dev/null'
```

Expected: command exits 0. Then merge the two snippets into `/etc/caddy/Caddyfile`, run `caddy validate --config /etc/caddy/Caddyfile`, and reload Caddy with the host's existing reload command.

- [ ] **Step 8: Validate Caddy snippet on qisi**

Run:

```bash
scp deploy/qisi/caddy/production.Caddyfile qisi:/tmp/ahand-production.Caddyfile
ssh qisi 'caddy adapt --config /tmp/ahand-production.Caddyfile >/dev/null'
```

Expected: command exits 0. Then merge the snippet into `/etc/caddy/Caddyfile`, run `caddy validate --config /etc/caddy/Caddyfile`, and reload Caddy with the host's existing reload command.

- [ ] **Step 9: Confirm no source commit is required for host bootstrap**

Run:

```bash
git status --short
```

Expected: no new source changes from host bootstrap. Host-local `.env` and `.env.secrets` files are not committed.

---

### Task 5: First Deploy And Runtime Verification

**Files:**
- Verify: `.github/workflows/qisi-deploy.yml`
- Verify: `deploy/qisi/scripts/deploy.sh`
- Verify: `deploy/qisi/scripts/healthcheck.sh`

- [ ] **Step 1: Trigger dev deploy**

Push a hub-related change to `dev` or manually run `Qisi aHand Deploy` on `dev` from GitHub Actions.

Expected: workflow runs `resolve-target`, both `build-images` matrix jobs, and `deploy` successfully. The target directory is `/opt/ahand-hub/dev`.

- [ ] **Step 2: Verify dev containers on qisi-dev**

Run:

```bash
ssh qisi-dev 'docker compose --env-file /opt/ahand-hub/dev/.env --env-file /opt/ahand-hub/dev/.env.secrets --env-file /opt/ahand-hub/dev/.env.images -f /opt/ahand-hub/dev/compose.yml ps'
```

Expected: `ahand-hub-dev-hub` and `ahand-hub-dev-dashboard` are running and healthy.

- [ ] **Step 3: Verify dev public URLs**

Run:

```bash
curl -fsS https://ahand-hub.dev.coffice.qisiai.top/api/health
curl -fsS https://admin.ahand.dev.coffice.qisiai.top/login >/tmp/ahand-dev-login.html
```

Expected: health URL returns JSON and dashboard login HTML is saved.

- [ ] **Step 4: Trigger staging deploy**

Push a hub-related change to `staging` or manually run `Qisi aHand Deploy` on `staging` from GitHub Actions.

Expected: workflow succeeds and target directory is `/opt/ahand-hub/staging`.

- [ ] **Step 5: Verify staging public URLs**

Run:

```bash
curl -fsS https://ahand-hub.staging.coffice.qisiai.top/api/health
curl -fsS https://admin.ahand.staging.coffice.qisiai.top/login >/tmp/ahand-staging-login.html
```

Expected: health URL returns JSON and dashboard login HTML is saved.

- [ ] **Step 6: Trigger production deploy**

After dev and staging succeed, merge the qisi deployment branch to `main` or manually run `Qisi aHand Deploy` on `main`.

Expected: workflow succeeds and target directory is `/opt/ahand-hub/production`.

- [ ] **Step 7: Verify production public URLs**

Run:

```bash
curl -fsS https://ahand-hub.coffice.qisiai.top/api/health
curl -fsS https://admin.ahand.coffice.qisiai.top/login >/tmp/ahand-production-login.html
```

Expected: health URL returns JSON and dashboard login HTML is saved.

- [ ] **Step 8: Verify rollback file history exists**

Run:

```bash
ssh qisi-dev 'find /opt/ahand-hub/dev/.deploy-history -maxdepth 1 -type f -name "*.env.images" -print | tail -5 || true'
ssh qisi 'find /opt/ahand-hub/production/.deploy-history -maxdepth 1 -type f -name "*.env.images" -print | tail -5 || true'
```

Expected: after at least one redeploy per environment, history files exist. On the first deploy for a new environment, no history file is expected because no previous `.env.images` existed.

---

### Task 6: Final Repository Verification

**Files:**
- Verify: all files changed by Tasks 1 and 2
- Verify: `docs/superpowers/specs/2026-06-21-qisi-ahand-cn-deployment-design.md`

- [ ] **Step 1: Check changed file scope**

Run:

```bash
git status --short
git log --oneline --max-count=6
```

Expected: only intentional qisi deployment files are modified or committed. Unrelated existing untracked files such as `.vscode/` remain untouched.

- [ ] **Step 2: Search for forbidden placeholders and accidental secrets**

Run:

```bash
rg -n "T[B]D|T[O]DO|F[I]XME|CHANGE[_]ME|password|secret-token|jwt-secret|postgres://[^\\s]*:[^\\s]*@|redis://[^\\s]*:[^\\s]*@" deploy/qisi .github/workflows/qisi-deploy.yml
```

Expected: no output. If the command finds `PASSWORD` or `SECRET` in variable names only, inspect the exact output and ensure no real value is committed.

- [ ] **Step 3: Confirm optional S3 vars are commented in secrets example**

Run:

```bash
rg -n "^AHAND_HUB_S3_|^AWS_ACCESS_KEY_ID=|^AWS_SECRET_ACCESS_KEY=" deploy/qisi/env/secrets.env.example
```

Expected: no output. The optional OSS/S3 keys must remain commented until enabled on the host.

- [ ] **Step 4: Confirm global AWS workflow remains unchanged**

Run:

```bash
git diff -- .github/workflows/deploy-hub.yml deploy/hub/deploy.sh deploy/hub/task-definition.template.json
```

Expected: no output. The qisi deployment is additive and does not alter the global AWS/t9 deployment path.

- [ ] **Step 5: Final commit if any verification-only fixes were needed**

If Task 6 required changes, commit only those fixes:

```bash
git add deploy/qisi .github/workflows/qisi-deploy.yml
git commit -m "fix: harden qisi ahand deployment checks"
```

Expected: commit succeeds only when there are real fixes. If there are no changes, do not create an empty commit.
