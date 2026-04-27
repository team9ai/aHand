variable "vpc_id" {
  type    = string
  default = "vpc-028cd35f94f14d52b"
}

variable "subnet_ids" {
  type    = list(string)
  default = ["subnet-09eb68ab6cae3c581", "subnet-0c8ca567a8d4def31"]
}

variable "traefik_alb_name" {
  type    = string
  default = "traefik-nlb"
}

variable "traefik_security_group_id" {
  type    = string
  default = "sg-0ffd97a77fdcbae8f"
}

variable "openclaw_rds_host" {
  type    = string
  default = "openclaw-hive-prod.chq8i2se49qd.us-east-1.rds.amazonaws.com"
}

variable "openclaw_rds_security_group_id" {
  type    = string
  default = "sg-0263c046dbc5b2be6"
}
