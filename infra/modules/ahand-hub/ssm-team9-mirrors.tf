# team9 gateway SSM mirrors.
#
# The team9 gateway reads AHAND_HUB_{URL,SERVICE_TOKEN,WEBHOOK_SECRET} via
# the /team9/{env}/ prefix so its own task definition never has to reach
# across into /ahand-hub/. These mirrors share the same random_password
# objects as the source parameters, so rotating a secret on the hub side
# (terraform taint random_password.service_token && apply) cascades to
# the gateway automatically with no cross-repo sync.

resource "aws_ssm_parameter" "team9_ahand_hub_url" {
  name  = "/team9/${var.env}/AHAND_HUB_URL"
  type  = "String"
  value = "https://${var.api_domain}"
  tags  = local.common_tags
}

resource "aws_ssm_parameter" "team9_ahand_hub_service_token" {
  name  = "/team9/${var.env}/AHAND_HUB_SERVICE_TOKEN"
  type  = "SecureString"
  value = random_password.service_token.result
  tags  = local.common_tags
}

resource "aws_ssm_parameter" "team9_ahand_hub_webhook_secret" {
  name  = "/team9/${var.env}/AHAND_HUB_WEBHOOK_SECRET"
  type  = "SecureString"
  value = random_password.webhook_secret.result
  tags  = local.common_tags
}
