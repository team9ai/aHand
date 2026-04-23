# ahand-hub shared (account-wide) resources.
#
# What lives here:
#   - GitHubActionsAhandHubDeploy IAM role trusted via OIDC by team9ai/ahand
#     (task 3.1, this file)
#   - ahand-hub ECR repository (task 3.2, ecr.tf)
#   - /ecs/ahand-hub CloudWatch log group (task 3.5, logs.tf)
#
# Resources that differ per environment (IAM execution/task roles, ECS service,
# Redis cluster, SSM parameters) live in ../modules/ahand-hub and are
# instantiated once from each envs/{prod,dev} stack.

# OIDC provider is shared with folder9 / other team9 services; folder9 created
# it earlier. Reuse via data lookup — never create a second one.
data "aws_iam_openid_connect_provider" "github" {
  url = "https://token.actions.githubusercontent.com"
}

resource "aws_iam_role" "github_actions_deploy" {
  name        = "GitHubActionsAhandHubDeploy"
  description = "Assumed via OIDC by .github/workflows/deploy-hub.yml in ${var.github_repo}"

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Principal = { Federated = data.aws_iam_openid_connect_provider.github.arn }
      Action    = "sts:AssumeRoleWithWebIdentity"
      Condition = {
        StringEquals = {
          "token.actions.githubusercontent.com:aud" = "sts.amazonaws.com"
        }
        StringLike = {
          "token.actions.githubusercontent.com:sub" = [
            "repo:${var.github_repo}:ref:refs/heads/main",
            "repo:${var.github_repo}:ref:refs/heads/dev",
          ]
        }
      }
    }]
  })
}

resource "aws_iam_role_policy" "github_actions_deploy" {
  name = "GitHubActionsAhandHubDeployPolicy"
  role = aws_iam_role.github_actions_deploy.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid      = "ECRAuth"
        Effect   = "Allow"
        Action   = ["ecr:GetAuthorizationToken"]
        Resource = "*"
      },
      {
        Sid    = "ECRPush"
        Effect = "Allow"
        Action = [
          "ecr:BatchCheckLayerAvailability",
          "ecr:GetDownloadUrlForLayer",
          "ecr:BatchGetImage",
          "ecr:InitiateLayerUpload",
          "ecr:UploadLayerPart",
          "ecr:CompleteLayerUpload",
          "ecr:PutImage",
          "ecr:DescribeRepositories",
          "ecr:DescribeImages",
        ]
        Resource = "arn:aws:ecr:${var.aws_region}:${var.aws_account_id}:repository/ahand-hub"
      },
      {
        Sid    = "ECSDeploy"
        Effect = "Allow"
        Action = [
          "ecs:RegisterTaskDefinition",
          "ecs:DeregisterTaskDefinition",
          "ecs:UpdateService",
          "ecs:DescribeServices",
          "ecs:DescribeTaskDefinition",
          "ecs:DescribeTasks",
          "ecs:ListTasks",
        ]
        Resource = "*"
      },
      {
        Sid      = "PassAhandHubRoles"
        Effect   = "Allow"
        Action   = ["iam:PassRole"]
        Resource = [
          "arn:aws:iam::${var.aws_account_id}:role/ahand-hub-prod-execution",
          "arn:aws:iam::${var.aws_account_id}:role/ahand-hub-dev-execution",
          "arn:aws:iam::${var.aws_account_id}:role/ahand-hub-prod-task",
          "arn:aws:iam::${var.aws_account_id}:role/ahand-hub-dev-task",
        ]
      },
      {
        Sid    = "ReadAhandHubSSM"
        Effect = "Allow"
        Action = [
          "ssm:GetParameter",
          "ssm:GetParameters",
          "ssm:DescribeParameters",
        ]
        Resource = [
          "arn:aws:ssm:${var.aws_region}:${var.aws_account_id}:parameter/ahand-hub/*",
        ]
      },
    ]
  })
}
