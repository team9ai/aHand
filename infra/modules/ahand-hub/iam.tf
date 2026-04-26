# Per-env IAM roles for the ahand-hub ECS service.
#
# - execution role: used by the Fargate agent to pull the image, write to
#   /ecs/ahand-hub CloudWatch logs, and inject SSM parameters at task start.
# - task role: assumed by the running container for any app-level AWS calls.
#   No AWS calls are required by the MVP hub binary, so the policy is minimal
#   (the sts:GetCallerIdentity noop keeps the role+policy resource graph valid).

locals {
  execution_role_name = "ahand-hub-${var.env}-execution"
  task_role_name      = "ahand-hub-${var.env}-task"
  ssm_prefix          = "arn:aws:ssm:${var.aws_region}:${var.aws_account_id}:parameter/ahand-hub/${var.env}"
  log_group_arn       = "arn:aws:logs:${var.aws_region}:${var.aws_account_id}:log-group:/ecs/ahand-hub"
  ecr_repo_arn        = "arn:aws:ecr:${var.aws_region}:${var.aws_account_id}:repository/ahand-hub"
}

data "aws_iam_policy_document" "ecs_assume" {
  statement {
    actions = ["sts:AssumeRole"]
    principals {
      type        = "Service"
      identifiers = ["ecs-tasks.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "execution" {
  name               = local.execution_role_name
  assume_role_policy = data.aws_iam_policy_document.ecs_assume.json
  tags               = local.common_tags
}

resource "aws_iam_role_policy_attachment" "execution_managed" {
  role       = aws_iam_role.execution.name
  policy_arn = "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy"
}

resource "aws_iam_role_policy" "execution" {
  name = "${local.execution_role_name}-policy"
  role = aws_iam_role.execution.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid    = "ECRPull"
        Effect = "Allow"
        Action = [
          "ecr:GetAuthorizationToken",
          "ecr:BatchCheckLayerAvailability",
          "ecr:GetDownloadUrlForLayer",
          "ecr:BatchGetImage",
        ]
        Resource = "*"
      },
      {
        Sid    = "CloudWatchLogs"
        Effect = "Allow"
        Action = [
          "logs:CreateLogStream",
          "logs:PutLogEvents",
        ]
        Resource = "${local.log_group_arn}:*"
      },
      {
        Sid    = "ReadSSM"
        Effect = "Allow"
        Action = [
          "ssm:GetParameters",
          "ssm:GetParameter",
        ]
        Resource = "${local.ssm_prefix}/*"
      },
      {
        Sid      = "DecryptSSM"
        Effect   = "Allow"
        Action   = ["kms:Decrypt"]
        Resource = "arn:aws:kms:${var.aws_region}:${var.aws_account_id}:key/*"
        Condition = {
          StringEquals = {
            "kms:ViaService" = "ssm.${var.aws_region}.amazonaws.com"
          }
        }
      },
    ]
  })
}

resource "aws_iam_role" "task" {
  name               = local.task_role_name
  assume_role_policy = data.aws_iam_policy_document.ecs_assume.json
  tags               = local.common_tags
}

resource "aws_iam_role_policy" "task" {
  name = "${local.task_role_name}-policy"
  role = aws_iam_role.task.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Sid      = "Noop"
      Effect   = "Allow"
      Action   = ["sts:GetCallerIdentity"]
      Resource = "*"
    }]
  })
}
