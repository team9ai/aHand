# aHand Sentry SDK Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire real automatic Sentry error capture into the Rust hub and Next.js hub dashboard, then deploy the qisi runtime with separate hub and dashboard Sentry DSNs.

**Architecture:** Add a focused Rust observability module that initializes Sentry before the hub runtime and attaches a Sentry tracing layer for error-level events. Add official `@sentry/nextjs` App Router instrumentation files to the dashboard, driven by a tested config helper. Update Qisi Compose so hub and dashboard read separate DSNs from `.env.secrets`.

**Tech Stack:** Rust, Tokio, `tracing_subscriber`, Sentry Rust SDK `0.46.2`, Next.js 16 App Router, `@sentry/nextjs` `10.42.0`, Vitest, Docker Compose, GitHub Actions, qisi-infra Sentry.

---

## File Structure

- Modify `Cargo.toml`: add workspace `sentry` dependency with the `tracing` feature.
- Modify `crates/ahand-hub/Cargo.toml`: use workspace `sentry`.
- Create `crates/ahand-hub/src/observability.rs`: parse Sentry env and initialize the SDK.
- Modify `crates/ahand-hub/src/main.rs`: initialize Sentry, attach tracing layer, and run Tokio from synchronous `main`.
- Modify `crates/ahand-hub/src/http/system.rs`: add token-gated hub Sentry smoke route.
- Modify `crates/ahand-hub/src/http/mod.rs`: register the hub Sentry smoke route.
- Modify `apps/hub-dashboard/package.json`: add `@sentry/nextjs`.
- Create `apps/hub-dashboard/src/lib/sentry-config.ts`: dashboard Sentry option helpers.
- Create `apps/hub-dashboard/src/lib/sentry-config.test.ts`: config helper tests.
- Create `apps/hub-dashboard/src/instrumentation-client.ts`: browser SDK init.
- Create `apps/hub-dashboard/src/sentry.server.config.ts`: server SDK init.
- Create `apps/hub-dashboard/src/sentry.edge.config.ts`: edge SDK init.
- Create `apps/hub-dashboard/src/instrumentation.ts`: Next runtime registration and request-error capture.
- Create `apps/hub-dashboard/src/app/global-error.tsx`: App Router render error capture.
- Create `apps/hub-dashboard/src/app/api/sentry-smoke/route.ts`: token-gated dashboard request-error smoke route.
- Modify `apps/hub-dashboard/src/middleware.ts`: allow the dashboard smoke route to reach its token gate.
- Modify `apps/hub-dashboard/next.config.ts`: wrap existing config with `withSentryConfig`.
- Modify `deploy/qisi/compose.yml`: pass dashboard Sentry DSN variables.
- Modify `deploy/qisi/env/secrets.env.example`: document `AHAND_HUB_DASHBOARD_SENTRY_DSN`.
- Modify `.github/workflows/qisi-deploy.yml` only if Compose validation requires extra sample env values.

---

### Task 1: Prepare the Branch and Baseline

**Files:**
- No source changes.

- [ ] **Step 1: Confirm isolated worktree**

Run:

```bash
git rev-parse --git-dir --git-common-dir --show-superproject-working-tree 2>/dev/null
git branch --show-current
git status --short
```

Expected:

```text
/Users/winrey/Projects/weightwave/aHand/.git/worktrees/qisi-sentry-sdk-integration
/Users/winrey/Projects/weightwave/aHand/.git
codex/qisi-sentry-sdk-integration
```

`git status --short` must be empty before implementation starts.

- [ ] **Step 2: Install or verify dependencies**

Run:

```bash
pnpm install --frozen-lockfile
cargo fetch
```

Expected: both commands exit 0.

- [ ] **Step 3: Run targeted baseline checks**

Run:

```bash
cargo test -p ahand-hub audit_writer::tests::buffered_store_falls_back_to_file_after_store_failure -- --exact
pnpm --filter @ahand/hub-dashboard test
```

Expected: Rust targeted test and existing dashboard test suite pass.

---

### Task 2: Add Hub Sentry Config Tests

**Files:**
- Create: `crates/ahand-hub/src/observability.rs`
- Modify: `crates/ahand-hub/src/main.rs`

- [ ] **Step 1: Add failing tests and module stub**

Create `crates/ahand-hub/src/observability.rs` with this initial content:

