# aHand Sentry SDK Integration Design

Date: 2026-06-21

## Scope

Add real automatic Sentry error capture for the aHand hub and hub dashboard.
The qisi-infra Sentry projects and qisi runtime DSN placeholders already exist;
this design connects the application code to those DSNs through official SDKs.

This design covers:

- Rust `ahand-hub` SDK initialization and tracing event capture.
- Next.js `apps/hub-dashboard` SDK initialization for browser, server, and edge
  runtime errors.
- Qisi Compose environment changes so hub and dashboard use separate Sentry
  projects.
- Tests and deployment verification that prove SDK initialization is wired.
- Default-disabled smoke hooks for verifying that the deployed SDKs capture
  events through normal application paths.

This design does not add source map upload, session replay, user feedback,
performance tracing, or Sentry issue ownership automation. Those can be added
later after error capture is stable and a Sentry auth token policy exists.

## Requirements

- No Sentry DSN or auth token is committed.
- Hub and dashboard report to separate Sentry projects.
- SDKs are disabled when their DSN is absent.
- Release values match CI commit SHA through existing `SENTRY_RELEASE`.
- Environment values match `dev`, `staging`, or `production`.
- Sentry setup must not change existing logging output format.
- Sentry setup must not require qisi-only code paths; it should work for both
  qisi and any global runtime that provides compatible env vars.
- Qisi production must continue passing existing health checks after deployment.

## Reference Docs

- Sentry Rust SDK: `sentry = "0.46.2"` with `sentry::init` and a retained init
  guard: `https://docs.sentry.io/platforms/rust/`.
- Sentry Rust tracing integration: `sentry::integrations::tracing::layer()`
  captures `tracing` events as breadcrumbs/events:
  `https://docs.sentry.io/platforms/rust/tracing/`.
- Sentry Next.js manual setup for Next.js 15+ App Router: `@sentry/nextjs`,
  `withSentryConfig`, `instrumentation-client.ts`, `sentry.server.config.ts`,
  `sentry.edge.config.ts`, `instrumentation.ts`, and `app/global-error.tsx`:
  `https://docs.sentry.io/platforms/javascript/guides/nextjs/manual-setup/`.

## Current State

Rust hub currently initializes only `tracing_subscriber` in
`crates/ahand-hub/src/main.rs`. It reads `SENTRY_DSN`, `SENTRY_ENVIRONMENT`, and
`SENTRY_RELEASE` from qisi Compose, but no Rust SDK consumes those variables.

Hub dashboard currently has no `@sentry/*` dependency and no Next
instrumentation files. Qisi Compose passes `SENTRY_ENVIRONMENT` and
`SENTRY_RELEASE` to the dashboard, but not a dashboard DSN.

Qisi `.env.secrets` currently uses `SENTRY_DSN` for the hub service. The
dashboard needs a separate DSN because it should report to the
`ahand-dashboard` Sentry project, not the `ahand-hub` project.

## Smoke Verification Hooks

Add two token-gated smoke routes that are disabled unless their token env var
is non-empty.

Hub route:

- Path: `POST /api/system/sentry-smoke`
- Token header: `x-ahand-sentry-smoke-token`
- Env token: `AHAND_HUB_SENTRY_SMOKE_TOKEN`
- Behavior: emit a `tracing::error!` event and return `{ "ok": true }`

Because the event is a normal `tracing` event, this verifies the automatic
Sentry tracing layer instead of calling Sentry directly.

Dashboard route:

- Path: `POST /api/sentry-smoke`
- Token header: `x-ahand-sentry-smoke-token`
- Env token: `AHAND_HUB_DASHBOARD_SENTRY_SMOKE_TOKEN`
- Behavior: throw an `Error`

Because the route throws inside a Next request, this verifies automatic capture
through the Next `onRequestError` hook. The middleware should explicitly allow
this route so the token-gated smoke handler receives the request.

Both routes return `404` when the token env var is absent and `403` when the
header token does not match.

## Architecture

### Hub

Create a small `crates/ahand-hub/src/observability.rs` module responsible for
Sentry option parsing and initialization. `main.rs` should call it before
tracing subscriber setup and before starting the Tokio runtime.

The hub binary should use a plain synchronous `main()`:

1. Read Sentry configuration from environment.
2. Initialize Sentry if `SENTRY_DSN` is non-empty.
3. Initialize `tracing_subscriber`, adding the Sentry tracing layer only when
   Sentry is enabled.
4. Build a Tokio runtime and run the existing async server body.

This keeps the Sentry init guard alive for the full process and follows the
Rust SDK guidance to retain the guard until shutdown.

Hub Sentry options:

- `dsn`: `SENTRY_DSN`
- `environment`: `SENTRY_ENVIRONMENT`
- `release`: first non-empty value among `SENTRY_RELEASE`, `GIT_SHA`, then
  `sentry::release_name!()`
