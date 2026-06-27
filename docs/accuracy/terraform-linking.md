# Terraform / Terragrunt Link Coverage

Measured coverage of Strata's **Terraform plane** (Track D1): how much of a
detected Terraform/OpenTofu estate the infra plane connects, and at what
honest-provenance tier. A `.tf`/`.tofu` config flows through the **same**
`build_infra_plane` path as CloudFormation/SAM (the `TerraformAdapter` produces the
same `InfraResource`s / `RefValue` grades the CFN adapter does), so the typed nodes
(`LambdaFn`/`IamRole`/`AppSyncApi`/…) and graded edges
(`Assumes`/`Routes`/`Contains`/`PRODUCES`) are identical in kind to the CFN report
(`docs/accuracy/infra-linking.md`). This is the Terraform companion to that report.

The numbers are produced by indexing a committed, hermetic fixture repo (sources
only; no `node_modules`, no Node, no `terraform`/network at test time) and reading
its per-repo `InfraLinkCoverage` + `TerragruntCoverage` (surfaced in `IndexStats`,
printed by `strata index`). They are kept honest two ways, the same discipline as
the OpenAPI/GraphQL/CFN reports:

- **`tests/terraform_coverage.rs::terraform_report_matches_committed_numbers`**
  asserts the live coverage equals the numbers tabulated below (so this report
  cannot silently drift from the code).
- **`tests/terraform_coverage.rs::terraform_coverage_meets_documented_floors`** is
  the CI gate: it fails the build if any documented floor regresses.

Regenerate the raw figures with:

```
cargo test -p strata-index --test terraform_coverage print_terraform_coverage -- --ignored --nocapture
```

## De-risk verdict (the slice hinged on it)

The `hcl` crate (`hcl-rs`, pinned **`=0.19.7`**) was evaluated against distilled
Terragrunt **and** `.tf` shapes **before** any building. Verdict: it parses **both**
into a walkable structural AST (`hcl::structure::{Body, Block, Structure}`) without
choking on HCL2 function syntax. Specifically:

| construct | parses? | how we read it |
|---|---|---|
| `resource`/`data`/`module`/`variable`/`locals` blocks | ✅ | `Block` with `identifier()` + `labels()` |
| nested blocks (`lambda_config { … }`) | ✅ | a child `Block` in the parent's body |
| a resource reference `aws_iam_role.x.arn` | ✅ | `Expression::Traversal` (head `Variable` + first `GetAttr` ⇒ `type.name`) |
| `${…}` interpolation | ✅ | `Expression::TemplateExpr`, walked via `hcl::Template::from_expr` |
| Terragrunt functions `find_in_parent_folders()` / `read_terragrunt_config(…)` | ✅ (as nodes) | `Expression::FuncCall`, **survives as an expression, never a parse error** |
| `for`/conditional/`try()` expressions | ✅ (as nodes) | `Expression::ForExpr` / `Conditional` / `FuncCall` |

We do **not evaluate** any of these (scope §7): variables, locals, functions,
module outputs, `count`/`for_each`, and Terragrunt `dependency.x.outputs.*` are
left as unevaluated expressions and surfaced honestly (Inferred/Unresolved), never
invented. So Terragrunt's *structure* (the `source` and `dependency.config_path`
literals) is extracted; its *evaluation* is explicitly out of scope.

## TF type → InfraKind mapping

| Terraform type | InfraKind / NodeKind |
|---|---|
| `aws_lambda_function` | LambdaFunction / `LambdaFn` |
| `aws_iam_role` | IamRole / `IamRole` |
| `aws_appsync_graphql_api` | AppSyncApi / `AppSyncApi` |
| `aws_appsync_resolver` | AppSyncResolver / `AppSyncResolver` |
| `aws_appsync_datasource` | AppSyncDataSource / `AppSyncDataSource` |
| every other `aws_*` | Generic / `CloudResource` (inventory) |
| an **unknown provider** (`google_*`, `azurerm_*`, …) | Generic / `CloudResource` (**never dropped**) |