```rust
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct HubSentryConfig {
    pub dsn: String,
    pub environment: Option<String>,
    pub release: Option<String>,
}

pub fn hub_sentry_config_from_lookup<F>(_lookup: F) -> Option<HubSentryConfig>
where
    F: Fn(&str) -> Option<String>,
{
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(values: HashMap<&'static str, &'static str>) -> impl Fn(&str) -> Option<String> {
        move |key| values.get(key).map(|value| (*value).to_owned())
    }

    #[test]
    fn empty_dsn_disables_sentry() {
        let values = HashMap::from([("SENTRY_DSN", "")]);

        let config = hub_sentry_config_from_lookup(lookup(values));

        assert_eq!(config, None);
    }

    #[test]
    fn reads_dsn_environment_and_release() {
        let values = HashMap::from([
            ("SENTRY_DSN", "https://public@example.invalid/1"),
            ("SENTRY_ENVIRONMENT", "production"),
            ("SENTRY_RELEASE", "abc123"),
            ("GIT_SHA", "ignored"),
        ]);

        let config = hub_sentry_config_from_lookup(lookup(values)).expect("config");

        assert_eq!(config.dsn, "https://public@example.invalid/1");
        assert_eq!(config.environment.as_deref(), Some("production"));
        assert_eq!(config.release.as_deref(), Some("abc123"));
    }

    #[test]
    fn falls_back_to_git_sha_for_release() {
        let values = HashMap::from([
            ("SENTRY_DSN", "https://public@example.invalid/1"),
            ("SENTRY_RELEASE", ""),
            ("GIT_SHA", "git-sha"),
        ]);

        let config = hub_sentry_config_from_lookup(lookup(values)).expect("config");

        assert_eq!(config.release.as_deref(), Some("git-sha"));
    }
}
```

Add this module line to `crates/ahand-hub/src/main.rs` temporarily:

```rust
mod observability;
```

- [ ] **Step 2: Verify RED**

Run:

```bash
cargo test -p ahand-hub observability::tests -- --nocapture
```

Expected: at least `reads_dsn_environment_and_release` fails because `hub_sentry_config_from_lookup` returns `None`.

---

### Task 3: Implement Hub Sentry Initialization

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/ahand-hub/Cargo.toml`
- Modify: `crates/ahand-hub/src/observability.rs`
- Modify: `crates/ahand-hub/src/main.rs`

- [ ] **Step 1: Add Rust Sentry dependencies**

Modify workspace dependencies in `Cargo.toml`:

```toml
sentry = { version = "0.46.2", features = ["tracing"] }
```

Modify `crates/ahand-hub/Cargo.toml` dependencies:

```toml
sentry.workspace = true
```

- [ ] **Step 2: Implement config parsing and SDK init**

Replace `crates/ahand-hub/src/observability.rs` with:

```rust
use std::borrow::Cow;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct HubSentryConfig {
    pub dsn: String,
    pub environment: Option<String>,
    pub release: Option<String>,
}

pub fn hub_sentry_config_from_env() -> Option<HubSentryConfig> {
    hub_sentry_config_from_lookup(|key| std::env::var(key).ok())
}

pub fn hub_sentry_config_from_lookup<F>(lookup: F) -> Option<HubSentryConfig>
where
    F: Fn(&str) -> Option<String>,
{
    let dsn = non_empty(lookup("SENTRY_DSN"))?;
    let environment = non_empty(lookup("SENTRY_ENVIRONMENT"));
    let release = non_empty(lookup("SENTRY_RELEASE")).or_else(|| non_empty(lookup("GIT_SHA")));

    Some(HubSentryConfig {
        dsn,
        environment,
        release,
    })
}

pub fn init_sentry(config: Option<HubSentryConfig>) -> Option<sentry::ClientInitGuard> {
    let config = config?;
    let release = config
        .release
        .map(Cow::Owned)
        .or_else(|| sentry::release_name!());

    Some(sentry::init((
        config.dsn,
        sentry::ClientOptions {
            release,
            environment: config.environment.map(Cow::Owned),
            send_default_pii: false,
            ..Default::default()
        },
    )))
}

fn non_empty(value: Option<String>) -> Option<String> {
    let value = value?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(values: HashMap<&'static str, &'static str>) -> impl Fn(&str) -> Option<String> {
        move |key| values.get(key).map(|value| (*value).to_owned())
    }

    #[test]
    fn empty_dsn_disables_sentry() {
        let values = HashMap::from([("SENTRY_DSN", "")]);

        let config = hub_sentry_config_from_lookup(lookup(values));

        assert_eq!(config, None);
    }

    #[test]
    fn reads_dsn_environment_and_release() {
        let values = HashMap::from([
            ("SENTRY_DSN", "https://public@example.invalid/1"),
            ("SENTRY_ENVIRONMENT", "production"),
            ("SENTRY_RELEASE", "abc123"),
            ("GIT_SHA", "ignored"),
        ]);

        let config = hub_sentry_config_from_lookup(lookup(values)).expect("config");

        assert_eq!(config.dsn, "https://public@example.invalid/1");
        assert_eq!(config.environment.as_deref(), Some("production"));
        assert_eq!(config.release.as_deref(), Some("abc123"));
    }

    #[test]
    fn falls_back_to_git_sha_for_release() {
        let values = HashMap::from([
            ("SENTRY_DSN", "https://public@example.invalid/1"),
            ("SENTRY_RELEASE", ""),
            ("GIT_SHA", "git-sha"),
        ]);

        let config = hub_sentry_config_from_lookup(lookup(values)).expect("config");

        assert_eq!(config.release.as_deref(), Some("git-sha"));
    }
}
```

- [ ] **Step 3: Restructure hub main**

Replace `crates/ahand-hub/src/main.rs` with:

```rust
mod observability;

