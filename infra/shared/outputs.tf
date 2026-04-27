output "deploy_role_arn" {
  description = "ARN of the GitHubActionsAhandHubDeploy role — use this in GitHub Actions secrets"
  value       = aws_iam_role.github_actions_deploy.arn
}

output "ecr_repository_url" {
  description = "ECR repo URL for docker push / pull"
  value       = aws_ecr_repository.ahand_hub.repository_url
}

output "ecr_repository_arn" {
  description = "ECR repo ARN"
  value       = aws_ecr_repository.ahand_hub.arn
}