A resource's logical id is its Terraform **address** (`aws_lambda_function.api`),
the identity Terraform itself uses; the `cfn_type`-analogue carries the TF type
string.

## Reference grading (honest provenance, R1)

Every reference is graded by `RefValue`, exactly like the CFN adapter:

| reference shape | grade | tier |
|---|---|---:|
| a same-file resource address (`role = aws_iam_role.x.arn`) | `Resource("aws_iam_role.x")` | Extracted **0.95** |
| an interpolation embedding a same-file resource (`"…/${aws_iam_role.x.name}"`) | `Inferred("aws_iam_role.x")` | Inferred **0.70** |
| a `var.`/`local.`/`module.x.out`/cross-file/data-output ref | `Unresolved` | n/a (no edge) |
| a function-buried ref (`try(aws_iam_role.x.arn, …)`) | `Unresolved` | n/a (no edge; never promoted) |
| a plain string | `Literal` | n/a |

A `Resource(id)` is **never** invented for a target absent from the parsed file;
the §4.1 band invariant extends, non-vacuously, over the Terraform edges
(`tests/confidence_bands.rs::terraform_infra_edges_satisfy_band_invariant`).

### Plan JSON: the "resolved over raw" preferred source

When a `terraform show -json` plan/state document is committed, its resources are
the **resolved** inventory, including module-/`count`-expanded instances raw HCL
cannot statically enumerate. The indexer ingests it (`is_plan_json` detects the
`terraform_version` + `planned_values` shape), reading the resource inventory from
`planned_values` and the inter-resource references from the `configuration` block's
`expressions.<attr>.references`. A plan resource **supersedes** an HCL one with the
same address (dedup by address). A configuration reference naming a plan resource is
a recorded dependency → Extracted; references to inputs (`var.`/`data`) yield no
edge.

IAM grants (Track D2, C3) are read from the plan's **resolved** policy documents
too, so a committed plan does not lose the grants the HCL parse would have found
(the superseding plan resource carries them). An `aws_iam_role`'s
`inline_policy[].policy` and a standalone `aws_iam_role_policy` / `aws_iam_policy`'s
resolved `policy` string are parsed to concrete actions with the SAME
never-confident-wrong grading as the HCL path: a `policy` unknown at plan time
(known-after-apply), a `managed_policy_arns` entry, or an
`aws_iam_role_policy_attachment` becomes an `<opaque:…>` indeterminacy marker, so
the targeted role is INDETERMINATE rather than falsely grant-less.

## What the `Runs` bridge does NOT do for Terraform (honest deferral)