use ahand_hub::{build_app, config::Config, state::AppState};
use observability::{hub_sentry_config_from_env, init_sentry};
use sentry::integrations::tracing::EventFilter;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

fn main() -> anyhow::Result<()> {
    let sentry = init_sentry(hub_sentry_config_from_env());
    init_tracing(sentry.is_some());

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async_main())
}

async fn async_main() -> anyhow::Result<()> {
    tracing::info!(
        git_sha = std::env::var("GIT_SHA").as_deref().unwrap_or("unknown"),
        "ahand-hub starting"
    );

    let config = Config::from_env()?;
    let bind_addr = config.bind_addr.clone();

    tracing::info!(bind_addr = %bind_addr, "config loaded; connecting to backing services");
    let state = AppState::from_config(config).await?;

    tracing::info!("backing services connected; binding listener");
    let app = build_app(state.clone());
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;

    tracing::info!(bind_addr = %bind_addr, "ahand-hub listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    state.shutdown().await?;
    Ok(())
}

fn init_tracing(sentry_enabled: bool) {
    let level = std::env::var("AHAND_HUB_LOG_LEVEL").unwrap_or_else(|_| "info".into());
    let filter = EnvFilter::try_new(&level).unwrap_or_else(|_| EnvFilter::new("info"));
    let format = std::env::var("AHAND_HUB_LOG_FORMAT").unwrap_or_default();
    let sentry_layer = sentry::integrations::tracing::layer().event_filter(|metadata| {
        if *metadata.level() == tracing::Level::ERROR {
            EventFilter::Event
        } else {
            EventFilter::Ignore
        }
    });

    if format.eq_ignore_ascii_case("json") {
        let subscriber = tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer().json());
        if sentry_enabled {
            subscriber.with(sentry_layer).init();
        } else {
            subscriber.init();
        }
    } else {
        let subscriber = tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer());
        if sentry_enabled {
            subscriber.with(sentry_layer).init();
        } else {
            subscriber.init();
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        let _ = signal.recv().await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}
```

- [ ] **Step 4: Verify GREEN**

Run:

```bash
cargo test -p ahand-hub observability::tests -- --nocapture
cargo check -p ahand-hub
```

Expected: tests pass and `cargo check` exits 0.

- [ ] **Step 5: Commit hub SDK setup**

Run:

```bash
git add Cargo.toml Cargo.lock crates/ahand-hub/Cargo.toml crates/ahand-hub/src/main.rs crates/ahand-hub/src/observability.rs
git commit -m "feat: initialize sentry for hub"
```

---

### Task 4: Add Hub Sentry Smoke Route

**Files:**
- Modify: `crates/ahand-hub/src/http/system.rs`
- Modify: `crates/ahand-hub/src/http/mod.rs`

- [ ] **Step 1: Add hub smoke route**

In `crates/ahand-hub/src/http/system.rs`, change imports from:

```rust
use axum::{Json, extract::State};
```

to:

```rust
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
```

Then append this handler:

```rust
pub async fn sentry_smoke(headers: HeaderMap) -> ApiResult<Json<HealthResponse>> {
    let expected = std::env::var("AHAND_HUB_SENTRY_SMOKE_TOKEN")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let Some(expected) = expected else {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "NOT_FOUND", "Not found"));
    };

    let actual = headers
        .get("x-ahand-sentry-smoke-token")
        .and_then(|value| value.to_str().ok());
    if actual != Some(expected.as_str()) {
        return Err(ApiError::forbidden());
    }

    tracing::error!(
        target: "ahand_hub_sentry_smoke",
        smoke = true,
        "aHand hub Sentry smoke test"
    );
    Ok(Json(HealthResponse { ok: true }))
}
```

- [ ] **Step 2: Register hub smoke route**

In `crates/ahand-hub/src/http/mod.rs`, add the route after `/api/health`:

```rust
        .route("/api/system/sentry-smoke", post(system::sentry_smoke))
```

- [ ] **Step 3: Run hub route checks**

Run:

```bash
cargo test -p ahand-hub observability::tests -- --nocapture
cargo check -p ahand-hub
```

Expected: both commands exit 0.

- [ ] **Step 4: Commit hub smoke route**

```bash
git add crates/ahand-hub/src/http/system.rs crates/ahand-hub/src/http/mod.rs
git commit -m "test: add hub sentry smoke route"
```

---

### Task 5: Add Dashboard Sentry Config Tests

**Files:**
- Create: `apps/hub-dashboard/src/lib/sentry-config.ts`
- Create: `apps/hub-dashboard/src/lib/sentry-config.test.ts`

- [ ] **Step 1: Add failing helper tests**

Create `apps/hub-dashboard/src/lib/sentry-config.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { getBrowserSentryOptions, getServerSentryOptions } from "./sentry-config";

