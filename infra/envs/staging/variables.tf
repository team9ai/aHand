variable "vpc_id" {
  type    = string
  default = "vpc-05804f4c4dd8965f3"
}

variable "subnet_ids" {
  type    = list(string)
  default = ["subnet-0eaffec23bfd7eb63", "subnet-0cdb64bc3cf4c6ee3"]
}

variable "traefik_alb_name" {
  type    = string
  default = "traefik-dev-nlb"
}

variable "traefik_security_group_id" {
  type    = string
  default = "sg-0368318519318a4ba"
}

variable "openclaw_rds_host" {
  type    = string
  default = "openclaw-hive-dev.c89gkagwy37d.us-east-1.rds.amazonaws.com"
}

variable "openclaw_rds_security_group_id" {
  type    = string
  default = "sg-0b7b9a007a8b5b7a6"
}
