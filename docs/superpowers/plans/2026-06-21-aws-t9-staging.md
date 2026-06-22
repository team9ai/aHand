# AWS t9 Staging Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a first-class AWS t9 staging environment for the aHand hub.

**Architecture:** Reuse the existing `ahand-hub` Terraform module with a new
`infra/envs/staging` stack, and extend the global hub deploy workflow and deploy
script to map the `staging` branch to `ahand-hub-staging`.

**Tech Stack:** Terraform, AWS ECS/Fargate, SSM Parameter Store, GitHub Actions,
Bash.

---

### Task 1: Add A Staging Config Guard

**Files:**
- Create: `scripts/verify-aws-staging-config.sh`

- [ ] **Step 1: Write the failing guard script**

Create `scripts/verify-aws-staging-config.sh` with checks for:

- `infra/envs/staging/main.tf`
- module env validation containing `staging`
- `.github/workflows/deploy-hub.yml` triggering on `staging`
- deploy script accepting and mapping `staging`

- [ ] **Step 2: Run the guard and confirm it fails**

Run: `bash scripts/verify-aws-staging-config.sh`

Expected before implementation: non-zero exit and a missing staging message.

- [ ] **Step 3: Commit the guard**

Run:

```bash
git add scripts/verify-aws-staging-config.sh docs/superpowers/specs/2026-06-21-aws-t9-staging-design.md docs/superpowers/plans/2026-06-21-aws-t9-staging.md
git commit -m "docs: plan aws t9 staging environment"
```

### Task 2: Add Terraform Staging Stack

**Files:**
- Create: `infra/envs/staging/backend.tf`
- Create: `infra/envs/staging/main.tf`
- Create: `infra/envs/staging/providers.tf`
- Create: `infra/envs/staging/variables.tf`
- Create: `infra/envs/staging/versions.tf`
- Modify: `infra/modules/ahand-hub/variables.tf`

- [ ] **Step 1: Add `staging` to module env validation**

Update the validation list from `["prod", "dev"]` to
`["prod", "dev", "staging"]`.

- [ ] **Step 2: Copy the dev stack shape into staging**

Use the same t9 VPC, subnet, Traefik, and RDS values as dev. Set:

- backend key `ahand-hub/envs/staging/terraform.tfstate`
- default tag `Environment = "staging"`
- module `env = "staging"`
- `ecs_cluster_name = "openclaw-hive-dev"`
- `api_domain = "ahand-hub.staging.team9.ai"`
- `gateway_public_url = "https://api.staging.team9.ai"`

- [ ] **Step 3: Run Terraform validation**

Run:

```bash
terraform fmt -check infra/modules/ahand-hub infra/envs/dev infra/envs/staging
terraform -chdir=infra/envs/staging init -backend=false
terraform -chdir=infra/envs/staging validate
```

Expected: all commands exit 0.

- [ ] **Step 4: Commit Terraform changes**

Run:

```bash
git add infra/envs/staging infra/modules/ahand-hub/variables.tf
git commit -m "infra: add t9 staging hub stack"
```

### Task 3: Wire Staging Deployment

**Files:**
- Modify: `.github/workflows/deploy-hub.yml`
- Modify: `deploy/hub/deploy.sh`
- Modify: `infra/README.md`

- [ ] **Step 1: Extend branch mapping**

Add `staging` to the workflow branch trigger and determine-env block.

- [ ] **Step 2: Extend deploy script**

Allow `staging` and set:

- `ECS_CLUSTER=openclaw-hive-dev`
- `SERVICE_NAME=ahand-hub-staging`
- `API_DOMAIN=ahand-hub.staging.team9.ai`

- [ ] **Step 3: Update runbook**

Document prod/dev/staging, t9 profile/account, staging DNS, state key, and
deploy command.

- [ ] **Step 4: Validate**

Run:

```bash
bash scripts/verify-aws-staging-config.sh
ruby -e 'require "yaml"; YAML.load_file(".github/workflows/deploy-hub.yml"); puts "yaml ok"'
bash -n deploy/hub/deploy.sh
```

Expected: all commands exit 0.

- [ ] **Step 5: Commit deploy wiring**

Run:

```bash
git add .github/workflows/deploy-hub.yml deploy/hub/deploy.sh infra/README.md scripts/verify-aws-staging-config.sh
git commit -m "ci: deploy hub staging on aws t9"
```

### Task 4: Live AWS Staging Bring-Up

**Files:**
- No source changes expected unless validation reveals missing config.

- [ ] **Step 1: Inspect Terraform plan**

Run:

```bash
terraform -chdir=infra/envs/staging init
terraform -chdir=infra/envs/staging plan
```

Expected: planned resources are isolated to `ahand-hub-staging` and
`/ahand-hub/staging/*`.

- [ ] **Step 2: Apply after plan review**

Run: `terraform -chdir=infra/envs/staging apply`

- [ ] **Step 3: Seed runtime secrets**

Write real values for:

- `/ahand-hub/staging/DATABASE_URL`
- `/ahand-hub/staging/SENTRY_DSN`

- [ ] **Step 4: Deploy staging image**

Push the branch and let the `staging` branch deploy, or run
`./deploy/hub/deploy.sh staging` after pushing an image tagged `staging`.

- [ ] **Step 5: Verify live service**

Run AWS ECS checks and `curl -fsS https://ahand-hub.staging.team9.ai/api/health`.