A `.tf` Lambda packages a build artifact (`filename = "…/x.zip"`, an
`archive_file`, or an `s3_key`); the **source directory** is generally not present
in the static config (it hides behind the build step). So the `Runs` bridge
(Lambda → its handler's code `Module`) typically resolves to **no module** and the
Lambda is counted `lambdas_handler_unresolved`, a surfaced miss, **never an
invented edge**. This is the Terraform analogue of the C# `::`-handler deferral
(`docs/accuracy/infra-linking.md`): the typed Lambda node and its `Assumes`/money
links are all present; only the source-file bridge is deferred. (The `handler`
literal is still captured as inventory.)

## Terragrunt structural coverage (NOT evaluation)

A `terragrunt.hcl` becomes a `CloudResource` node (one per unit, keyed by its
directory). Each `dependency "x" { config_path = "../vpc" }` whose **literal**
`config_path` resolves to another detected unit emits a `CloudResource —Routes→
CloudResource` edge at Extracted 0.95 (the config_path is a structural fact). The
`terraform { source = … }` literal is recorded on the unit.

**Honest bound:** Terragrunt `dependency.<name>.outputs.*`, `locals`,
`read_terragrunt_config`, `find_in_parent_folders`, and generate/remote-state
blocks are **not evaluated**. So cross-unit *attribute* wiring (a unit's resource
consuming another unit's output) is **Unresolved**, surfaced by the absence of
those edges, never invented. A `source`/`config_path` that is an interpolation or
function call (not a string literal) is recorded as `None`/skipped, never guessed.

## Corpus

One committed fixture repo under
`crates/strata-index/tests/fixtures/terraform_appsync/`:

- **`main.tf`**: an AppSync chain: `aws_lambda_function.user` assuming
  `aws_iam_role.user_exec`, an `aws_appsync_datasource.user_ds` backing it, and
  `get_user`/`create_user` resolvers whose chains resolve crisply to the Lambda
  (the money link). Plus a `ghost` resolver for an undeclared field (stays
  unlinked), a `worker` Lambda whose role is a `var.` (no invented `Assumes`), and
  a `google_storage_bucket.assets` (Generic, never dropped).
- **`schema.graphql`**: declares `Query.getUser` + `Mutation.createUser` (the
  fields the resolvers PRODUCES); `ghostField` is deliberately absent.
- **`infra/app/terragrunt.hcl`** + **`infra/vpc/terragrunt.hcl`**: two Terragrunt
  units; `app` declares a structural `dependency` on `vpc`.

Indexed hermetically with `index_repo` (no Node/SCIP/network).

## Results

Measured 2026-06-13 over the committed `terraform_appsync` fixture.

### Terraform `.tf` (through the infra plane)

| metric | value |
|---|---:|
| `templates_detected` (TF configs) | **1** |
| `resources_total` | **9** |
| `resolvers_total` | **3** |
| &nbsp;&nbsp;of which `resolvers_linked` | 2 |
| &nbsp;&nbsp;of which `resolvers_unlinked` | 1 |
| `lambdas_runs_linked` | **0** |
| `lambdas_handler_unresolved` | **2** |

Reading the numbers:

- **1 config, 9 resources:** the Lambda, role, AppSync API, data source, three
  resolvers (incl. the ghost), the `worker` Lambda, and the `google_*` bucket:
  every one a typed node (the unknown provider carried as a Generic
  `CloudResource`, never dropped). The API `Contains` its data source/resolvers via
  their `api_id`.
- **2 of 3 resolvers linked:** `get_user` → `Query.getUser` and `create_user` →
  `Mutation.createUser`, each at Extracted 0.95 sourced from the `user` Lambda
  (their resolver→datasource→lambda chains are wholly `Resource`-graded). The third
  (`ghost`, for the undeclared `ghostField`) is honestly **unlinked**, no edge
  invented.
- **0 Runs links, 2 handler-unresolved:** both Lambdas package a `.zip`
  (`filename`), so the source directory is not in the config and the `Runs` bridge
  resolves to nothing, surfaced, never invented (the deferral documented above).

### Terragrunt (structural)

| metric | value |
|---|---:|
| `units_detected` | **2** |
| `deps_total` | **1** |
| &nbsp;&nbsp;of which `deps_linked` | 1 |
| &nbsp;&nbsp;of which `deps_unresolved` | 0 |

- **2 units, 1 dependency linked:** the `app` unit `Routes` to the `vpc` unit (its
  literal `config_path = "../vpc"` resolves to the sibling unit) at Extracted 0.95.
  The cross-unit *output* wiring is not evaluated (Unresolved by design).

## Honesty / scope caveat

**The corpus is a single hand-built fixture repo.** These numbers are a *starting*
coverage measurement that exercises every Terraform linking path once, not a
statistically authoritative claim about real-world recall. The durable
deliverables are the `TerraformAdapter`, the plan-JSON ingestion, the Terragrunt
structural extractor, the `RefValue`-graded wiring through the one
`build_infra_plane`, the band invariant over the TF edges, the indexer routing
(`.tf`/`.tofu` → adapter, `.terraform/` pruned), the CI gate, and this report, all
of which sharpen automatically as the corpus grows.

## CI floors

`terraform_coverage_meets_documented_floors` gates: `templates_detected ≥ 1`,
`resources_total ≥ 6`, `resolvers_linked ≥ 2`, `units_detected ≥ 2`, and the
two-sided honesty pins `resolvers_unlinked == 1` / `lambdas_runs_linked == 0` /
`deps_unresolved == 0`: a regression that silently invented a link (or dropped a
real one) fails the build. Floors sit at the measured values; they are re-derived
from this report whenever the fixture changes.
