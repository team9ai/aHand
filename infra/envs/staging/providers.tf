provider "aws" {
  region  = "us-east-1"
  profile = "t9"

  default_tags {
    tags = {
      Environment = "staging"
      Service     = "ahand-hub"
      ManagedBy   = "Terraform"
    }
  }
}
