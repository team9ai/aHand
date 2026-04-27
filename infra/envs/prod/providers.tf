provider "aws" {
  region  = "us-east-1"
  profile = "ww"

  default_tags {
    tags = {
      Environment = "prod"
      Service     = "ahand-hub"
      ManagedBy   = "Terraform"
    }
  }
}
