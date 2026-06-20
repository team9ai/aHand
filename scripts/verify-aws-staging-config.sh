#!/usr/bin/env bash
set -euo pipefail

fail() {
  printf 'aws_staging_config=missing: %s\n' "$1" >&2
  exit 1
}

require_file() {
  [[ -f "$1" ]] || fail "$1"
}

require_match() {
  local pattern="$1"
  local file="$2"
  rg -q "$pattern" "$file" || fail "$file lacks $pattern"
}

require_file infra/envs/staging/backend.tf
require_file infra/envs/staging/main.tf
require_file infra/envs/staging/providers.tf
require_file infra/envs/staging/variables.tf
require_file infra/envs/staging/versions.tf

require_match 'contains\(\["prod", "dev", "staging"\], var\.env\)' infra/modules/ahand-hub/variables.tf
require_match 'branches: \[main, dev, staging\]' .github/workflows/deploy-hub.yml
require_match 'refs/heads/staging' .github/workflows/deploy-hub.yml
require_match 'SERVICE_NAME=ahand-hub-staging' .github/workflows/deploy-hub.yml
require_match 'openclaw-hive-dev' .github/workflows/deploy-hub.yml

require_match '\[\[ "\$ENV" == "dev" \|\| "\$ENV" == "staging" \|\| "\$ENV" == "prod" \]\]' deploy/hub/deploy.sh
require_match 'SERVICE_NAME="ahand-hub-staging"' deploy/hub/deploy.sh
require_match 'API_DOMAIN="ahand-hub\.staging\.team9\.ai"' deploy/hub/deploy.sh

require_match 'env[[:space:]]*=[[:space:]]*"staging"' infra/envs/staging/main.tf
require_match 'ecs_cluster_name[[:space:]]*=[[:space:]]*"openclaw-hive-dev"' infra/envs/staging/main.tf
require_match 'api_domain[[:space:]]*=[[:space:]]*"ahand-hub\.staging\.team9\.ai"' infra/envs/staging/main.tf
require_match 'gateway_public_url[[:space:]]*=[[:space:]]*"https://api\.staging\.team9\.ai"' infra/envs/staging/main.tf
require_match 'key[[:space:]]*=[[:space:]]*"ahand-hub/envs/staging/terraform\.tfstate"' infra/envs/staging/backend.tf
require_match 'profile[[:space:]]*=[[:space:]]*"t9"' infra/envs/staging/backend.tf
require_match 'Environment[[:space:]]*=[[:space:]]*"staging"' infra/envs/staging/providers.tf

printf 'aws_staging_config=ok\n'
