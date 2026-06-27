//! Terraform `.tf` adapter tests (Track D1, Slice 14, M1).
//!
//! Fixture-driven, mirroring the CFN/SAM adapter tests. The honesty discipline is
//! the same as CFN: a same-file resource reference grades [`RefValue::Resource`]
//! (Extracted), an interpolation that recovers a same-file resource grades
//! [`RefValue::Inferred`], and a `var.`/`local.`/`module.`/`data.` reference that
//! names no same-file resource grades [`RefValue::Unresolved`] — a `Resource(id)`
//! is NEVER invented for a target absent from the parsed file.

use strata_infra::{IacAdapter, InfraError, InfraKind, InfraResource, RefValue, TerraformAdapter};

fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("terraform")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read fixture {name}: {e}"))
}

/// Look up the single resource with `logical_id` (panics if absent), so per-field
/// assertions are independent of vector ordering.
fn res_by_id<'a>(resources: &'a [InfraResource], id: &str) -> &'a InfraResource {
    resources
        .iter()
        .find(|r| r.logical_id == id)
        .unwrap_or_else(|| panic!("no resource {id:?}; got ids {:?}", ids(resources)))
}

fn ids(resources: &[InfraResource]) -> Vec<&str> {
    resources.iter().map(|r| r.logical_id.as_str()).collect()
}

// ── Test 1: detects ─────────────────────────────────────────────────────────

#[test]
fn detects_true_for_tf_false_for_non_tf() {
    let a = TerraformAdapter;

    // True for a real `.tf` with resource/data/module blocks.
    assert!(a.detects("main.tf", &fixture("main.tf")));

    // False for HCL that carries no resource/data/module block (a `.tfvars`-shaped
    // file is just attributes) — it is valid HCL but not a Terraform config we
    // extract resources from.
    let tfvars = "region = \"us-east-1\"\ninstance_count = 3\n";
    assert!(
        !a.detects("terraform.tfvars", tfvars),
        "a tfvars-shaped HCL file (no resource/data/module) must not detect"
    );

    // False for non-HCL entirely (a JSON spec) — the cheap textual signal is
    // absent and a parse would not yield Terraform blocks anyway.
    assert!(
        !a.detects("package.json", "{\"name\": \"x\", \"version\": \"1.0.0\"}"),
        "arbitrary JSON must not detect as Terraform"
    );
}

// ── Test 2: type → InfraKind mapping (including unknown providers) ────────────

#[test]
fn maps_tf_types_to_infra_kinds() {
    let t = TerraformAdapter
        .extract("main.tf", &fixture("main.tf"))
        .expect("main.tf parses");

    // The vertical-specific kinds.
    assert_eq!(
        res_by_id(&t.resources, "aws_lambda_function.api").kind,
        InfraKind::LambdaFunction
    );
    assert_eq!(
        res_by_id(&t.resources, "aws_iam_role.lambda_exec").kind,
        InfraKind::IamRole
    );
    assert_eq!(
        res_by_id(&t.resources, "aws_appsync_graphql_api.main").kind,
        InfraKind::AppSyncApi
    );
    assert_eq!(
        res_by_id(&t.resources, "aws_appsync_datasource.lambda_ds").kind,
        InfraKind::AppSyncDataSource
    );
    assert_eq!(
        res_by_id(&t.resources, "aws_appsync_resolver.get_user").kind,
        InfraKind::AppSyncResolver
    );

    // An unknown provider (`google_*`) → Generic, NEVER dropped.
    let bucket = res_by_id(&t.resources, "google_storage_bucket.assets");
    assert_eq!(bucket.kind, InfraKind::Generic);
    assert_eq!(bucket.cfn_type, "google_storage_bucket");

    // Any other `aws_*` resource is also Generic inventory (carried, not dropped).
    let esm = res_by_id(&t.resources, "aws_lambda_event_source_mapping.from_module");
    assert_eq!(esm.kind, InfraKind::Generic);

    // The logical id is the `type.name` address; the cfn_type-analogue is the TF
    // type string.
    let lambda = res_by_id(&t.resources, "aws_lambda_function.api");
    assert_eq!(lambda.cfn_type, "aws_lambda_function");
    assert_eq!(lambda.logical_id, "aws_lambda_function.api");
}

// ── Test 3: same-file resource refs grade Extracted (Resource) ───────────────

#[test]
fn same_file_resource_ref_is_resource_extracted() {
    let t = TerraformAdapter
        .extract("main.tf", &fixture("main.tf"))
        .expect("parses");

    // `role = aws_iam_role.lambda_exec.arn` → Resource("aws_iam_role.lambda_exec").
    let lambda = res_by_id(&t.resources, "aws_lambda_function.api");
    assert_eq!(
        lambda.role_refs,
        vec![RefValue::Resource("aws_iam_role.lambda_exec".to_string())],
        "a same-file resource ref in `role` grades Resource (Extracted)"
    );

    // The data source's `service_role_arn` likewise.
    let ds = res_by_id(&t.resources, "aws_appsync_datasource.lambda_ds");
    assert_eq!(
        ds.role_refs,
        vec![RefValue::Resource("aws_iam_role.lambda_exec".to_string())]
    );
    // `LambdaConfig.function_arn` → the data source's lambda_ref Resource.
    assert_eq!(
        ds.lambda_ref,
        Some(RefValue::Resource("aws_lambda_function.api".to_string())),
        "nested lambda_config.function_arn resolves to the same-file Lambda"
    );
    // `api_id = aws_appsync_graphql_api.main.id` → api_ref Resource.
    assert_eq!(
        ds.api_ref,
        Some(RefValue::Resource(
            "aws_appsync_graphql_api.main".to_string()
        ))
    );

    // The resolver's data_source + api refs.
    let resolver = res_by_id(&t.resources, "aws_appsync_resolver.get_user");
    assert_eq!(
        resolver.data_source_ref,
        Some(RefValue::Resource(
            "aws_appsync_datasource.lambda_ds".to_string()
        ))
    );
    assert_eq!(resolver.type_name.as_deref(), Some("Query"));
    assert_eq!(resolver.field_name.as_deref(), Some("getUser"));
}

