# ahand-hub container image registry.
#
# One account-wide repo serves both prod and dev; the image tag
# (prod / dev / <sha>) distinguishes environments. deploy-hub.yml pushes
# both a semantic tag and an immutable SHA tag on every deploy so we can
# roll back to any recent commit even after the semantic tag is reused.

resource "aws_ecr_repository" "ahand_hub" {
  name                 = "ahand-hub"
  image_tag_mutability = "MUTABLE"

  image_scanning_configuration {
    scan_on_push = true
  }

  encryption_configuration {
    encryption_type = "AES256"
  }
}

resource "aws_ecr_lifecycle_policy" "ahand_hub" {
  repository = aws_ecr_repository.ahand_hub.name

  # Two rules, evaluated in priority order:
  #   1. Tagged prod/dev images: never expire (countMoreThan=10000 approximates
  #      "keep all" — ECR has no native "retain indefinitely" selector).
  #   2. Untagged images (SHA-only builds from merged PRs + old rollback targets):
  #      keep the most recent 30, delete the rest.
  policy = jsonencode({
    rules = [
      {
        rulePriority = 1
        description  = "Keep prod and dev tagged images indefinitely"
        selection = {
          tagStatus     = "tagged"
          tagPrefixList = ["prod", "dev"]
          countType     = "imageCountMoreThan"
          countNumber   = 10000
        }
        action = { type = "expire" }
      },
      {
        rulePriority = 2
        description  = "Keep last 30 untagged (SHA-only) images"
        selection = {
          tagStatus   = "untagged"
          countType   = "imageCountMoreThan"
          countNumber = 30
        }
        action = { type = "expire" }
      },
    ]
  })
}
