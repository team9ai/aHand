# ECS Fargate service stub for ahand-hub.
#
# Strategy: declare the service + a minimal stub task definition so the
# Stream A deploy-hub.yml workflow can target `aws ecs update-service
# --force-new-deployment` on a pre-existing service on its first run.
# The stub uses amazon/amazon-ecs-sample:latest (always pullable) so the
# service doesn't crash-loop before the first real deploy lands.
#
# Terraform intentionally stops at the stub:
#   lifecycle.ignore_changes = [container_definitions]  (task def)
#   lifecycle.ignore_changes = [task_definition, desired_count]  (service)
# lets deploy-hub.yml register fresh task definition revisions and point
# the service at them without Terraform reverting the state on the next
# apply.

# ECS task security group — Traefik hits port 1515 on the task's private
# IP (task is placed in the same openclaw-hive VPC; assign_public_ip=true
# only because the VPC has no private subnets). All egress open so the
# task can reach RDS (5432), Redis (6379), SSM, CloudWatch, and the
# gateway webhook.
resource "aws_security_group" "task" {
  name        = "ahand-hub-${var.env}-task-sg"
  description = "Ingress from Traefik (1515) for the ahand-hub ${var.env} ECS service"
  vpc_id      = var.vpc_id

  ingress {
    from_port       = 1515
    to_port         = 1515
    protocol        = "tcp"
    security_groups = [var.traefik_security_group_id]
    description     = "From Traefik"
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
    description = "All egress (RDS, Redis, SSM, CloudWatch, gateway webhook)"
  }

  tags = local.common_tags
}

# Attach the task SG to the Redis SG's ingress on 6379. Kept as a separate
# aws_security_group_rule so Tasks 3.4 (Redis) and 3.6 (ECS) stay
# independently removable.
resource "aws_security_group_rule" "redis_ingress_from_task" {
  count                    = var.redis_mode == "create" ? 1 : 0
  type                     = "ingress"
  from_port                = 6379
  to_port                  = 6379
  protocol                 = "tcp"
  security_group_id        = local.redis_security_group_id
  source_security_group_id = aws_security_group.task.id
  description              = "ahand-hub-${var.env} ECS task"
}

resource "aws_ecs_task_definition" "stub" {
  family                   = "ahand-hub-${var.env}"
  cpu                      = var.env == "prod" ? "512" : "256"
  memory                   = var.env == "prod" ? "1024" : "512"
  network_mode             = "awsvpc"
  requires_compatibilities = ["FARGATE"]
  execution_role_arn       = aws_iam_role.execution.arn
  task_role_arn            = aws_iam_role.task.arn

  # Placeholder only — deploy-hub.yml registers fresh revisions on every
  # push to main/dev with the real image + secrets + Traefik labels.
  container_definitions = jsonencode([
    {
      name         = "ahand-hub"
      image        = "amazon/amazon-ecs-sample:latest"
      essential    = true
      portMappings = [{ containerPort = 1515, protocol = "tcp" }]
      logConfiguration = {
        logDriver = "awslogs"
        options = {
          awslogs-group         = "/ecs/ahand-hub"
          awslogs-region        = var.aws_region
          awslogs-stream-prefix = "ahand-hub-${var.env}"
        }
      }
    }
  ])

  lifecycle {
    ignore_changes = [container_definitions]
  }

  tags = local.common_tags
}

resource "aws_ecs_service" "ahand_hub" {
  name            = "ahand-hub-${var.env}"
  cluster         = var.ecs_cluster_name
  task_definition = aws_ecs_task_definition.stub.arn
  desired_count   = 1
  launch_type     = "FARGATE"

  network_configuration {
    subnets          = var.subnet_ids
    security_groups  = [aws_security_group.task.id]
    # VPC has only public subnets → Fargate tasks need public IPs to reach
    # ECR / SSM / CloudWatch via the IGW. Traefik still targets the private
    # IP via Docker labels; the public IP is outbound-only in practice.
    assign_public_ip = true
  }

  deployment_maximum_percent         = 200
  deployment_minimum_healthy_percent = 0

  lifecycle {
    ignore_changes = [task_definition, desired_count]
  }

  tags = local.common_tags
}

output "ecs_service_name" {
  value = aws_ecs_service.ahand_hub.name
}

output "ecs_task_definition_family" {
  value = aws_ecs_task_definition.stub.family
}

output "ecs_task_security_group_id" {
  value = aws_security_group.task.id
}
