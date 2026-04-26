# prod ahand-hub stack.
# Shared resources (ECR, OIDC deploy role, log group) live in ../../shared.

# Traefik NLB lookup so Cloudflare CNAME documentation stays in sync with
# whatever the LB's DNS name is today.
data "aws_lb" "traefik" {
  name = var.traefik_alb_name
}

module "ahand_hub" {
  source = "../../modules/ahand-hub"

  env                        = "prod"
  ecs_cluster_name           = "openclaw-hive"
  api_domain                 = "ahand-hub.team9.ai"
  openclaw_rds_host          = var.openclaw_rds_host
  vpc_id                     = var.vpc_id
  subnet_ids                 = var.subnet_ids
  traefik_security_group_id  = var.traefik_security_group_id
  gateway_public_url         = "https://gateway.team9.ai"
  redis_mode                 = "create"
}

output "execution_role_arn" {
  value = module.ahand_hub.execution_role_arn
}

output "task_role_arn" {
  value = module.ahand_hub.task_role_arn
}

output "traefik_lb_dns_name" {
  description = "Configure Cloudflare CNAME ahand-hub.team9.ai → this value (DNS-only, gray cloud)"
  value       = data.aws_lb.traefik.dns_name
}
