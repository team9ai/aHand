data "aws_lb" "traefik" {
  name = var.traefik_alb_name
}

module "ahand_hub" {
  source = "../../modules/ahand-hub"

  # t9 staging is isolated from dev at ECS/SSM/Redis/IAM while reusing the
  # non-production cluster and network shape.
  env                            = "staging"
  ecs_cluster_name               = "openclaw-hive-dev"
  api_domain                     = "ahand-hub.staging.team9.ai"
  openclaw_rds_host              = var.openclaw_rds_host
  openclaw_rds_security_group_id = var.openclaw_rds_security_group_id
  vpc_id                         = var.vpc_id
  subnet_ids                     = var.subnet_ids
  traefik_security_group_id      = var.traefik_security_group_id
  gateway_public_url             = "https://api.staging.team9.ai"
  redis_mode                     = "create"
}

output "execution_role_arn" {
  value = module.ahand_hub.execution_role_arn
}

output "task_role_arn" {
  value = module.ahand_hub.task_role_arn
}

output "file_ops_bucket_name" {
  value = module.ahand_hub.file_ops_bucket_name
}

output "traefik_lb_dns_name" {
  description = "Configure Cloudflare CNAME ahand-hub.staging.team9.ai -> this value (DNS-only, gray cloud)"
  value       = data.aws_lb.traefik.dns_name
}
