# Qisi aHand CN Deployment Design

Date: 2026-06-21

## Scope

Deploy a China-specific aHand hub stack onto Aliyun hosts reachable as `qisi`
and `qisi-dev`, following the current Agent PI qisi deployment pattern.

This design covers:

- Docker image publishing to the qisi registry.
- Branch-based GitHub Actions rollout over SSH.
- CI-triggered CN rollout whenever the global hub deployment workflow would
  update the AWS/t9 environment for the same branch and hub-related changes.
- Per-environment Docker Compose projects.
- Caddy routing, public domains, and loopback-bound host ports.
- Environment file layout, secret handling, health checks, and rollback.

This design does not replace or modify the existing AWS/t9 deployment. The
existing `.github/workflows/deploy-hub.yml`, ECS task definition, and AWS
infrastructure remain the global deployment path. The CN workflow is additive:
hub updates should keep deploying to global and should also deploy to qisi.

It also does not provision Aliyun RDS, Redis, OSS, DNS, or Sentry resources.
Those resources are operator-managed and are injected through host-local
environment files.

## Reference Pattern

The implementation should mirror the active Agent PI qisi deployment:

- Each environment owns a directory under `/opt`.
- Docker Compose reads three env files:
  - `.env` for non-secret runtime settings.
  - `.env.secrets` for credentials and tokens.
  - `.env.images` for the currently deployed immutable image tags.
- CI writes `.env.images.next`, copies it to the host, and invokes a host-local
  `scripts/deploy.sh`.
- The host deploy script validates Compose, pulls candidate images, backs up the
  active `.env.images`, promotes the candidate, starts containers, runs health
  checks, and restores the previous image file on failure.
- Caddy proxies public domains to loopback-bound host ports.

## Deployment Topology

Production runs on `qisi`.

- Git branch: `main`
- Environment name: `production`
- Host: `qisi`
- Remote directory: `/opt/ahand-hub/production`

Development and staging run on `qisi-dev`.

- Git branch: `dev`
- Environment name: `dev`
- Host: `qisi-dev`
- Remote directory: `/opt/ahand-hub/dev`

- Git branch: `staging`
- Environment name: `staging`
- Host: `qisi-dev`
- Remote directory: `/opt/ahand-hub/staging`

Each environment runs two containers in its own Docker Compose project:

- `hub`, running the Rust `ahand-hub` service.
- `dashboard`, running the Next.js `ahand-hub-dashboard` service.

Postgres and Redis are not containerized. Production should use production
Aliyun-managed data stores. Dev and staging should use dev-side Aliyun-managed
data stores, isolated by separate databases and Redis logical DB indexes or
separate instances.

## Public Domains

Production:

- Hub: `ahand-hub.coffice.qisiai.top`
- Dashboard: `admin.ahand.coffice.qisiai.top`

Staging:

- Hub: `ahand-hub.staging.coffice.qisiai.top`
- Dashboard: `admin.ahand.staging.coffice.qisiai.top`

Development:

- Hub: `ahand-hub.dev.coffice.qisiai.top`
- Dashboard: `admin.ahand.dev.coffice.qisiai.top`

Public DNS should point production domains to `qisi` and dev/staging domains to
`qisi-dev`. Private DNS may provide split-horizon records inside the Aliyun VPC,
but containers should not depend on public DNS for same-environment calls.

## Caddy Routing

Each host owns Caddy routing for the environments it serves.

Production on `qisi`:

- `ahand-hub.coffice.qisiai.top` reverse proxies to `127.0.0.1:3815`.
- `admin.ahand.coffice.qisiai.top` reverse proxies to `127.0.0.1:3816`.

Dev/staging on `qisi-dev`:

- `ahand-hub.dev.coffice.qisiai.top` reverse proxies to `127.0.0.1:5815`.
- `admin.ahand.dev.coffice.qisiai.top` reverse proxies to `127.0.0.1:5816`.
- `ahand-hub.staging.coffice.qisiai.top` reverse proxies to `127.0.0.1:4815`.
- `admin.ahand.staging.coffice.qisiai.top` reverse proxies to
  `127.0.0.1:4816`.

Compose should bind application ports to loopback only. No aHand container port
should listen directly on a public interface.

## Host Ports

Use host ports that do not collide with the existing Agent PI deployments:

| Environment | Hub Host Port | Dashboard Host Port |
| ----------- | ------------- | ------------------- |
| production  | 3815          | 3816                |
| staging     | 4815          | 4816                |
| dev         | 5815          | 5816                |

Container ports stay stable:

- Hub container: `1515`
- Dashboard container: `1516`

## Images

Reuse the existing `deploy/hub/Dockerfile` and its two targets:

- `hub`
- `dashboard`

No qisi-specific Dockerfiles are needed unless future Aliyun builds require
China-only base-image mirrors.

Image names:

- `registry.image.coffice.qisiai.top/coffice/ahand/ahand-hub`
- `registry.image.coffice.qisiai.top/coffice/ahand/ahand-hub-dashboard`

Each build publishes both immutable and floating tags:

- Immutable: `<environment>-<git-sha>`
- Floating: `dev`, `staging`, or `production`

Examples:

- `registry.image.coffice.qisiai.top/coffice/ahand/ahand-hub:dev-<sha>`
- `registry.image.coffice.qisiai.top/coffice/ahand/ahand-hub-dashboard:production`

Deploys should run from immutable tags written into `.env.images.next`.
Floating tags are for operator inspection and emergency manual pulls only.

## Remote Files

Each environment directory contains host-local files:

- `compose.yml`
- `.env`
- `.env.secrets`
- `.env.images`
- `.env.images.next` during deploy
- `.deploy-history/`
- `.deploy-locks/`
- `scripts/deploy.sh`
- `scripts/healthcheck.sh`

Repository-tracked deployment files:

- `deploy/qisi/compose.yml`
- `deploy/qisi/env/dev.env.example`
- `deploy/qisi/env/staging.env.example`
- `deploy/qisi/env/production.env.example`
- `deploy/qisi/env/secrets.env.example`
- `deploy/qisi/caddy/dev.Caddyfile`
- `deploy/qisi/caddy/staging.Caddyfile`
- `deploy/qisi/caddy/production.Caddyfile`
- `deploy/qisi/scripts/deploy.sh`
- `deploy/qisi/scripts/healthcheck.sh`
- `.github/workflows/qisi-deploy.yml`

No secret values should be committed.

## Compose Runtime

The shared Compose file should be parameterized by `.env` and `.env.images`.

Compose project name:

```yaml
name: ahand-hub-${DEPLOY_ENV}
```

Service `hub`:

- Container name: `ahand-hub-${DEPLOY_ENV}-hub`
- Image variable: `AHAND_HUB_IMAGE`
- Restart policy: `unless-stopped`
- Env files: `.env`, `.env.secrets`, `.env.images`
- Host port: `127.0.0.1:${AHAND_HUB_HOST_PORT}:1515`
- Volume: `hub-audit-data:/var/lib/ahand-hub`
- Health check: `curl -fsS http://127.0.0.1:1515/api/health`

Service `dashboard`:

- Container name: `ahand-hub-${DEPLOY_ENV}-dashboard`
- Image variable: `AHAND_HUB_DASHBOARD_IMAGE`
- Restart policy: `unless-stopped`
- Depends on healthy `hub`
- Env files: `.env`, `.env.secrets`, `.env.images`
- Host port: `127.0.0.1:${AHAND_HUB_DASHBOARD_HOST_PORT}:1516`
- Runtime hub URL: `AHAND_HUB_BASE_URL=http://hub:1515`
- Health check: `curl -fsS http://127.0.0.1:1516/login`

## Environment Files

`.env` contains non-secret values.

Common keys:

```dotenv
DEPLOY_ENV=dev
APP_ENV=development
NODE_ENV=production

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

Environment-specific `.env` examples should set:

- `DEPLOY_ENV`
- `APP_ENV`
- public domains and URLs
- host ports
- dashboard allowed origins

`.env.secrets` contains sensitive values.

Required keys:

```dotenv
AHAND_HUB_SERVICE_TOKEN=
AHAND_HUB_DASHBOARD_PASSWORD=
AHAND_HUB_DEVICE_BOOTSTRAP_TOKEN=
AHAND_HUB_DEVICE_BOOTSTRAP_DEVICE_ID=
AHAND_HUB_JWT_SECRET=
AHAND_HUB_DATABASE_URL=
AHAND_HUB_REDIS_URL=
```

Optional keys:

```dotenv
AHAND_HUB_WEBHOOK_URL=
AHAND_HUB_WEBHOOK_SECRET=
AHAND_HUB_S3_BUCKET=
AHAND_HUB_S3_REGION=
AHAND_HUB_S3_ENDPOINT=
AHAND_HUB_S3_THRESHOLD_BYTES=
AHAND_HUB_S3_URL_EXPIRATION_SECS=
AWS_ACCESS_KEY_ID=
AWS_SECRET_ACCESS_KEY=
SENTRY_DSN=
```

For Aliyun OSS compatibility, `AHAND_HUB_S3_ENDPOINT` should be set when using
an OSS S3-compatible endpoint. S3/OSS can be left unset for a first deployment;
large file transfer endpoints will then return the existing `S3_DISABLED`
application response.

`.env.images` is written by CI and contains active image tags:

```dotenv
AHAND_HUB_IMAGE=registry.image.coffice.qisiai.top/coffice/ahand/ahand-hub:dev-<sha>
AHAND_HUB_DASHBOARD_IMAGE=registry.image.coffice.qisiai.top/coffice/ahand/ahand-hub-dashboard:dev-<sha>
GIT_SHA=<sha>
SENTRY_RELEASE=<sha>
```

## GitHub Actions Workflow

Add `.github/workflows/qisi-deploy.yml`.

Triggers:

- Push to `dev`
- Push to `staging`
- Push to `main`
- Manual `workflow_dispatch`

The `dev` and `main` push triggers must include the same hub-related path set as
`.github/workflows/deploy-hub.yml`, plus qisi deployment files. This keeps CN
deploys synchronized with global hub deploys while avoiding a qisi rollout for
unrelated docs or client-only changes. `staging` uses the same path set for a
pre-production qisi validation lane.

Required path set:

- `apps/hub-dashboard/**`
- `crates/ahand-hub/**`
- `crates/ahand-hub-core/**`
- `crates/ahand-hub-store/**`
- `crates/ahand-protocol/**`
- `proto/**`
- `Cargo.lock`
- `package.json`
- `pnpm-lock.yaml`
- `pnpm-workspace.yaml`
- `turbo.json`
- `deploy/hub/Dockerfile`
- `deploy/qisi/**`
- `.github/workflows/qisi-deploy.yml`

Concurrency:

```yaml
group: qisi-ahand-deploy-${{ github.ref_name }}
cancel-in-progress: false
```

Branch mapping:

| Branch    | Env        | SSH Host Secret      | Deploy Dir                    |
| --------- | ---------- | -------------------- | ----------------------------- |
| `dev`     | `dev`      | `QISI_DEV_SSH_HOST`  | `/opt/ahand-hub/dev`          |
| `staging` | `staging`  | `QISI_DEV_SSH_HOST`  | `/opt/ahand-hub/staging`      |
| `main`    | `production` | `QISI_SSH_HOST`    | `/opt/ahand-hub/production`   |

Build matrix:

- Image `ahand-hub`, Dockerfile `deploy/hub/Dockerfile`, target `hub`.
- Image `ahand-hub-dashboard`, Dockerfile `deploy/hub/Dockerfile`, target
  `dashboard`.

The workflow should:

1. Resolve target environment and deploy directory.
2. Log in to `registry.image.coffice.qisiai.top`.
3. Build and push both images with immutable and floating tags.
4. Configure SSH from GitHub secrets.
5. Write `.env.images.next` with immutable image tags.
6. Copy `compose.yml`, `scripts/deploy.sh`, `scripts/healthcheck.sh`, and
   `.env.images.next` to the target directory.
7. Run `cd <deploy_dir> && bash scripts/deploy.sh .env.images.next`.

Required GitHub secrets:

- `QISI_REGISTRY_USERNAME`
- `QISI_REGISTRY_PASSWORD`
- `QISI_SSH_PRIVATE_KEY`
- `QISI_SSH_USER`
- `QISI_SSH_HOST`
- `QISI_DEV_SSH_HOST`
- `QISI_KNOWN_HOSTS`

Optional GitHub secrets for future Sentry release automation:

- `QISI_SENTRY_AUTH_TOKEN`

Sentry release automation is intentionally not required for the first CN
deployment. Runtime `SENTRY_DSN` can be supplied through `.env.secrets`.

## Host Deploy Script

`deploy/qisi/scripts/deploy.sh` should follow the Agent PI deploy script shape:

1. Require `compose.yml`, `.env`, `.env.secrets`, and candidate image env.
2. Source `.env` and require `DEPLOY_ENV`.
3. Acquire an environment-specific lock under `.deploy-locks`.
4. Render a candidate Compose file that points env-file entries to absolute
   paths, using candidate `.env.images.next`.
5. Run `docker compose config` for the candidate.
6. Pull candidate images unless `SKIP_PULL=1`.
7. Back up active `.env.images` to `.deploy-history/<timestamp>-<pid>.env.images`.
8. Copy the candidate image env to `.env.images`.
9. Run `docker compose --env-file .env --env-file .env.secrets --env-file .env.images -f compose.yml up -d --remove-orphans`.
10. Run `scripts/healthcheck.sh`.
11. If any post-promotion step fails, restore the backed-up `.env.images` and
    run Compose again.

## Health Checks

`deploy/qisi/scripts/healthcheck.sh` should source `.env` and check:

- Hub: `http://127.0.0.1:${AHAND_HUB_HOST_PORT}/api/health`
- Dashboard: `http://127.0.0.1:${AHAND_HUB_DASHBOARD_HOST_PORT}/login`

Each check should retry for up to 120 seconds and fail loudly with the checked
URL if the service does not become healthy.

## Rollback

Rollback is image-level and host-local:

1. Pick a prior `.deploy-history/*.env.images` file.
2. Copy it over `.env.images`.
3. Run `docker compose --env-file .env --env-file .env.secrets --env-file .env.images -f compose.yml up -d --remove-orphans`.
4. Run `bash scripts/healthcheck.sh`.

The deploy script handles automatic rollback when a new rollout fails after
promotion.

## Bootstrap Tasks

Before the first deploy, operators must:

1. Create directories:
   - `qisi-dev:/opt/ahand-hub/dev`
   - `qisi-dev:/opt/ahand-hub/staging`
   - `qisi:/opt/ahand-hub/production`
2. Copy the matching env example to `.env` in each directory.
3. Create `.env.secrets` in each directory from `secrets.env.example`.
4. Ensure qisi/qisi-dev can pull from `registry.image.coffice.qisiai.top`.
5. Add or merge the Caddy snippets into each host's Caddy config.
6. Run `caddy validate --config /etc/caddy/Caddyfile`.
7. Reload Caddy after validation.

## Testing Strategy

Local/repository checks:

- `docker compose --env-file <tmp>.env --env-file <tmp>.env.secrets --env-file <tmp>.env.images -f deploy/qisi/compose.yml config`
- `docker build --target hub -f deploy/hub/Dockerfile -t ahand-hub:qisi-smoke .`
- `docker build --target dashboard -f deploy/hub/Dockerfile -t ahand-hub-dashboard:qisi-smoke .`
- Shell syntax check for `deploy/qisi/scripts/deploy.sh` and
  `deploy/qisi/scripts/healthcheck.sh`.
- Existing hub CI should continue to cover Rust hub and dashboard behavior.

Host checks:

- `ssh qisi-dev 'docker --version && docker compose version && caddy version'`
- `ssh qisi 'docker --version && docker compose version && caddy version'`
- First deploy to `dev`, then staging, then production.
- After deploy, validate:
  - `https://ahand-hub.dev.coffice.qisiai.top/api/health`
  - `https://admin.ahand.dev.coffice.qisiai.top/login`

## Risks And Decisions

- The CN deployment intentionally does not share AWS/t9 resources. Database,
  Redis, and optional OSS/S3 values must be supplied per environment.
- Reusing the existing Dockerfile keeps image behavior aligned with AWS but may
  need base-image mirror work if GitHub Actions or Docker Hub access becomes
  unreliable.
- Dashboard and hub are split-origin publicly, so
  `AHAND_HUB_DASHBOARD_ALLOWED_ORIGINS` must explicitly list the dashboard
  origin for each environment.
- Webhook integration with the CN team9 gateway is optional at bootstrap. If
  `AHAND_HUB_WEBHOOK_URL` is set, `AHAND_HUB_WEBHOOK_SECRET` must also be set,
  matching the existing hub config validation.
- S3/OSS file transfer can be enabled later by filling `AHAND_HUB_S3_*` and AWS
  credential-compatible env vars. It is not required for the initial hub and
  dashboard deployment.
