# Stream C — AWS Preflight Audit

Recorded 2026-04-22 on behalf of the ahand-hub Terraform onboarding.
This is the authoritative reference for decisions encoded in `infra/`.

## Account

| Field | Value |
|---|---|
| AWS account | `471112576951` (from `sts get-caller-identity` with profile `ww`) |
| Region | `us-east-1` |
| AWS profile (local) | `ww` |
| Terraform version | 1.14.8 (≥ 1.6.0 required) |

## ECS clusters

| Environment | Cluster name | Status |
|---|---|---|
| prod | `openclaw-hive` | ACTIVE |
| dev  | `openclaw-hive-dev` | ACTIVE |

## RDS instances

| Environment | Identifier | Host | Port | VPC |
|---|---|---|---|---|
| prod | `openclaw-hive-prod` | `openclaw-hive-prod.chq8i2se49qd.us-east-1.rds.amazonaws.com` | 5432 | `vpc-028cd35f94f14d52b` |
| dev  | `openclaw-hive-dev`  | `openclaw-hive-dev.chq8i2se49qd.us-east-1.rds.amazonaws.com`  | 5432 | `vpc-028cd35f94f14d52b` |

Instance names differ from the plan's assumption (`openclaw-hive-db`). Both instances
are `MasterUsername=openclaw`, `PubliclyAccessible=true`, `MasterUserSecret=null` —
the admin password is NOT stored in Secrets Manager or SSM that Stream C can see.

**Decision (approved by operator, 2026-04-22): Task 3.3 follows the folder9 pattern
— declare `/ahand-hub/{env}/DATABASE_URL` as a placeholder SSM SecureString with
`lifecycle { ignore_changes = [value] }` and document the manual
`psql + aws ssm put-parameter` sequence in the module README.** No postgresql
provider is wired in; database + role are created by the operator out-of-band.

## ElastiCache Redis

`describe-cache-clusters` returns `[]`. Interestingly, two SGs named
`control-plane-redis-{prod,dev}-sg` already exist (sg-056575047643f3a50,
sg-067389c463f125cde) — possibly reserved by an older provision that was
never applied, possibly prepared for a future control-plane Redis.

**Decision: Task 3.4 walks the `redis_mode = "create"` branch**, provisioning
a dedicated `ahand-hub-{env}` `cache.t4g.micro` cluster with its own SG
(`ahand-hub-{env}-cache-sg`) — no accidental reuse of the control-plane SGs.
Redis subnet group is created fresh from the same VPC subnets used by ECS.

## GitHub OIDC provider

Already exists: `arn:aws:iam::471112576951:oidc-provider/token.actions.githubusercontent.com`.
folder9 created it with `create_oidc_provider = true` in its shared stack.

**Decision: Task 3.1 uses `data "aws_iam_openid_connect_provider" "github"`**
to reference the existing provider — do not declare a new one.

## Traefik load balancers

| Environment | LB name | Type | DNS | Zone |
|---|---|---|---|---|
| prod | `traefik-nlb` | NLB (internet-facing) | `traefik-nlb-9d708d124f9805ad.elb.us-east-1.amazonaws.com` | `Z26RNL4JYFTOTI` |
| dev  | `traefik-dev-nlb` | NLB (internet-facing) | `traefik-dev-nlb-8cda97ce6b37e5e1.elb.us-east-1.amazonaws.com` | `Z26RNL4JYFTOTI` |

Plan refers to "ALB" but reality is NLB — `data "aws_lb"` lookup still works
regardless of LB type, so no code change is needed.

## Traefik security groups

| Environment | SG name | ID |
|---|---|---|
| prod | `traefik-sg` | `sg-0ffd97a77fdcbae8f` |
| dev  | `traefik-dev-sg` | `sg-07050efac1b71052b` |

