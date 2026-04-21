output "deploy_role_arn" {
  description = "ARN of the GitHubActionsAhandHubDeploy role — use this in GitHub Actions secrets"
  value       = aws_iam_role.github_actions_deploy.arn
}
