data "aws_s3_bucket" "file_ops" {
  count = var.manage_file_ops_bucket ? 0 : 1

  bucket = local.file_ops_bucket_name
}

resource "aws_s3_bucket" "file_ops" {
  count = var.manage_file_ops_bucket ? 1 : 0

  bucket = local.file_ops_bucket_name

  tags = merge(local.common_tags, {
    Name = local.file_ops_bucket_name
  })
}

resource "aws_s3_bucket_public_access_block" "file_ops" {
  count = var.manage_file_ops_bucket ? 1 : 0

  bucket = aws_s3_bucket.file_ops[0].id

  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_ownership_controls" "file_ops" {
  count = var.manage_file_ops_bucket ? 1 : 0

  bucket = aws_s3_bucket.file_ops[0].id

  rule {
    object_ownership = "BucketOwnerEnforced"
  }
}

resource "aws_s3_bucket_server_side_encryption_configuration" "file_ops" {
  count = var.manage_file_ops_bucket ? 1 : 0

  bucket = aws_s3_bucket.file_ops[0].id

  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm = "AES256"
    }
  }
}

resource "aws_s3_bucket_lifecycle_configuration" "file_ops" {
  count = var.manage_file_ops_bucket ? 1 : 0

  bucket = aws_s3_bucket.file_ops[0].id

  rule {
    id     = "expire-file-ops-staging"
    status = "Enabled"

    filter {
      prefix = "file-ops/"
    }

    expiration {
      days = var.s3_file_ops_lifecycle_days
    }

    abort_incomplete_multipart_upload {
      days_after_initiation = 1
    }
  }
}

moved {
  from = aws_s3_bucket.file_ops
  to   = aws_s3_bucket.file_ops[0]
}

moved {
  from = aws_s3_bucket_public_access_block.file_ops
  to   = aws_s3_bucket_public_access_block.file_ops[0]
}

moved {
  from = aws_s3_bucket_ownership_controls.file_ops
  to   = aws_s3_bucket_ownership_controls.file_ops[0]
}

moved {
  from = aws_s3_bucket_server_side_encryption_configuration.file_ops
  to   = aws_s3_bucket_server_side_encryption_configuration.file_ops[0]
}

moved {
  from = aws_s3_bucket_lifecycle_configuration.file_ops
  to   = aws_s3_bucket_lifecycle_configuration.file_ops[0]
}

locals {
  selected_file_ops_bucket_arn  = var.manage_file_ops_bucket ? aws_s3_bucket.file_ops[0].arn : data.aws_s3_bucket.file_ops[0].arn
  selected_file_ops_bucket_id   = var.manage_file_ops_bucket ? aws_s3_bucket.file_ops[0].id : data.aws_s3_bucket.file_ops[0].id
  selected_file_ops_bucket_name = var.manage_file_ops_bucket ? aws_s3_bucket.file_ops[0].bucket : data.aws_s3_bucket.file_ops[0].bucket
}

resource "aws_ssm_parameter" "s3_bucket" {
  name  = "/ahand-hub/${var.env}/S3_BUCKET"
  type  = "String"
  value = local.selected_file_ops_bucket_name
  tags  = local.common_tags
}

resource "aws_ssm_parameter" "s3_region" {
  name  = "/ahand-hub/${var.env}/S3_REGION"
  type  = "String"
  value = var.aws_region
  tags  = local.common_tags
}

resource "aws_ssm_parameter" "s3_threshold_bytes" {
  name  = "/ahand-hub/${var.env}/S3_THRESHOLD_BYTES"
  type  = "String"
  value = tostring(var.s3_file_transfer_threshold_bytes)
  tags  = local.common_tags
}

resource "aws_ssm_parameter" "s3_url_expiration_secs" {
  name  = "/ahand-hub/${var.env}/S3_URL_EXPIRATION_SECS"
  type  = "String"
  value = tostring(var.s3_url_expiration_secs)
  tags  = local.common_tags
}
