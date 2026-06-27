# A Terragrunt unit (the `app` deployment). It sources a module and declares a
# structural dependency on the sibling `vpc` unit. Functions/locals/outputs are
# NOT evaluated — only the literal `source` and `dependency.config_path`.
terraform {
  source = "git::git@github.com:acme/modules.git//app?ref=v1.0.0"
}

dependency "vpc" {
  config_path = "../vpc"
  mock_outputs = {
    vpc_id = "vpc-fake"
  }
}

inputs = {
  vpc_id = dependency.vpc.outputs.vpc_id
}
