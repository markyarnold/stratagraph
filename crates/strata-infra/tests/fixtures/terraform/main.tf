# Distilled Terraform fixture (Track D1, Slice 14). NOT copied from client code —
# a hand-built shape exercising every M1 extraction/grading path once:
#   - resource/data/module blocks
#   - same-file resource references (Extracted)
#   - var./local. interpolation + plain refs (Inferred / Unresolved, never invented)
#   - a cross-module ref (module.x.out — Unresolved)
#   - an unknown provider (google_*) → Generic, never dropped
#   - the AppSync resolver→datasource→lambda money chain

variable "env" {
  type    = string
  default = "dev"
}

locals {
  name_prefix = "${var.env}-app"
}

data "aws_caller_identity" "current" {}

resource "aws_iam_role" "lambda_exec" {
  name = "${local.name_prefix}-exec"
}

resource "aws_lambda_function" "api" {
  function_name = "${local.name_prefix}-api"
  role          = aws_iam_role.lambda_exec.arn
  handler       = "index.handler"
  runtime       = "nodejs18.x"
  filename      = "build/api.zip"
}

# A second Lambda whose role is only reachable through a `var.` — the honest
# Unresolved case (no same-file resource is named, so NO Assumes edge).
resource "aws_lambda_function" "worker" {
  function_name = "worker"
  role          = var.worker_role_arn
  handler       = "worker.handler"
  runtime       = "nodejs18.x"
}

resource "aws_appsync_graphql_api" "main" {
  name                = "${local.name_prefix}-api"
  authentication_type = "API_KEY"
}

resource "aws_appsync_datasource" "lambda_ds" {
  api_id           = aws_appsync_graphql_api.main.id
  name             = "lambda_ds"
  type             = "AWS_LAMBDA"
  service_role_arn = aws_iam_role.lambda_exec.arn
  lambda_config {
    function_arn = aws_lambda_function.api.arn
  }
}

resource "aws_appsync_resolver" "get_user" {
  api_id      = aws_appsync_graphql_api.main.id
  type        = "Query"
  field       = "getUser"
  data_source = aws_appsync_datasource.lambda_ds.name
}

# An unknown provider — carried as a Generic inventory node, never dropped.
resource "google_storage_bucket" "assets" {
  name     = "my-assets"
  location = "US"
}

module "vpc" {
  source = "terraform-aws-modules/vpc/aws"
  name   = local.name_prefix
}

# A Lambda wired to a module output (`module.vpc.queue_arn`) — cross-module, so
# the ref is Unresolved (we never evaluate module outputs); but its role is a
# crisp same-file ref (Extracted).
resource "aws_lambda_event_source_mapping" "from_module" {
  event_source_arn = module.vpc.queue_arn
  function_name    = aws_lambda_function.api.arn
}
