//! End-to-end indexer-routing tests for the Terraform plane (Track D1, Slice 14,
//! M2). Builds a repo on disk and runs the full `index_repo` IO shell to prove:
//!
//! - `.tf`/`.tofu` files are routed to the `TerraformAdapter` and their resources
//!   land in the graph (round-tripped through the store).
//! - `.terraform/` (downloaded modules) is PRUNED — a vendored resource never
//!   appears, even with no `.gitignore` (the `TF_SKIP_DIRS` belt-and-suspenders).
//! - `terragrunt.hcl` units become `CloudResource` nodes and their `dependency`
//!   config-paths become structural `Routes` edges between units.
//! - A `terraform show -json` plan supersedes the HCL parse by address (resolved
//!   over raw) and contributes module-expanded resources HCL cannot enumerate.

use std::fs;
use std::path::Path;

use strata_core::{Direction, EdgeKind, Graph, NodeKind, Uid};
use strata_index::{index_repo, IndexStats};
use strata_store::{DuckGraphStore, GraphStore};

/// Index a freshly-built repo at `root` and return its graph + stats.
fn index(root: &Path) -> (Graph, IndexStats) {
    let mut store = DuckGraphStore::open_in_memory().unwrap();
    let stats = index_repo(root, &mut store).unwrap();
    let g = store.load_graph().unwrap();
    (g, stats)
}

fn repo_name(root: &Path) -> String {
    root.file_name().unwrap().to_str().unwrap().to_string()
}

// ── Test 1: .tf routing + .terraform/ pruning ───────────────────────────────

#[test]
fn indexes_tf_resources_and_prunes_dot_terraform() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("app")).unwrap();
    // A vendored module under `.terraform/` — must be pruned (NO `.gitignore` here,
    // so only `TF_SKIP_DIRS` can do it).
    fs::create_dir_all(root.join(".terraform/modules/vendored")).unwrap();

    fs::write(
        root.join("app/main.tf"),
        concat!(
            "resource \"aws_iam_role\" \"exec\" {\n",
            "  name = \"app-exec\"\n",
            "}\n",
            "resource \"aws_lambda_function\" \"api\" {\n",
            "  function_name = \"app-api\"\n",
            "  role          = aws_iam_role.exec.arn\n",
            "  handler       = \"index.handler\"\n",
            "  filename      = \"build/api.zip\"\n",
            "}\n",
        ),
    )
    .unwrap();
    fs::write(
        root.join(".terraform/modules/vendored/main.tf"),
        "resource \"aws_iam_role\" \"vendored\" {\n  name = \"vendored\"\n}\n",
    )
    .unwrap();

    let (g, stats) = index(root);
    let name = repo_name(root);

    // The two `.tf` resources are nodes (path is the repo-relative `.tf` file).
    let lambda = Uid::new("infra", &name, "app/main.tf", "aws_lambda_function.api", "");
    let role = Uid::new("infra", &name, "app/main.tf", "aws_iam_role.exec", "");
    assert_eq!(
        g.get_node(&lambda).map(|n| n.kind),
        Some(NodeKind::LambdaFn),
        "the .tf Lambda is indexed"
    );
    assert_eq!(g.get_node(&role).map(|n| n.kind), Some(NodeKind::IamRole));

    // Assumes edge survives the round-trip.
    let assumes: Vec<Uid> = g
        .neighbors(&lambda, Direction::Outgoing, &[EdgeKind::Assumes])
        .into_iter()
        .map(|(e, _)| e.dst.clone())
        .collect();
    assert!(
        assumes.contains(&role),
        "Lambda —Assumes→ role: {assumes:?}"
    );

    // The `.terraform/` vendored resource is PRUNED — NO node for it anywhere.
    let vendored_present = g.nodes().any(|n| n.name == "aws_iam_role.vendored");
    assert!(
        !vendored_present,
        "a resource under .terraform/ must be pruned, never indexed"
    );

    // Coverage counts the TF template + its resources.
    assert!(stats.infra_link.templates_detected >= 1);
    assert!(stats.infra_link.resources_total >= 2);
}

// ── Test 2: terragrunt.hcl units + structural dependency edges ───────────────

