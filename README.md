# AHand

Local execution gateway for cloud AI. Lets cloud-side orchestrators run tools on local machines behind NAT/firewalls via WebSocket, with strong typing (protobuf), local policy enforcement, and browser automation.

## Architecture

```
Cloud (WS server)  ←──  WebSocket (protobuf)  ──→  Local daemon (WS client)
      │                                                    │
  @ahand/sdk                                           ahandd
  (control plane)                                  (job executor)
                                                   ├─ shell / tools
                                                   ├─ browser automation
                                                   └─ policy enforcement
```

- **Cloud** hosts the WebSocket endpoint; local daemon connects outbound (no public IP needed).
- **SDK** (`@ahand/sdk`) accepts upgraded WS connections and provides a typed Job API.
- **Daemon** (`ahandd`) enforces local security policy before executing any job.
- **Admin Panel** — local web UI served by `ahandctl configure` for status, config, logs, and run history.

## Production Control Center

The production hub stack is the operator-facing control plane:

- `ahand-hub` is the Rust API/WebSocket service that owns auth, device state, jobs, and audit logs.
- `ahand-hub-dashboard` is the Next.js 16 dashboard that talks to the hub over HTTP and the dashboard WebSocket.
- PostgreSQL stores durable hub state.
- Redis backs transient queueing and presence data.

The deployment assets live under [`deploy/hub`](deploy/hub). That stack exposes two Docker build targets:

- `hub` for the Rust service image
- `dashboard` for the Next.js dashboard image

The dashboard can be deployed independently first. The compose stack runs the hub and dashboard against externally managed PostgreSQL and Redis endpoints.

## Quick Start

### 1. Install

```bash
curl -fsSL https://raw.githubusercontent.com/team9ai/aHand/main/scripts/dist/install.sh | bash
```

This installs `ahandd`, `ahandctl`, the admin panel, and browser setup script to `~/.ahand/`.

Environment variables:
- `AHAND_VERSION` — install a specific version (default: latest)
- `AHAND_DIR` — install directory (default: `~/.ahand`)

### 2. Configure

```bash
ahandctl configure          # open admin panel in browser to set up config
```

The admin panel guides you through initial setup (connection mode, gateway host, etc.) and writes `~/.ahand/config.toml`.

### 3. Start the Daemon

```bash
ahandctl start              # start daemon in background
ahandctl status             # check if daemon is running
ahandctl stop               # stop the daemon
ahandctl restart             # restart the daemon
```

Logs are written to `~/.ahand/data/daemon.log`.

### Upgrade

```bash
ahandctl upgrade            # upgrade to latest
ahandctl upgrade --check    # check for updates without installing
```

### Browser Automation Setup

```bash
ahandctl browser-init       # install browser automation dependencies
```