describe("dashboard Sentry config", () => {
  it("disables browser Sentry without a public DSN", () => {
    expect(getBrowserSentryOptions({})).toBeNull();
  });

  it("builds browser options from public env", () => {
    expect(
      getBrowserSentryOptions({
        NEXT_PUBLIC_SENTRY_DSN: "https://public@example.invalid/2",
        SENTRY_ENVIRONMENT: "production",
        SENTRY_RELEASE: "abc123",
      }),
    ).toMatchObject({
      dsn: "https://public@example.invalid/2",
      environment: "production",
      release: "abc123",
      sendDefaultPii: false,
      tracesSampleRate: 0,
    });
  });

  it("prefers server DSN and falls back to public DSN for server runtime", () => {
    expect(
      getServerSentryOptions({
        SENTRY_DSN: "https://server@example.invalid/2",
        NEXT_PUBLIC_SENTRY_DSN: "https://public@example.invalid/2",
      }),
    )?.dsn).toBe("https://server@example.invalid/2");

    expect(
      getServerSentryOptions({
        NEXT_PUBLIC_SENTRY_DSN: "https://public@example.invalid/2",
      }),
    )?.dsn).toBe("https://public@example.invalid/2");
  });
});
```

Create `apps/hub-dashboard/src/lib/sentry-config.ts` with a stub:

```ts
export type SentryEnv = Record<string, string | undefined>;

export type DashboardSentryOptions = {
  dsn: string;
  environment?: string;
  release?: string;
  sendDefaultPii: false;
  tracesSampleRate: 0;
};

export function getBrowserSentryOptions(_env: SentryEnv): DashboardSentryOptions | null {
  return null;
}

export function getServerSentryOptions(_env: SentryEnv): DashboardSentryOptions | null {
  return null;
}
```

- [ ] **Step 2: Verify RED**

Run:

```bash
pnpm --filter @ahand/hub-dashboard test -- src/lib/sentry-config.test.ts
```

Expected: tests fail because non-empty DSNs still return `null`.

---

### Task 6: Implement Dashboard Sentry Config and SDK Files

**Files:**
- Modify: `apps/hub-dashboard/package.json`
- Modify: `pnpm-lock.yaml`
- Modify: `apps/hub-dashboard/src/lib/sentry-config.ts`
- Create: `apps/hub-dashboard/src/instrumentation-client.ts`
- Create: `apps/hub-dashboard/src/sentry.server.config.ts`
- Create: `apps/hub-dashboard/src/sentry.edge.config.ts`
- Create: `apps/hub-dashboard/src/instrumentation.ts`
- Create: `apps/hub-dashboard/src/app/global-error.tsx`
- Create: `apps/hub-dashboard/src/app/api/sentry-smoke/route.ts`
- Modify: `apps/hub-dashboard/src/middleware.ts`
- Modify: `apps/hub-dashboard/next.config.ts`

- [ ] **Step 1: Add `@sentry/nextjs`**

Run:

```bash
pnpm --filter @ahand/hub-dashboard add @sentry/nextjs@10.42.0
```

Expected: `apps/hub-dashboard/package.json` and `pnpm-lock.yaml` change.

- [ ] **Step 2: Implement dashboard config helper**

Replace `apps/hub-dashboard/src/lib/sentry-config.ts` with:

```ts
export type SentryEnv = Record<string, string | undefined>;

export type DashboardSentryOptions = {
  dsn: string;
  environment?: string;
  release?: string;
  sendDefaultPii: false;
  tracesSampleRate: 0;
};

export function getBrowserSentryOptions(env: SentryEnv): DashboardSentryOptions | null {
  const dsn = nonEmpty(env.NEXT_PUBLIC_SENTRY_DSN);
  if (!dsn) {
    return null;
  }
  return buildOptions(dsn, env);
}

export function getServerSentryOptions(env: SentryEnv): DashboardSentryOptions | null {
  const dsn = nonEmpty(env.SENTRY_DSN) ?? nonEmpty(env.NEXT_PUBLIC_SENTRY_DSN);
  if (!dsn) {
    return null;
  }
  return buildOptions(dsn, env);
}

function buildOptions(dsn: string, env: SentryEnv): DashboardSentryOptions {
  return {
    dsn,
    environment: nonEmpty(env.SENTRY_ENVIRONMENT),
    release: nonEmpty(env.SENTRY_RELEASE),
    sendDefaultPii: false,
    tracesSampleRate: 0,
  };
}

function nonEmpty(value: string | undefined): string | undefined {
  const trimmed = value?.trim();
  return trimmed ? trimmed : undefined;
}
```

- [ ] **Step 3: Verify GREEN for config helper**

Run:

```bash
pnpm --filter @ahand/hub-dashboard test -- src/lib/sentry-config.test.ts
```

Expected: tests pass.

- [ ] **Step 4: Add client SDK initialization**

Create `apps/hub-dashboard/src/instrumentation-client.ts`:

```ts
import * as Sentry from "@sentry/nextjs";
import { getBrowserSentryOptions } from "@/lib/sentry-config";