// ── Test 4: var./local./module. refs grade Unresolved (never invented) ───────

#[test]
fn var_local_module_refs_are_unresolved_never_invented() {
    let t = TerraformAdapter
        .extract("main.tf", &fixture("main.tf"))
        .expect("parses");

    // `role = var.worker_role_arn` names NO same-file resource → Unresolved.
    let worker = res_by_id(&t.resources, "aws_lambda_function.worker");
    assert_eq!(
        worker.role_refs,
        vec![RefValue::Unresolved],
        "a `var.` role ref names no same-file resource → Unresolved, never invented"
    );

    // `event_source_arn = module.vpc.queue_arn` is cross-module → we never
    // evaluate module outputs, so there is no resource ref to grade (the field is
    // not a role/known ref). The role-bearing fields stay honest; confirm no
    // role_ref invented a `module.vpc` Resource.
    let esm = res_by_id(&t.resources, "aws_lambda_event_source_mapping.from_module");
    assert!(
        !esm.role_refs
            .iter()
            .any(|r| matches!(r, RefValue::Resource(id) if id.starts_with("module."))),
        "a module.* ref must never be graded Resource: {:?}",
        esm.role_refs
    );
}

// ── Test 5: interpolation that recovers a same-file resource → Inferred ───────

#[test]
fn interpolation_recovering_same_file_resource_is_inferred() {
    // A Lambda whose role is an interpolated ARN that *embeds* a same-file
    // resource address: `"arn:...:${aws_iam_role.r.name}"` → Inferred (a
    // best-effort id recovered from interpolation, not a crisp Resource).
    let src = concat!(
        "resource \"aws_iam_role\" \"r\" {\n",
        "  name = \"r\"\n",
        "}\n",
        "resource \"aws_lambda_function\" \"f\" {\n",
        "  function_name = \"f\"\n",
        "  role          = \"arn:aws:iam::123:role/${aws_iam_role.r.name}\"\n",
        "  handler       = \"i.h\"\n",
        "}\n",
    );
    let t = TerraformAdapter.extract("i.tf", src).expect("parses");
    let f = res_by_id(&t.resources, "aws_lambda_function.f");
    assert_eq!(
        f.role_refs,
        vec![RefValue::Inferred("aws_iam_role.r".to_string())],
        "an interpolation embedding a same-file resource grades Inferred"
    );

    // An interpolation embedding only a `var.`/`local.` (no same-file resource) →
    // Unresolved.
    let src2 = concat!(
        "resource \"aws_lambda_function\" \"g\" {\n",
        "  function_name = \"g\"\n",
        "  role          = \"arn:aws:iam::123:role/${var.role_name}\"\n",
        "  handler       = \"i.h\"\n",
        "}\n",
    );
    let t2 = TerraformAdapter.extract("i2.tf", src2).expect("parses");
    let g = res_by_id(&t2.resources, "aws_lambda_function.g");
    assert_eq!(
        g.role_refs,
        vec![RefValue::Unresolved],
        "an interpolation with only var. recovers no resource → Unresolved"
    );
}

// ── Test 6: literal handler / code path ──────────────────────────────────────

#[test]
fn lambda_literal_fields_are_captured() {
    let t = TerraformAdapter
        .extract("main.tf", &fixture("main.tf"))
        .expect("parses");
    let lambda = res_by_id(&t.resources, "aws_lambda_function.api");
    assert_eq!(lambda.handler.as_deref(), Some("index.handler"));
    // `filename` is the TF analogue of CodeUri (the local artifact path).
    assert_eq!(lambda.code_uri.as_deref(), Some("build/api.zip"));
}

// ── Test 7: determinism — same input → identical extraction ──────────────────

#[test]
fn extraction_is_deterministic() {
    let a = TerraformAdapter
        .extract("main.tf", &fixture("main.tf"))
        .expect("parses");
    let b = TerraformAdapter
        .extract("main.tf", &fixture("main.tf"))
        .expect("parses");
    assert_eq!(
        a, b,
        "the same (path, content) yields identical InfraTemplates"
    );
}

// ── Test 8: malformed HCL degrades visibly (Err, never a panic/partial) ──────

#[test]
fn malformed_hcl_returns_parse_error() {
    // An unterminated block is a parse error — surfaced as InfraError::Parse, never
    // a panic and never a partial extraction.
    let bad = "resource \"aws_lambda_function\" \"x\" {\n  name = \n";
    let err = TerraformAdapter.extract("bad.tf", bad).unwrap_err();
    match err {
        InfraError::Parse { path, .. } => assert_eq!(path, "bad.tf"),
    }
}