#[test]
fn indexes_terragrunt_units_and_dependency_edges() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("app")).unwrap();
    fs::create_dir_all(root.join("vpc")).unwrap();

    fs::write(
        root.join("app/terragrunt.hcl"),
        concat!(
            "terraform {\n",
            "  source = \"git::git@github.com:acme/mods.git//lambda?ref=v1\"\n",
            "}\n",
            "dependency \"vpc\" {\n",
            "  config_path = \"../vpc\"\n",
            "}\n",
        ),
    )
    .unwrap();
    fs::write(
        root.join("vpc/terragrunt.hcl"),
        "terraform {\n  source = \"git::git@github.com:acme/mods.git//vpc?ref=v1\"\n}\n",
    )
    .unwrap();

    let (g, stats) = index(root);
    let name = repo_name(root);

    // Each unit is a CloudResource node keyed by its directory.
    let app_unit = Uid::new("infra", &name, "app/terragrunt.hcl", "app", "");
    let vpc_unit = Uid::new("infra", &name, "vpc/terragrunt.hcl", "vpc", "");
    assert_eq!(
        g.get_node(&app_unit).map(|n| n.kind),
        Some(NodeKind::CloudResource),
        "the app unit is a node"
    );
    assert_eq!(
        g.get_node(&vpc_unit).map(|n| n.kind),
        Some(NodeKind::CloudResource)
    );

    // app —Routes→ vpc (the structural dependency, Extracted).
    let routes: Vec<Uid> = g
        .neighbors(&app_unit, Direction::Outgoing, &[EdgeKind::Routes])
        .into_iter()
        .map(|(e, _)| e.dst.clone())
        .collect();
    assert!(
        routes.contains(&vpc_unit),
        "app unit —Routes→ vpc unit (structural dependency): {routes:?}"
    );

    assert_eq!(stats.terragrunt.units_detected, 2, "{:?}", stats.terragrunt);
    assert_eq!(stats.terragrunt.deps_linked, 1, "{:?}", stats.terragrunt);
    assert_eq!(
        stats.terragrunt.deps_unresolved, 0,
        "{:?}",
        stats.terragrunt
    );
}

// ── Test 3: plan-JSON supersedes the HCL parse (resolved over raw) ───────────

#[test]
fn plan_json_supersedes_hcl_and_adds_module_expanded() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // The raw HCL: one Lambda (no role wiring resolvable here — role via var).
    fs::write(
        root.join("main.tf"),
        concat!(
            "resource \"aws_lambda_function\" \"api\" {\n",
            "  function_name = \"api\"\n",
            "  role          = var.role_arn\n",
            "  handler       = \"index.handler\"\n",
            "}\n",
        ),
    )
    .unwrap();

    // A committed plan JSON whose resolved view wires the role AND expands a module
    // resource the raw HCL cannot enumerate.
    fs::write(
        root.join("plan.tfplan.json"),
        r#"{
  "format_version": "1.0",
  "terraform_version": "1.5.7",
  "planned_values": {
    "root_module": {
      "resources": [
        { "address": "aws_iam_role.exec", "type": "aws_iam_role", "name": "exec", "values": {} },
        { "address": "aws_lambda_function.api", "type": "aws_lambda_function", "name": "api",
          "values": { "handler": "index.handler" } }
      ],
      "child_modules": [
        { "address": "module.q",
          "resources": [
            { "address": "module.q.aws_sqs_queue.main", "type": "aws_sqs_queue", "name": "main", "values": {} }
          ]
        }
      ]
    }
  },
  "configuration": {
    "root_module": {
      "resources": [
        { "address": "aws_lambda_function.api", "type": "aws_lambda_function",
          "expressions": { "role": { "references": ["aws_iam_role.exec.arn", "aws_iam_role.exec"] } } }
      ]
    }
  }
}"#,
    )
    .unwrap();

    let (g, _stats) = index(root);
    let name = repo_name(root);

    // The plan's resolved chain wires the Lambda → role (the raw HCL's var-only ref
    // could not). The Lambda node comes from the plan (path = plan.tfplan.json).
    let lambda = Uid::new(
        "infra",
        &name,
        "plan.tfplan.json",
        "aws_lambda_function.api",
        "",
    );
    let role = Uid::new("infra", &name, "plan.tfplan.json", "aws_iam_role.exec", "");
    let assumes: Vec<Uid> = g
        .neighbors(&lambda, Direction::Outgoing, &[EdgeKind::Assumes])
        .into_iter()
        .map(|(e, _)| e.dst.clone())
        .collect();
    assert!(
        assumes.contains(&role),
        "the plan resolves the Lambda→role Assumes the raw HCL could not: {assumes:?}"
    );

    // The module-expanded resource (only the plan can enumerate it) is present.
    let queue = Uid::new(
        "infra",
        &name,
        "plan.tfplan.json",
        "module.q.aws_sqs_queue.main",
        "",
    );
    assert!(
        g.get_node(&queue).is_some(),
        "the module-expanded resource from the plan is indexed"
    );

    // Dedup by address: the HCL Lambda node (path main.tf) was superseded — there is
    // NOT also a `main.tf`-pathed Lambda node for the same address.
    let hcl_lambda = Uid::new("infra", &name, "main.tf", "aws_lambda_function.api", "");
    assert!(
        g.get_node(&hcl_lambda).is_none(),
        "the HCL Lambda is superseded by the plan's (deduped by address)"
    );
}
