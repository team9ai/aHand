data "aws_lb" "traefik" {
  name = var.traefik_alb_name
}

module "ahand_hub" {
  source = "../../modules/ahand-hub"

  env                        = "dev"
  ecs_cluster_name           = "openclaw-hive-dev"
  api_domain                 = "ahand-hub.dev.team9.ai"
  openclaw_rds_host          = var.openclaw_rds_host
  vpc_id                     = var.vpc_id
  subnet_ids                 = var.subnet_ids
  traefik_security_group_id  = var.traefik_security_group_id
  # Note: team9 gateway is at api.dev.team9.ai (not gateway.dev.*). Path
  # is /api/v1/ahand/hub-webhook because the gateway has URI versioning
  # with defaultVersion='1'. The ssm.tf module concats ${gateway_public_url}
  # with the webhook path; set this to the BASE (no path), which the
  # module already expects.
  gateway_public_url         = "https://api.dev.team9.ai"
  redis_mode                 = "create"
}

output "execution_role_arn" {
  value = module.ahand_hub.execution_role_arn
}

output "task_role_arn" {
  value = module.ahand_hub.task_role_arn
}

output "traefik_lb_dns_name" {
  description = "Configure Cloudflare CNAME ahand-hub.dev.team9.ai → this value (DNS-only, gray cloud)"
  value       = data.aws_lb.traefik.dns_name
}