const options = getBrowserSentryOptions(process.env);

if (options) {
  Sentry.init(options);
}
```

- [ ] **Step 5: Add server SDK initialization**

Create `apps/hub-dashboard/src/sentry.server.config.ts`:

```ts
import * as Sentry from "@sentry/nextjs";
import { getServerSentryOptions } from "@/lib/sentry-config";

const options = getServerSentryOptions(process.env);

if (options) {
  Sentry.init(options);
}
```

- [ ] **Step 6: Add edge SDK initialization**

Create `apps/hub-dashboard/src/sentry.edge.config.ts`:

```ts
import * as Sentry from "@sentry/nextjs";
import { getServerSentryOptions } from "@/lib/sentry-config";

const options = getServerSentryOptions(process.env);

if (options) {
  Sentry.init(options);
}
```

- [ ] **Step 7: Add Next instrumentation registration**

Create `apps/hub-dashboard/src/instrumentation.ts`:

```ts
import * as Sentry from "@sentry/nextjs";

export async function register() {
  if (process.env.NEXT_RUNTIME === "nodejs") {
    await import("./sentry.server.config");
  }

  if (process.env.NEXT_RUNTIME === "edge") {
    await import("./sentry.edge.config");
  }
}

export const onRequestError = Sentry.captureRequestError;
```

- [ ] **Step 8: Add App Router global error capture**

Create `apps/hub-dashboard/src/app/global-error.tsx`:

```tsx
"use client";

import * as Sentry from "@sentry/nextjs";
import NextError from "next/error";
import { useEffect } from "react";

export default function GlobalError({ error }: { error: Error & { digest?: string } }) {
  useEffect(() => {
    Sentry.captureException(error);
  }, [error]);

  return (
    <html lang="en">
      <body>
        <NextError statusCode={0} />
      </body>
    </html>
  );
}
```

- [ ] **Step 9: Wrap Next config with Sentry**

Replace `apps/hub-dashboard/next.config.ts` with:

```ts
import path from "node:path";
import type { NextConfig } from "next";
import { withSentryConfig } from "@sentry/nextjs";

const nextConfig: NextConfig = {
  reactStrictMode: true,
  turbopack: {
    root: path.join(__dirname, "../.."),
  },
};

export default withSentryConfig(nextConfig, {
  org: process.env.SENTRY_ORG ?? "sentry",
  project: process.env.SENTRY_PROJECT ?? "ahand-dashboard",
  silent: !process.env.CI,
  disableLogger: true,
});
```

- [ ] **Step 10: Add dashboard request-error smoke route**

Create `apps/hub-dashboard/src/app/api/sentry-smoke/route.ts`:

```ts
import { NextRequest } from "next/server";

export async function POST(request: NextRequest) {
  const expected = process.env.AHAND_HUB_DASHBOARD_SENTRY_SMOKE_TOKEN?.trim();
  if (!expected) {
    return new Response("Not found", { status: 404 });
  }

  const actual = request.headers.get("x-ahand-sentry-smoke-token");
  if (actual !== expected) {
    return new Response("Forbidden", { status: 403 });
  }

  throw new Error("aHand dashboard Sentry smoke test");
}
```

- [ ] **Step 11: Allow dashboard smoke route through middleware**

In `apps/hub-dashboard/src/middleware.ts`, add this branch after the dashboard WebSocket branch and before the session lookup:

```ts
  if (request.nextUrl.pathname === "/api/sentry-smoke") {
    return NextResponse.next();
  }
```

- [ ] **Step 12: Run dashboard checks**

Run:

```bash
pnpm --filter @ahand/hub-dashboard test
pnpm --filter @ahand/hub-dashboard lint
pnpm --filter @ahand/hub-dashboard build
```

Expected: all commands exit 0.

- [ ] **Step 13: Commit dashboard SDK setup**

Run:

```bash
git add apps/hub-dashboard package.json pnpm-lock.yaml
git commit -m "feat: initialize sentry for hub dashboard"
```

---

### Task 7: Update Qisi Runtime Configuration

**Files:**
- Modify: `deploy/qisi/compose.yml`
- Modify: `deploy/qisi/env/secrets.env.example`

- [ ] **Step 1: Update dashboard Compose env**

In `deploy/qisi/compose.yml`, change the dashboard service `env_file` from:

```yaml
    env_file:
      - .env
      - .env.images
```

to:

```yaml
    env_file:
      - .env
      - .env.secrets
      - .env.images