This sets up [playwright-cli](https://github.com/microsoft/playwright-cli), a local Node.js runtime (if needed), and detects/installs Chrome/Chromium.

## Session Modes

The daemon enforces per-caller session modes:

| Mode | Behavior |
|------|----------|
| **Inactive** | Default — rejects all jobs until activated |
| **Strict** | Every command requires manual approval |
| **Trust** | Auto-approve with inactivity timeout (default 60 min) |
| **Auto-Accept** | Auto-approve, no timeout |

## File Operations

The daemon exposes a 14-operation file API (read text/binary/image, write, edit, delete, stat, list, glob, mkdir, copy, move, create_symlink, chmod) gated by a `[file_policy]` block in `~/.ahand/config.toml`:

```toml
[file_policy]
enabled = true
path_allowlist  = ["~/projects/**", "/workspace/**"]
path_denylist   = ["**/.git/**", "**/node_modules/**"]
dangerous_paths = ["~/.ssh/**", "/etc/passwd"]
max_read_bytes  = 104857600  # 100 MB
max_write_bytes = 10485760   # 10 MB
```

`dangerous_paths` matches escalate to STRICT-mode approval. Allowlist patterns support `~` expansion (fail-loud if `HOME` is unavailable) and glob (`**`, `?`). The hub forwards via `POST /api/devices/{device_id}/files` with admission control. See `proto/ahand/v1/file_ops.proto` for the wire format.

## Repository Structure

```
ahand/
├─ proto/ahand/v1/             # Protobuf definitions (single source of truth)
│  ├─ envelope.proto           #   core protocol messages
│  ├─ browser.proto            #   browser automation messages
│  └─ file_ops.proto           #   file operation messages (read/write/edit/delete/list/glob/copy/move/symlink/chmod)
├─ packages/
│  ├─ proto-ts/                # @ahand/proto — ts-proto generated types
│  └─ sdk/                     # @ahand/sdk — cloud control plane SDK
├─ apps/
│  ├─ admin/                   # Admin panel (Solid.js SPA)
│  ├─ hub-dashboard/           # Production Control Center dashboard (Next.js)
│  ├─ dashboard/               # Dashboard UI (dev mode)
│  └─ dev-cloud/               # Development cloud server (WS + dashboard)
├─ crates/
│  ├─ ahand-protocol/          # Rust prost generated types
│  ├─ ahand-hub/               # Production control plane HTTP/WebSocket server
│  ├─ ahand-hub-core/          # Hub domain logic
│  ├─ ahand-hub-store/         # Hub persistence adapters
│  ├─ ahandd/                  # Local daemon (bin)
│  └─ ahandctl/                # CLI tool (bin)
├─ deploy/
│  └─ hub/                     # Hub Dockerfile + compose stack
├─ scripts/
│  └─ dist/                    # Distribution scripts (install, upgrade, setup-browser)
├─ e2e/scripts/                # E2E tests for distribution scripts (BATS)
├─ .github/workflows/          # CI/CD (hub-ci, release-rust, release-admin, release-browser, release-hub)
├─ turbo.json                  # Turborepo pipeline
├─ Cargo.toml                  # Rust workspace
└─ pnpm-workspace.yaml         # pnpm monorepo
```

## Development

### Prerequisites

- Node.js >= 20, pnpm >= 10
- Rust (edition 2024)
- protoc (Protocol Buffers compiler)

### Build

```bash
pnpm install                # install TS dependencies
pnpm build                  # build all TS packages (turbo)
cargo check                 # check Rust workspace
```

### Dev

```bash
pnpm dev                    # start dashboard + dev-cloud + daemon
pnpm dev:admin              # admin panel only
pnpm dev:cloud              # cloud server + dashboard
pnpm dev:daemon             # daemon only (watch mode)
pnpm dev:hub-dashboard      # production dashboard only
```

### Test

```bash
pnpm test                   # all tests
pnpm test:ts                # TypeScript tests
pnpm test:rust              # Rust tests
pnpm test:hub-dashboard     # hub dashboard tests
pnpm test:e2e:scripts       # distribution script tests (BATS)

# Persistent store roundtrip against disposable Postgres + Redis
cargo test -p ahand-hub-store --features test-support --test store_roundtrip
```

### Release Build

```bash
bash scripts/release.sh     # local release build to release/
```

## Release

Per-component versioning via git tags:

```bash
git tag rust-v0.2.0 && git push origin rust-v0.2.0       # daemon + CLI
git tag admin-v0.2.0 && git push origin admin-v0.2.0     # admin panel
git tag browser-v0.2.0 && git push origin browser-v0.2.0 # browser bundle
git tag hub-v0.2.0 && git push origin hub-v0.2.0         # production control center images
```

Each tag triggers a GitHub Actions workflow that builds and publishes the relevant release artifacts. The hub release workflow pushes the `ahand-hub` and `ahand-hub-dashboard` images to GitHub Container Registry.

`release-hub.yml` is tag-provenance preserving by default:

- `git push origin hub-vX.Y.Z` builds from that pushed tag
- `workflow_dispatch` is only for rebuilding an existing `hub-v*` tag, not for publishing an arbitrary branch under a release tag

### Hub deployment

To validate the hub stack against external PostgreSQL and Redis endpoints:

```bash
export AHAND_HUB_SERVICE_TOKEN=dev-service-token
export AHAND_HUB_DASHBOARD_PASSWORD=dev-dashboard-password
export AHAND_HUB_DEVICE_BOOTSTRAP_TOKEN=dev-bootstrap-token
export AHAND_HUB_DEVICE_BOOTSTRAP_DEVICE_ID=device-dev-1
export AHAND_HUB_DASHBOARD_ALLOWED_ORIGINS=http://127.0.0.1:3100
export AHAND_HUB_JWT_SECRET=dev-jwt-secret
export AHAND_HUB_DATABASE_URL=postgres://ahand_hub:secret@db.example.internal:5432/ahand_hub
export AHAND_HUB_REDIS_URL=redis://cache.example.internal:6379
export AHAND_HUB_AUDIT_FALLBACK_PATH=/var/lib/ahand-hub/audit-fallback.jsonl
docker compose -f deploy/hub/docker-compose.yml up --build
```

The compose file starts the hub and dashboard containers only. PostgreSQL and Redis remain external dependencies that must already be reachable at the configured URLs.
If the dashboard is served from a different browser origin than the hub, set `AHAND_HUB_DASHBOARD_ALLOWED_ORIGINS` on the hub to the public dashboard origin list.

For a local smoke environment, build local images, provision disposable external dependencies, wait for them to accept connections, and then run the hub and dashboard containers on the same network:

```bash
docker build -t ahand-hub:local --target hub -f deploy/hub/Dockerfile .
docker build -t ahand-hub-dashboard:local --target dashboard -f deploy/hub/Dockerfile .

docker network create ahand-hub-smoke
docker run -d --rm --network ahand-hub-smoke --name ahand-hub-postgres \
  -e POSTGRES_DB=ahand_hub \
  -e POSTGRES_USER=ahand_hub \
  -e POSTGRES_PASSWORD=ahand_hub \
  postgres:16-alpine
docker run -d --rm --network ahand-hub-smoke --name ahand-hub-redis redis:7-alpine
until docker exec ahand-hub-postgres pg_isready -U ahand_hub -d ahand_hub; do sleep 1; done
until docker exec ahand-hub-redis redis-cli ping; do sleep 1; done

docker run -d --rm --network ahand-hub-smoke --name ahand-hub \
  -p 18080:8080 \
  -e AHAND_HUB_BIND_ADDR=0.0.0.0:8080 \
  -e AHAND_HUB_SERVICE_TOKEN=dev-service-token \
  -e AHAND_HUB_DASHBOARD_PASSWORD=dev-dashboard-password \
  -e AHAND_HUB_DEVICE_BOOTSTRAP_TOKEN=dev-bootstrap-token \
  -e AHAND_HUB_DEVICE_BOOTSTRAP_DEVICE_ID=device-dev-1 \
  -e AHAND_HUB_DASHBOARD_ALLOWED_ORIGINS=http://127.0.0.1:13100 \
  -e AHAND_HUB_JWT_SECRET=dev-jwt-secret \
  -e AHAND_HUB_DATABASE_URL=postgres://ahand_hub:ahand_hub@ahand-hub-postgres:5432/ahand_hub \
  -e AHAND_HUB_REDIS_URL=redis://ahand-hub-redis:6379 \
  -e AHAND_HUB_AUDIT_FALLBACK_PATH=/var/lib/ahand-hub/audit-fallback.jsonl \
  -v ahand-hub-audit:/var/lib/ahand-hub \
  ahand-hub:local

docker run -d --rm --network ahand-hub-smoke --name ahand-hub-dashboard \
  -p 13100:3100 \
  -e AHAND_HUB_BASE_URL=http://ahand-hub:8080 \
  ahand-hub-dashboard:local

curl -fsS http://127.0.0.1:18080/api/health
curl -fsS http://127.0.0.1:13100/login >/dev/null
curl -fsS \
  -c /tmp/ahand-hub-dashboard.cookies \
  -H 'content-type: application/json' \
  -d '{"password":"dev-dashboard-password"}' \
  http://127.0.0.1:13100/api/auth/login >/tmp/ahand-hub-dashboard-login.json
TOKEN=$(awk '$6 == "ahand_hub_session" { print $7 }' /tmp/ahand-hub-dashboard.cookies)
if [ -z "$TOKEN" ]; then
  cat /tmp/ahand-hub-dashboard-login.json >&2
  exit 1
fi
curl -fsS \
  -b /tmp/ahand-hub-dashboard.cookies \
  http://127.0.0.1:13100/api/proxy/api/auth/verify
DASHBOARD_SESSION="$TOKEN" node --input-type=module <<'EOF'
import http from "node:http";

const request = http.request("http://127.0.0.1:13100/ws/dashboard", {
  headers: {
    Connection: "Upgrade",
    Upgrade: "websocket",
    "Sec-WebSocket-Version": "13",
    "Sec-WebSocket-Key": "YWhhbmQtaHViLXNtb2tlIQ==",
    Origin: "http://127.0.0.1:13100",
    Cookie: `ahand_hub_session=${process.env.DASHBOARD_SESSION ?? ""}`,
  },
});

const timeout = setTimeout(() => {
  console.error("dashboard websocket handshake timed out");
  request.destroy(new Error("dashboard websocket timeout"));
}, 5_000);

request.on("upgrade", (_response, socket) => {
  clearTimeout(timeout);
  socket.destroy();
  process.exit(0);
});

request.on("response", (response) => {
  clearTimeout(timeout);
  console.error(`unexpected dashboard websocket response: ${response.statusCode}`);
  process.exit(1);
});

request.on("error", (error) => {
  clearTimeout(timeout);
  console.error(error.message);
  process.exit(1);
});

request.end();
EOF
```

## License

Apache-2.0