- `send_default_pii`: `false`
- tracing layer: map `ERROR` tracing events to Sentry events and keep lower
  levels out of Sentry to avoid noisy or sensitive breadcrumbs in the first
  rollout.

### Hub Dashboard

Install `@sentry/nextjs` in `apps/hub-dashboard`.

Add these files under `apps/hub-dashboard/src`:

- `instrumentation-client.ts`: browser SDK init using
  `NEXT_PUBLIC_SENTRY_DSN`.
- `sentry.server.config.ts`: Node runtime SDK init using `SENTRY_DSN` or
  `NEXT_PUBLIC_SENTRY_DSN`.
- `sentry.edge.config.ts`: edge runtime SDK init using `SENTRY_DSN` or
  `NEXT_PUBLIC_SENTRY_DSN`.
- `instrumentation.ts`: imports server or edge config based on
  `NEXT_RUNTIME` and exports `Sentry.captureRequestError`.
- `app/global-error.tsx`: captures React render errors with
  `Sentry.captureException`.

Update `apps/hub-dashboard/next.config.ts` to wrap the current config with
`withSentryConfig`. Do not configure source map upload in this pass; without a
Sentry auth token, source map upload is intentionally out of scope.

Dashboard Sentry options:

- Browser `dsn`: `NEXT_PUBLIC_SENTRY_DSN`
- Server/edge `dsn`: `SENTRY_DSN || NEXT_PUBLIC_SENTRY_DSN`
- `environment`: `SENTRY_ENVIRONMENT`
- `release`: `SENTRY_RELEASE`
- `sendDefaultPii`: `false`
- `tracesSampleRate`: `0`
- session replay: disabled
- user feedback: disabled

### Qisi Runtime Env

Keep hub DSN as `SENTRY_DSN` in `.env.secrets`.

Add dashboard DSN as:

```dotenv
AHAND_HUB_DASHBOARD_SENTRY_DSN=
AHAND_HUB_SENTRY_SMOKE_TOKEN=
AHAND_HUB_DASHBOARD_SENTRY_SMOKE_TOKEN=
```

Update `deploy/qisi/compose.yml`:

- `hub` keeps `.env`, `.env.secrets`, `.env.images`.
- `dashboard` also reads `.env.secrets`.
- `dashboard.environment.SENTRY_DSN` is set from
  `${AHAND_HUB_DASHBOARD_SENTRY_DSN:-}`.
- `dashboard.environment.NEXT_PUBLIC_SENTRY_DSN` is set from
  `${AHAND_HUB_DASHBOARD_SENTRY_DSN:-}`.

This lets both services read shared secret files while preserving separate
Sentry projects.

## Testing Strategy

Hub tests:

- Unit-test Sentry config parsing with a lookup closure instead of mutating
  process env.
- Verify empty DSN disables Sentry.
- Verify release precedence is `SENTRY_RELEASE`, then `GIT_SHA`, then fallback.
- Verify `SENTRY_ENVIRONMENT` maps into the config.

Dashboard tests:

- Add a small `src/lib/sentry-config.ts` helper and unit-test DSN selection,
  release, environment, and disabled behavior.
- Keep SDK initialization files thin and driven by the tested helper.
- Run existing Vitest suite and Next build.

Deployment tests:

- Validate Qisi Compose config with sample `.env`, `.env.secrets`, and
  `.env.images`.
- Deploy through CI to qisi.
- Confirm qisi production hub and dashboard health checks pass.
- Confirm hub container has non-empty `SENTRY_DSN`.
- Confirm dashboard container has non-empty `SENTRY_DSN` and
  `NEXT_PUBLIC_SENTRY_DSN`.
- Send one backend and one dashboard SDK smoke event to qisi-infra Sentry using
  the deployed runtime paths. Do not print DSNs in logs or final output.

## Rollout

1. Merge SDK code and Qisi Compose changes.
2. Add `AHAND_HUB_DASHBOARD_SENTRY_DSN` to qisi/qisi-dev `.env.secrets` from the
   qisi-infra `ahand-dashboard` project.
3. Let `main` CI deploy production qisi images.
4. Verify qisi health checks and Sentry smoke events.
5. Leave dev/staging env prepared; their containers pick up the same setup on
   the next dev/staging deployment.

## Risks

- `@sentry/nextjs` can change Next build output and instrumentation behavior.
  Mitigation: run dashboard tests and `next build` before deployment.
- Rust tracing integration can create event volume if too permissive.
  Mitigation: only map `ERROR` events to Sentry in the first rollout.
- Dashboard client DSN is public by design. Mitigation: only use the public DSN
  and do not expose auth tokens.
- Source maps will not be uploaded in this pass. Stack traces may be minified
  until source map upload is explicitly added.
