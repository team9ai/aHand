terraform {
  backend "s3" {
    bucket         = "team9-tfstate"
    key            = "ahand-hub/envs/staging/terraform.tfstate"
    region         = "us-east-1"
    profile        = "t9"
    dynamodb_table = "terraform-state-lock"
    encrypt        = true
  }
}
