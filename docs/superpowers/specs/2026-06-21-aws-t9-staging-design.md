# AWS t9 Staging Environment Design

## Goal

Add a first-class AWS t9 staging environment for the aHand hub, matching the
existing global AWS deployment path instead of only relying on the Qisi staging
runtime.

## Scope

This change covers the Rust hub service deployed by `.github/workflows/deploy-hub.yml`.
The global AWS path does not currently deploy the Next.js hub dashboard
container, so dashboard Sentry/runtime settings remain Qisi-only until a global
dashboard deployment path exists.

## Environment Mapping

The GitHub branch mapping should be:

| Branch | Environment | ECS cluster | ECS service | Public host |
|---|---|---|---|---|
| `main` | `prod` | `openclaw-hive` | `ahand-hub-prod` | `ahand-hub.team9.ai` |
| `dev` | `dev` | `openclaw-hive-dev` | `ahand-hub-dev` | `ahand-hub.dev.team9.ai` |
| `staging` | `staging` | `openclaw-hive-dev` | `ahand-hub-staging` | `ahand-hub.staging.team9.ai` |

Staging uses the t9 account (`149614785083`), the non-production cluster
(`openclaw-hive-dev`), and a separate Terraform stack under
`infra/envs/staging`.

## Terraform

Add `infra/envs/staging` as an independent backend state:

- S3 backend bucket: `team9-tfstate`
- State key: `ahand-hub/envs/staging/terraform.tfstate`
- AWS profile: `t9`
- Lock table: `terraform-state-lock`

The staging stack should call `../../modules/ahand-hub` with:

- `env = "staging"`
- `ecs_cluster_name = "openclaw-hive-dev"`
- `api_domain = "ahand-hub.staging.team9.ai"`
- The same t9 VPC, subnet, Traefik, and RDS values used by dev
- `gateway_public_url = "https://api.staging.team9.ai"`
- `redis_mode = "create"`

The shared module's `env` validation must accept `staging`.

## Runtime Parameters

Terraform seeds `/ahand-hub/staging/*` parameters the same way as dev/prod.
Operator-seeded values remain out of Terraform state:

- `/ahand-hub/staging/DATABASE_URL`
- `/ahand-hub/staging/SENTRY_DSN`

`DATABASE_URL` must point at a dedicated `ahand_hub_staging` database/user in
the staging/non-production RDS instance before the service can run healthily.
`SENTRY_DSN` can be placeholder initially, but automatic Sentry capture is not
active until a real DSN is written and ECS is redeployed.

## CI/CD

`.github/workflows/deploy-hub.yml` should trigger on `staging` and set:

- `ENV=staging`
- `ECS_CLUSTER=openclaw-hive-dev`
- `SERVICE_NAME=ahand-hub-staging`

`deploy/hub/deploy.sh` should accept `staging`, select the same cluster/service,
and render `API_DOMAIN=ahand-hub.staging.team9.ai`.

## Validation

Local validation should include:

- A repo check proving staging is wired into Terraform, workflow, and deploy
  script.
- YAML parse of `.github/workflows/deploy-hub.yml`.
- Bash syntax check of `deploy/hub/deploy.sh`.
- `terraform fmt -check` for shared module and env stacks.
- `terraform init -backend=false` and `terraform validate` for
  `infra/envs/staging`.

Live validation after merge/apply should include:

- Terraform plan/apply for `infra/envs/staging` using profile `t9`.
- SSM presence checks for `/ahand-hub/staging/*`.
- ECS service check for `ahand-hub-staging`.
- Staging deploy run from the `staging` branch.
- Health check at `https://ahand-hub.staging.team9.ai/api/health`.