```

Then add these entries under `dashboard.environment`:

```yaml
      SENTRY_DSN: ${AHAND_HUB_DASHBOARD_SENTRY_DSN:-}
      NEXT_PUBLIC_SENTRY_DSN: ${AHAND_HUB_DASHBOARD_SENTRY_DSN:-}
      SENTRY_ORG: ${SENTRY_ORG:-sentry}
      SENTRY_PROJECT: ${SENTRY_DASHBOARD_PROJECT:-ahand-dashboard}
```

Keep the existing `SENTRY_ENVIRONMENT` and `SENTRY_RELEASE` entries.

- [ ] **Step 2: Update secrets example**

In `deploy/qisi/env/secrets.env.example`, replace:

```dotenv
# Optional Sentry DSN for the hub service.
SENTRY_DSN=
```

with:

```dotenv
# Optional Sentry DSN for the Rust hub service.
SENTRY_DSN=

# Optional Sentry DSN for the Next.js hub dashboard service.
AHAND_HUB_DASHBOARD_SENTRY_DSN=

# Optional smoke tokens for verifying deployed Sentry SDK capture.
AHAND_HUB_SENTRY_SMOKE_TOKEN=
AHAND_HUB_DASHBOARD_SENTRY_SMOKE_TOKEN=
```

- [ ] **Step 3: Validate Compose with sample env files**

Run:

```bash
tmpdir="$(mktemp -d)"
cp deploy/qisi/env/production.env.example "$tmpdir/.env"
cp deploy/qisi/env/secrets.env.example "$tmpdir/.env.secrets"
cat > "$tmpdir/.env.images" <<'EOF'
AHAND_HUB_IMAGE=registry.image.coffice.qisiai.top/coffice/ahand/ahand-hub:production-test
AHAND_HUB_DASHBOARD_IMAGE=registry.image.coffice.qisiai.top/coffice/ahand/ahand-hub-dashboard:production-test
GIT_SHA=test
EOF
docker compose --project-directory "$tmpdir" \
  --file deploy/qisi/compose.yml \
  --env-file "$tmpdir/.env.images" \
  --env-file "$tmpdir/.env" \
  --env-file "$tmpdir/.env.secrets" \
  config >/tmp/ahand-qisi-compose-sentry.yml
rg 'NEXT_PUBLIC_SENTRY_DSN|AHAND_HUB_DASHBOARD_SENTRY_DSN|SENTRY_DSN' /tmp/ahand-qisi-compose-sentry.yml
rm -rf "$tmpdir"
```

Expected: command exits 0 and rendered config contains dashboard `NEXT_PUBLIC_SENTRY_DSN` and `SENTRY_DSN` entries.

- [ ] **Step 4: Commit Qisi runtime config**

Run:

```bash
git add deploy/qisi/compose.yml deploy/qisi/env/secrets.env.example
git commit -m "chore: pass dashboard sentry dsn in qisi deploy"
```

---

### Task 8: Full Local Verification

**Files:**
- No new source changes unless verification finds issues.

- [ ] **Step 1: Run Rust formatting and hub checks**

Run:

```bash
cargo fmt --check
cargo test -p ahand-hub observability::tests -- --nocapture
cargo clippy -p ahand-hub --all-targets --all-features -- -D warnings
```

Expected: all commands exit 0.

- [ ] **Step 2: Run dashboard checks**

Run:

```bash
pnpm --filter @ahand/hub-dashboard test
pnpm --filter @ahand/hub-dashboard lint
pnpm --filter @ahand/hub-dashboard build
```

Expected: all commands exit 0.

- [ ] **Step 3: Run Qisi Compose validation**

Run the Compose validation command from Task 7 Step 3 again.

Expected: command exits 0.

- [ ] **Step 4: Inspect diff**

Run:

```bash
git status --short
git diff --stat origin/main...HEAD
```

Expected: `git status --short` is empty after commits. Diff includes only Sentry SDK, dashboard instrumentation, lockfile, and Qisi env/compose changes.

---

### Task 9: PR, CI, and Qisi Deployment

**Files:**
- No source changes.

- [ ] **Step 1: Push branch**

Run:

```bash
git push -u origin codex/qisi-sentry-sdk-integration
```

Expected: branch pushes successfully.

- [ ] **Step 2: Open PR**

Run:

```bash
gh pr create \
  --repo team9ai/aHand \
  --base main \
  --head codex/qisi-sentry-sdk-integration \
  --title "feat: add sentry sdk integration for hub" \
  --body "Adds Sentry SDK initialization for aHand hub and hub dashboard, plus Qisi dashboard DSN wiring."
