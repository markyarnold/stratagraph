# The `vpc` unit — the dependency target of `app`. No dependencies of its own.
terraform {
  source = "git::git@github.com:acme/modules.git//vpc?ref=v1.0.0"
}
