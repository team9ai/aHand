variable "aws_account_id" {
  description = "AWS account ID"
  type        = string
  default     = "471112576951"
}

variable "aws_region" {
  description = "AWS region"
  type        = string
  default     = "us-east-1"
}

variable "github_repo" {
  description = "GitHub repository (owner/name) trusted to assume the deploy role"
  type        = string
  default     = "team9ai/ahand"
}
