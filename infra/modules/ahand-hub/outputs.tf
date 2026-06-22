output "execution_role_arn" {
  description = "ECS task execution role ARN for the hub service in this env"
  value       = aws_iam_role.execution.arn
}

output "task_role_arn" {
  description = "ECS task role ARN for the hub service in this env"
  value       = aws_iam_role.task.arn
}

output "file_ops_bucket_name" {
  description = "S3 bucket used by hub file operation staging in this env"
  value       = local.selected_file_ops_bucket_name
}