Current ingress on `traefik-sg`: ports 80 / 443 / 8080 from `0.0.0.0/0`. Traefik
reads container labels from the ECS task metadata and proxies to the task's
private IP. ECS task SGs therefore need ingress on port 1515 from the Traefik
SG. Task 3.6 creates per-service `ahand-hub-{env}-task-sg` that allows 1515
from the Traefik SG only.

## VPC + subnets

| Field | Value |
|---|---|
| VPC | `vpc-028cd35f94f14d52b` (Name `openclaw-hive-vpc`, CIDR `10.0.0.0/16`) |

Four subnets, all associated to the single public route table
(`openclaw-hive-public-rt`) that routes 0.0.0.0/0 to IGW. There is no
separate private subnet tier. folder9 uses subnets `public-2a` / `public-2b`
for its ECS tasks with `assign_public_ip = true`. We mirror that choice.

| Use | Subnets |
|---|---|
| ECS tasks | `subnet-09eb68ab6cae3c581` (public-2a), `subnet-0c8ca567a8d4def31` (public-2b) |
| RDS (control-plane-prod group) | `subnet-01c64f89d80d10ab4` (public-1b), `subnet-09e6f38adced71f46` (public-1a) |

**Decision: ECS tasks use public subnets with `assign_public_ip = true`**,
matching folder9. The Redis subnet group uses the same two subnets as ECS
(public-2a / public-2b) so the ECS task SG can reach cache nodes intra-VPC.

## Route53

`list-hosted-zones` returns an empty list. **team9.ai is NOT managed in this
AWS account.** folder9's runbook states the DNS layer is Cloudflare (CNAMEs
configured as "DNS-only / gray cloud" pointing at the NLB DNS names).

**Decision (approved by operator, 2026-04-22): skip the Route53 module for
Task 3.5.** No `dns.tf` is written. `infra/README.md` documents the required
Cloudflare CNAMEs that the operator must configure by hand:

| Host | → | Target |
|---|---|---|
| `ahand-hub.team9.ai` | CNAME | `traefik-nlb-9d708d124f9805ad.elb.us-east-1.amazonaws.com` |
| `ahand-hub.dev.team9.ai` | CNAME | `traefik-dev-nlb-8cda97ce6b37e5e1.elb.us-east-1.amazonaws.com` |

Both records must be DNS-only (not proxied) so Traefik's LetsEncrypt HTTP-01
challenge can complete.

## SSM

- `/ahand-hub/*` tree: empty (will be populated by this stream).
- `/team9/*` tree: empty today; Stream D will rely on the `/team9/{env}/AHAND_HUB_*`
  mirrors that Task 3.5 writes.
- `/folder9/{prod,dev}/` tree: present, owned by folder9 — not touched.
- No `/openclaw-hive/*` entries at all (confirming the RDS admin password is
  stored outside SSM).

## Terraform state

- Bucket: `weightwave-tfstate` (existing, owned by folder9 / team9 infra).
- Lock table: `terraform-state-lock` (DynamoDB, existing).
- Keys (new, unique within bucket):
  - `ahand-hub/shared/terraform.tfstate`
  - `ahand-hub/envs/prod/terraform.tfstate`
  - `ahand-hub/envs/dev/terraform.tfstate`

No new bucket or lock table created — reuse only.

## Summary of deviations from the plan

| Plan assumption | Actual | Treatment |
|---|---|---|
| RDS id `openclaw-hive-db` | `openclaw-hive-{prod,dev}` | Use real names in variables/data |
| `data "aws_route53_zone" "team9"` | team9.ai not in Route53 | Skip `dns.tf`; document Cloudflare CNAMEs |
| `/openclaw-hive/rds/admin-password` SSM | Not present | No postgresql provider; manual db+role via psql |
| ALB | NLB | `data "aws_lb"` works for both |
| `subnet tag:Tier=private` | All subnets public | Use folder9's subnet choice + `assign_public_ip=true` |
| `ahand-hub-shared` module wrapper | Merged into `infra/shared/` stack directly | Matches folder9 structure |
