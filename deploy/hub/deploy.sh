#!/bin/bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

ENV="${1:-}"
[[ "$ENV" == "dev" || "$ENV" == "prod" ]] || { echo "Usage: $0 {dev|prod}"; exit 1; }

AWS_REGION="us-east-1"
ACCOUNT_ID="471112576951"
ECR_REGISTRY="${ACCOUNT_ID}.dkr.ecr.${AWS_REGION}.amazonaws.com"
ECR_REPO="ahand-hub"
GIT_SHA="${GIT_SHA:-$(git rev-parse --short HEAD)}"

if [[ "$ENV" == "prod" ]]; then
  ECS_CLUSTER="openclaw-hive"
  SERVICE_NAME="ahand-hub-prod"
  API_DOMAIN="ahand-hub.team9.ai"
else
  ECS_CLUSTER="openclaw-hive-dev"
  SERVICE_NAME="ahand-hub-dev"
  API_DOMAIN="ahand-hub.dev.team9.ai"
fi

ECR_IMAGE="${ECR_REGISTRY}/${ECR_REPO}:${ENV}"
SSM_PREFIX="arn:aws:ssm:${AWS_REGION}:${ACCOUNT_ID}:parameter/ahand-hub/${ENV}"

# Ensure log group exists (idempotent — safe to run even if Terraform hasn't provisioned it yet)
aws logs create-log-group --region "$AWS_REGION" \
  --log-group-name /ecs/ahand-hub 2>/dev/null || true
EXECUTION_ROLE_ARN="arn:aws:iam::${ACCOUNT_ID}:role/ahand-hub-${ENV}-execution"
TASK_ROLE_ARN="arn:aws:iam::${ACCOUNT_ID}:role/ahand-hub-${ENV}-task"

RENDERED=$(mktemp)
trap 'rm -f "$RENDERED"' EXIT

sed \
  -e "s|\${ENV}|${ENV}|g" \
  -e "s|\${ECR_IMAGE}|${ECR_IMAGE}|g" \
  -e "s|\${EXECUTION_ROLE_ARN}|${EXECUTION_ROLE_ARN}|g" \
  -e "s|\${TASK_ROLE_ARN}|${TASK_ROLE_ARN}|g" \
  -e "s|\${API_DOMAIN}|${API_DOMAIN}|g" \
  -e "s|\${SSM_PREFIX}|${SSM_PREFIX}|g" \
  -e "s|\${AWS_REGION}|${AWS_REGION}|g" \
  -e "s|\${GIT_SHA}|${GIT_SHA}|g" \
  "${SCRIPT_DIR}/task-definition.template.json" > "$RENDERED"

aws ecs register-task-definition --region "$AWS_REGION" \
  --cli-input-json "file://${RENDERED}" > /dev/null
aws ecs update-service --region "$AWS_REGION" \
  --cluster "$ECS_CLUSTER" --service "$SERVICE_NAME" \
  --task-definition "ahand-hub-${ENV}" --force-new-deployment > /dev/null
aws ecs wait services-stable --region "$AWS_REGION" \
  --cluster "$ECS_CLUSTER" --services "$SERVICE_NAME"