```

Expected: GitHub returns a PR URL.

- [ ] **Step 3: Watch PR checks**

Run:

```bash
gh pr checks --repo team9ai/aHand --watch
```

Expected: Hub CI, Client CI, and Qisi deploy validation checks pass. If any check fails, fetch the failing job log with `gh run view RUN_ID --log-failed`, fix the cause, commit, push, and re-run checks.

- [ ] **Step 4: Merge after approval**

Run:

```bash
gh pr merge --repo team9ai/aHand --merge --delete-branch
```

Expected: PR merges to `main`.

- [ ] **Step 5: Watch main runs**

Run:

```bash
gh run list --repo team9ai/aHand --branch main --limit 4 --json databaseId,name,status,conclusion,headSha,url
```

Expected after completion: latest `Hub CI`, `Client CI`, `Deploy Hub`, and `Qisi aHand Deploy` for the merge commit all have `conclusion: success`.

---

### Task 10: Remote Qisi Sentry Runtime Setup

**Files:**
- No repository files.
- Remote files:
  - `qisi:/opt/ahand-hub/production/.env.secrets`
  - `qisi-dev:/opt/ahand-hub/dev/.env.secrets`
  - `qisi-dev:/opt/ahand-hub/staging/.env.secrets`

- [ ] **Step 1: Retrieve dashboard DSN and generate smoke tokens without printing secrets**

Run:

```bash
set -euo pipefail
DASHBOARD_SENTRY_DSN="$(
  ssh qisi-infra 'cd /opt/sentry && ./sentry-admin.sh shell -c "from sentry.models.projectkey import ProjectKey; k=ProjectKey.objects.filter(project__slug=\"ahand-dashboard\").first(); print(k.dsn_public)"' |
  tail -n 1 |
  tr -d '\r'
)"
case "$DASHBOARD_SENTRY_DSN" in
  https://*@sentry.coffice.qisiai.top/*) echo dashboard_sentry_dsn=valid ;;
  *) echo dashboard_sentry_dsn=invalid >&2; exit 1 ;;
esac
HUB_SENTRY_SMOKE_TOKEN="$(openssl rand -hex 24)"
DASHBOARD_SENTRY_SMOKE_TOKEN="$(openssl rand -hex 24)"
echo sentry_smoke_tokens=generated
```

Expected visible output:

```text
dashboard_sentry_dsn=valid
sentry_smoke_tokens=generated
```

Do not print the DSN or tokens.

- [ ] **Step 2: Update remote env files**

For each remote `.env.secrets`, set:

```dotenv
AHAND_HUB_DASHBOARD_SENTRY_DSN=${DASHBOARD_SENTRY_DSN}
AHAND_HUB_SENTRY_SMOKE_TOKEN=${HUB_SENTRY_SMOKE_TOKEN}
AHAND_HUB_DASHBOARD_SENTRY_SMOKE_TOKEN=${DASHBOARD_SENTRY_SMOKE_TOKEN}
```

Use this script from the same shell where Step 1 variables are still set:

```bash
set -euo pipefail
export DASHBOARD_SENTRY_DSN_B64="$(printf '%s' "$DASHBOARD_SENTRY_DSN" | base64 | tr -d '\n')"
export HUB_SENTRY_SMOKE_TOKEN_B64="$(printf '%s' "$HUB_SENTRY_SMOKE_TOKEN" | base64 | tr -d '\n')"
export DASHBOARD_SENTRY_SMOKE_TOKEN_B64="$(printf '%s' "$DASHBOARD_SENTRY_SMOKE_TOKEN" | base64 | tr -d '\n')"

update_remote_env() {
  host="$1"
  target_env_files="$2"
  ssh "$host" \
    "DASHBOARD_SENTRY_DSN_B64='$DASHBOARD_SENTRY_DSN_B64' HUB_SENTRY_SMOKE_TOKEN_B64='$HUB_SENTRY_SMOKE_TOKEN_B64' DASHBOARD_SENTRY_SMOKE_TOKEN_B64='$DASHBOARD_SENTRY_SMOKE_TOKEN_B64' TARGET_ENV_FILES='$target_env_files' python3 - <<'PY'
import base64
import os
from pathlib import Path

paths = os.environ["TARGET_ENV_FILES"].split(":")
values = {
    "AHAND_HUB_DASHBOARD_SENTRY_DSN": base64.b64decode(os.environ["DASHBOARD_SENTRY_DSN_B64"]).decode(),
    "AHAND_HUB_SENTRY_SMOKE_TOKEN": base64.b64decode(os.environ["HUB_SENTRY_SMOKE_TOKEN_B64"]).decode(),
    "AHAND_HUB_DASHBOARD_SENTRY_SMOKE_TOKEN": base64.b64decode(os.environ["DASHBOARD_SENTRY_SMOKE_TOKEN_B64"]).decode(),
}
for filename in paths:
    path = Path(filename)
    lines = path.read_text().splitlines() if path.exists() else []
    for key, value in values.items():
        replacement = key + "=" + value
        for idx, line in enumerate(lines):
            if line.startswith(key + "="):
                lines[idx] = replacement
                break
        else:
            lines.append(replacement)
    path.write_text("\n".join(lines) + "\n")
    path.chmod(0o600)
    print(f"{filename}: AHAND_HUB_DASHBOARD_SENTRY_DSN=set")
    print(f"{filename}: sentry_smoke_tokens=set")
PY"
}

