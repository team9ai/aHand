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

## Quick Start

### Install

```bash
curl -fsSL https://raw.githubusercontent.com/team9ai/aHand/main/scripts/dist/install.sh | bash
```

This installs `ahandd`, `ahandctl`, the admin panel, and browser setup script to `~/.ahand/`.

Environment variables:
- `AHAND_VERSION` — install a specific version (default: latest)
- `AHAND_DIR` — install directory (default: `~/.ahand`)

### Upgrade

```bash
ahandctl upgrade            # upgrade to latest
ahandctl upgrade --check    # check for updates without installing
```

### Browser Automation Setup

```bash
ahandctl browser-init       # install browser automation dependencies
```

This sets up [agent-browser](https://github.com/AHand-Project/agent-browser), a local Node.js runtime (if needed), and detects/installs Chrome/Chromium.

### Admin Panel

```bash
ahandctl configure          # open the admin panel in browser
```

## Session Modes

The daemon enforces per-caller session modes:

| Mode | Behavior |
|------|----------|
| **Inactive** | Default — rejects all jobs until activated |
| **Strict** | Every command requires manual approval |
| **Trust** | Auto-approve with inactivity timeout (default 60 min) |
| **Auto-Accept** | Auto-approve, no timeout |

## Repository Structure

```
ahand/
├─ proto/ahand/v1/             # Protobuf definitions (single source of truth)
│  ├─ envelope.proto           #   core protocol messages
│  └─ browser.proto            #   browser automation messages
├─ packages/
│  ├─ proto-ts/                # @ahand/proto — ts-proto generated types
│  ├─ sdk/                     # @ahand/sdk — cloud control plane SDK
│  └─ browser-bridge/          # ncc-bundled agent-browser daemon
├─ apps/
│  ├─ admin/                   # Admin panel (Solid.js SPA)
│  ├─ dashboard/               # Dashboard UI (dev mode)
│  └─ dev-cloud/               # Development cloud server (WS + dashboard)
├─ crates/
│  ├─ ahand-protocol/          # Rust prost generated types
│  ├─ ahandd/                  # Local daemon (bin)
│  └─ ahandctl/                # CLI tool (bin)
├─ scripts/
│  └─ dist/                    # Distribution scripts (install, upgrade, setup-browser)
├─ e2e/scripts/                # E2E tests for distribution scripts (BATS)
├─ .github/workflows/          # CI/CD (release-rust, release-admin, release-browser)
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
```

### Test

```bash
pnpm test                   # all tests
pnpm test:ts                # TypeScript tests
pnpm test:rust              # Rust tests
pnpm test:e2e:scripts       # distribution script tests (BATS)
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
```

Each tag triggers a GitHub Actions workflow that builds and publishes to GitHub Releases.

## License

Apache-2.0
