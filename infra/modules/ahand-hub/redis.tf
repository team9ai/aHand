# Dedicated ElastiCache Redis cluster for the hub (per-env).
#
# No existing ElastiCache was found in the account (see stream-c-audit.md), so
# redis_mode defaults to "create". The reuse branch is kept as dead code for
# future consolidation — flipping var.redis_mode to "reuse" and supplying
# var.existing_redis_cluster_id routes everything through the data block
# without touching resource graph.
#
# Cluster size: cache.t4g.micro, single node. MVP cost budget per spec § 7.9.
# Key namespacing in hub code MUST prefix everything with `ahand:` so any
# future shared-cluster reuse is trivial (see infra/README.md).

resource "aws_elasticache_subnet_group" "ahand_hub" {
  count       = var.redis_mode == "create" ? 1 : 0
  name        = "ahand-hub-${var.env}-cache-subnet"
  description = "Subnets for ahand-hub ${var.env} ElastiCache"
  subnet_ids  = var.subnet_ids
  tags        = local.common_tags
}

resource "aws_security_group" "redis" {
  count       = var.redis_mode == "create" ? 1 : 0
  name        = "ahand-hub-${var.env}-cache-sg"
  description = "Inbound 6379 for ahand-hub ${var.env} Redis from the ECS task SG"
  vpc_id      = var.vpc_id

  # Ingress rule for the ECS task SG is added in ecs.tf via
  # aws_security_group_rule so the two resources stay independently ownable.

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
    description = "All egress"
  }

  tags = local.common_tags
}

resource "aws_elasticache_cluster" "ahand_hub" {
  count                = var.redis_mode == "create" ? 1 : 0
  cluster_id           = "ahand-hub-${var.env}"
  engine               = "redis"
  engine_version       = "7.1"
  node_type            = "cache.t4g.micro"
  num_cache_nodes      = 1
  port                 = 6379
  parameter_group_name = "default.redis7"
  subnet_group_name    = aws_elasticache_subnet_group.ahand_hub[0].name
  security_group_ids   = [aws_security_group.redis[0].id]

  maintenance_window       = "sun:07:00-sun:08:00"
  snapshot_window          = "08:00-09:00"
  snapshot_retention_limit = var.env == "prod" ? 7 : 1

  apply_immediately = false

  tags = local.common_tags
}

# Reuse branch — kept present but count=0 today because no cluster exists to
# attach to. Flip var.redis_mode to "reuse" and populate existing_redis_cluster_id
# to activate.
data "aws_elasticache_cluster" "existing" {
  count      = var.redis_mode == "reuse" ? 1 : 0
  cluster_id = var.existing_redis_cluster_id
}

locals {
  redis_host = var.redis_mode == "create" ? (
    aws_elasticache_cluster.ahand_hub[0].cache_nodes[0].address
  ) : data.aws_elasticache_cluster.existing[0].cache_nodes[0].address

  redis_port = var.redis_mode == "create" ? (
    aws_elasticache_cluster.ahand_hub[0].cache_nodes[0].port
  ) : data.aws_elasticache_cluster.existing[0].cache_nodes[0].port

  redis_url = format("redis://%s:%d", local.redis_host, local.redis_port)

  # Exposed so ecs.tf can add ingress 6379 from the task SG.
  redis_security_group_id = var.redis_mode == "create" ? aws_security_group.redis[0].id : null
}

resource "aws_ssm_parameter" "redis_url" {
  name  = "/ahand-hub/${var.env}/REDIS_URL"
  type  = "SecureString"
  value = local.redis_url
  tags  = local.common_tags
}
