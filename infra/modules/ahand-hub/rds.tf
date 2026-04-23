# RDS connection string for the ahand_hub_{env} database.
#
# ahand-hub piggybacks on the shared openclaw-hive-{env} Postgres instance.
# The database (ahand_hub_{env}) and scoped role (ahand_hub_{env}) are created
# by the operator out-of-band — see infra/README.md "Database bootstrap".
#
# Why manual: the openclaw admin password is NOT stored in SSM or Secrets
# Manager in this account, so wiring the cyrilgdn/postgresql Terraform
# provider would require plumbing the password as a TF_VAR with no storage
# path that's both shared-workstation-safe and CI-safe. Declarative ownership
# of a per-env hub database is low-churn (one row in the RDS catalog) and is
# acceptable to manage by hand in exchange for keeping admin credentials
# out of Terraform.
#
# The SSM resource is declared here so drift is still detected (missing
# parameter will show as a diff) but lifecycle.ignore_changes lets the
# operator bootstrap the real value via `aws ssm put-parameter` without
# Terraform reverting it on subsequent applies.

resource "aws_ssm_parameter" "database_url" {
  name  = "/ahand-hub/${var.env}/DATABASE_URL"
  type  = "SecureString"
  value = "PLACEHOLDER_SET_MANUALLY_VIA_PSQL_BOOTSTRAP"
  tags  = local.common_tags

  lifecycle {
    ignore_changes = [value]
  }
}
