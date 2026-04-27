terraform {
  backend "s3" {
    bucket         = "weightwave-tfstate"
    key            = "ahand-hub/shared/terraform.tfstate"
    region         = "us-east-1"
    profile        = "ww"
    dynamodb_table = "terraform-state-lock"
    encrypt        = true
  }
}
