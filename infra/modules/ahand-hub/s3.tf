resource "aws_s3_bucket" "file_ops" {
  bucket = local.file_ops_bucket_name

  tags = merge(local.common_tags, {
    Name = local.file_ops_bucket_name
  })
}

resource "aws_s3_bucket_public_access_block" "file_ops" {
  bucket = aws_s3_bucket.file_ops.id

  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_ownership_controls" "file_ops" {
  bucket = aws_s3_bucket.file_ops.id

  rule {
    object_ownership = "BucketOwnerEnforced"
  }
}

resource "aws_s3_bucket_server_side_encryption_configuration" "file_ops" {
  bucket = aws_s3_bucket.file_ops.id

  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm = "AES256"
    }
  }
}

resource "aws_s3_bucket_lifecycle_configuration" "file_ops" {
  bucket = aws_s3_bucket.file_ops.id

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

resource "aws_ssm_parameter" "s3_bucket" {
  name  = "/ahand-hub/${var.env}/S3_BUCKET"
  type  = "String"
  value = aws_s3_bucket.file_ops.bucket
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
