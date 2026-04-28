variable "env" {
  description = "Deployment environment"
  type        = string
  validation {
    condition     = contains(["prod", "dev"], var.env)
    error_message = "env must be 'prod' or 'dev'"
  }
}

variable "aws_region" {
  description = "AWS region"
  type        = string
  default     = "us-east-1"
}

variable "aws_account_id" {
  description = "AWS account ID"
  type        = string
  default     = "471112576951"
}

variable "ecs_cluster_name" {
  description = "ECS cluster the ahand-hub service runs in"
  type        = string
}

variable "api_domain" {
  description = "Public domain for the hub API (also used by the team9 gateway AHAND_HUB_URL mirror)"
  type        = string
}

variable "openclaw_rds_host" {
  description = "openclaw-hive RDS endpoint host (passed in from the env stack)"
  type        = string
}

variable "openclaw_rds_port" {
  description = "openclaw-hive RDS port"
  type        = number
  default     = 5432
}

variable "openclaw_rds_security_group_id" {
  description = "Security group attached to the openclaw-hive RDS instance. Required so the ahand-hub ECS task SG can be authorized inbound on port 5432."
  type        = string
}

variable "vpc_id" {
  description = "VPC that hosts ECS tasks, RDS, and Redis"
  type        = string
}

variable "subnet_ids" {
  description = "Subnets for ECS tasks + ElastiCache"
  type        = list(string)
}

variable "traefik_security_group_id" {
  description = "Security group ID of the Traefik containers that will call into the hub task on port 1515"
  type        = string
}

variable "gateway_public_url" {
  description = "team9 gateway base URL (used to build the outbound WEBHOOK_URL the hub posts events to)"
  type        = string
}

variable "redis_mode" {
  description = "'create' to provision a dedicated ElastiCache cluster, 'reuse' to attach to an existing one"
  type        = string
  default     = "create"
  validation {
    condition     = contains(["create", "reuse"], var.redis_mode)
    error_message = "redis_mode must be 'create' or 'reuse'"
  }
}

variable "existing_redis_cluster_id" {
  description = "Existing ElastiCache cluster ID when redis_mode = reuse"
  type        = string
  default     = null
}

locals {
  common_tags = {
    Environment = var.env
    Service     = "ahand-hub"
    ManagedBy   = "Terraform"
  }
}
