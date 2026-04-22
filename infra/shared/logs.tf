# Single CloudWatch log group shared by prod and dev ahand-hub tasks.
# Stream prefix (set in the ECS task definition to `ahand-hub-{env}`)
# disambiguates the environments. Mirrors the pattern folder9 uses so a
# single log insights query can filter across both envs.

resource "aws_cloudwatch_log_group" "ahand_hub" {
  name              = "/ecs/ahand-hub"
  retention_in_days = 30
}

output "log_group_name" {
  value = aws_cloudwatch_log_group.ahand_hub.name
}
