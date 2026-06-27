# Distilled Terraform AppSync fixture (Track D1, Slice 14). NOT copied from client
# code. It mirrors the CFN `infra_appsync` fixture's shape so the SAME infra plane
# (`build_infra_plane`) wires it: a Lambda assuming a role, an AppSync data source
# routing to that Lambda, resolvers producing root GraphQL fields, an unknown
# provider carried as Generic inventory, and an honest var-only ref that invents
# nothing.

variable "env" {
  type    = string
  default = "dev"
}

variable "worker_role_arn" {
  type = string
}

locals {
  name_prefix = "${var.env}-user"
}

# The Lambda assuming a same-file role (Extracted Assumes), backing the data
# source (Extracted Routes). Its `handler` is captured as inventory; the source
# dir hides behind the zip artifact, so the `Runs` bridge stays honestly
# unresolved (the TF analogue of the C# `::`-handler deferral).
resource "aws_lambda_function" "user" {
  function_name = "${local.name_prefix}-fn"
  role          = aws_iam_role.user_exec.arn
  handler       = "user.handler"
  runtime       = "nodejs18.x"
  filename      = "build/user.zip"
}

resource "aws_iam_role" "user_exec" {
  name = "${local.name_prefix}-exec"
}

resource "aws_appsync_graphql_api" "main" {
  name                = "${local.name_prefix}-api"
  authentication_type = "API_KEY"
}

resource "aws_appsync_datasource" "user_ds" {
  api_id           = aws_appsync_graphql_api.main.id
  name             = "user_ds"
  type             = "AWS_LAMBDA"
  service_role_arn = aws_iam_role.user_exec.arn
  lambda_config {
    function_arn = aws_lambda_function.user.arn
  }
}

# Two resolvers whose chains resolve crisply to the Lambda → the money link
# (PRODUCES) sources from the Lambda at Extracted 0.95.
resource "aws_appsync_resolver" "get_user" {
  api_id      = aws_appsync_graphql_api.main.id
  type        = "Query"
  field       = "getUser"
  data_source = aws_appsync_datasource.user_ds.name
}

resource "aws_appsync_resolver" "create_user" {
  api_id      = aws_appsync_graphql_api.main.id
  type        = "Mutation"
  field       = "createUser"
  data_source = aws_appsync_datasource.user_ds.name
}

# A resolver for a field the schema does NOT declare → it must stay unlinked
# (honesty: no PRODUCES edge invented for a ghost field).
resource "aws_appsync_resolver" "ghost" {
  api_id      = aws_appsync_graphql_api.main.id
  type        = "Query"
  field       = "ghostField"
  data_source = aws_appsync_datasource.user_ds.name
}

# A second Lambda whose role is only reachable through a `var.` → the honest
# Unresolved case: NO Assumes edge is invented.
resource "aws_lambda_function" "worker" {
  function_name = "worker"
  role          = var.worker_role_arn
  handler       = "worker.handler"
  runtime       = "nodejs18.x"
  filename      = "build/worker.zip"
}

# An unknown provider — carried as a Generic CloudResource node, never dropped.
resource "google_storage_bucket" "assets" {
  name     = "${local.name_prefix}-assets"
  location = "US"
}
