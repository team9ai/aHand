output "execution_role_arn" {
  description = "ECS task execution role ARN for the hub service in this env"
  value       = aws_iam_role.execution.arn
}

output "task_role_arn" {
  description = "ECS task role ARN for the hub service in this env"
  value       = aws_iam_role.task.arn
}