update_remote_env qisi "/opt/ahand-hub/production/.env.secrets"
update_remote_env qisi-dev "/opt/ahand-hub/dev/.env.secrets:/opt/ahand-hub/staging/.env.secrets"
```

Keep the existing hub `SENTRY_DSN` unchanged.

Expected visible output:

```text
/opt/ahand-hub/production/.env.secrets: AHAND_HUB_DASHBOARD_SENTRY_DSN=set
/opt/ahand-hub/dev/.env.secrets: AHAND_HUB_DASHBOARD_SENTRY_DSN=set
/opt/ahand-hub/staging/.env.secrets: AHAND_HUB_DASHBOARD_SENTRY_DSN=set
/opt/ahand-hub/production/.env.secrets: sentry_smoke_tokens=set
/opt/ahand-hub/dev/.env.secrets: sentry_smoke_tokens=set
/opt/ahand-hub/staging/.env.secrets: sentry_smoke_tokens=set
```

- [ ] **Step 3: Confirm production deploy picked up the env**

After `Qisi aHand Deploy` succeeds on main, run:

```bash
ssh qisi 'cd /opt/ahand-hub/production && \
  bash scripts/healthcheck.sh && \
  docker compose --env-file .env.images --env-file .env --env-file .env.secrets exec -T hub sh -c '"'"'test -n "$SENTRY_DSN" && echo hub_sentry_dsn=set'"'"' && \
  docker compose --env-file .env.images --env-file .env --env-file .env.secrets exec -T dashboard sh -c '"'"'test -n "$SENTRY_DSN" && test -n "$NEXT_PUBLIC_SENTRY_DSN" && echo dashboard_sentry_dsn=set'"'"''
```

Expected:

```text
ahand-hub healthy: http://127.0.0.1:3815/api/health
ahand-hub-dashboard healthy: http://127.0.0.1:3816/login
hub_sentry_dsn=set
dashboard_sentry_dsn=set
```

- [ ] **Step 4: Trigger automatic SDK smoke events**

Use deployed runtime paths to trigger one hub `tracing::error!` event and one dashboard server request error. Run from the same shell where Step 1 variables are still set:

```bash
set -euo pipefail
ssh qisi "curl -fsS -X POST \
  -H 'x-ahand-sentry-smoke-token: ${HUB_SENTRY_SMOKE_TOKEN}' \
  http://127.0.0.1:3815/api/system/sentry-smoke >/dev/null && echo hub_sentry_sdk_smoke=triggered"
ssh qisi "status=\$(curl -sS -o /dev/null -w '%{http_code}' -X POST \
  -H 'x-ahand-sentry-smoke-token: ${DASHBOARD_SENTRY_SMOKE_TOKEN}' \
  http://127.0.0.1:3816/api/sentry-smoke); \
  test \"\$status\" = 500 && echo dashboard_sentry_sdk_smoke=triggered"
```

Expected visible output:

```text
hub_sentry_sdk_smoke=triggered
dashboard_sentry_sdk_smoke=triggered
```

- [ ] **Step 5: Confirm smoke events reached qisi-infra Sentry**

Run:

```bash
for attempt in $(seq 1 12); do
  output="$(
    ssh qisi-infra 'cd /opt/sentry && ./sentry-admin.sh shell -c "
from sentry.models.group import Group
checks = [
    (\"ahand-hub\", \"aHand hub Sentry smoke test\", \"hub_sentry_sdk_smoke\"),
    (\"ahand-dashboard\", \"aHand dashboard Sentry smoke test\", \"dashboard_sentry_sdk_smoke\"),
]
for project, title, label in checks:
    exists = Group.objects.filter(project__slug=project, title__icontains=title).exists()
    print(label + (\"=accepted\" if exists else \"=missing\"))
"'
  )"
  printf '%s\n' "$output"
  if printf '%s\n' "$output" | rg -q 'hub_sentry_sdk_smoke=accepted' &&
     printf '%s\n' "$output" | rg -q 'dashboard_sentry_sdk_smoke=accepted'; then
    break
  fi
  test "$attempt" = 12 && exit 1
  sleep 5
done
```

Expected visible output:

```text
hub_sentry_sdk_smoke=accepted
dashboard_sentry_sdk_smoke=accepted
```

---

## Self-Review Checklist

- Spec coverage: hub SDK, dashboard SDK, qisi env separation, local tests, CI, deployment, and remote verification all have tasks.
- Operator inputs: required runtime values are explicitly named, and no
  unspecified code steps remain.
- Type consistency: `HubSentryConfig`, `DashboardSentryOptions`, `AHAND_HUB_DASHBOARD_SENTRY_DSN`, `SENTRY_DSN`, `NEXT_PUBLIC_SENTRY_DSN`, `SENTRY_ENVIRONMENT`, and `SENTRY_RELEASE` are used consistently.
- Scope control: source map upload, replay, feedback, and performance tracing are explicitly out of scope.
