//! CloudFormation / SAM adapter tests (Slice 5, Milestone 1 — DoD tests 1–6).
//!
//! Fixture-driven, mirroring `strata-contract`'s adapter tests. Test 2 (the SAM
//! short-form chain) doubles as the de-risk proof that short-form CFN tags are
//! parsed correctly via the textual normalization pass.

use strata_infra::{CfnSamAdapter, IacAdapter, InfraError, InfraKind, InfraResource, RefValue};

fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read fixture {name}: {e}"))
}

/// Look up the single resource with `logical_id` (panics if absent), so per-field
/// assertions are independent of vector ordering.
fn res_by_id<'a>(resources: &'a [InfraResource], id: &str) -> &'a InfraResource {
    resources
        .iter()
        .find(|r| r.logical_id == id)
        .unwrap_or_else(|| panic!("no resource {id:?}; got {resources:?}"))
}

// ── Test 1: detects ─────────────────────────────────────────────────────────

#[test]
fn detects_true_for_templates_false_for_non_templates() {
    let a = CfnSamAdapter;

    // True for the three real templates (fixtures 1–3).
    assert!(a.detects("template.yaml", &fixture("sam_appsync.yaml")));
    assert!(a.detects("template.json", &fixture("cfn_long_form.json")));
    assert!(a.detects("template.yaml", &fixture("unresolved_refs.yaml")));

    // False for non-templates.
    assert!(
        !a.detects("buildspec.yml", &fixture("not_a_template.yml")),
        "buildspec (phases/commands) must not detect"
    );
    assert!(
        !a.detects("samconfig.toml", &fixture("samconfig.toml")),
        "samconfig TOML won't YAML-parse to a Resources map"
    );
    assert!(
        !a.detects("docker-compose.yml", &fixture("docker_compose.yml")),
        "docker-compose (services:) must not detect"
    );
    assert!(
        !a.detects("package.json", &fixture("arbitrary.json")),
        "arbitrary JSON must not detect"
    );
}

// ── Test 2: SAM chain extraction (fixture 1, short-form tags) ───────────────

#[test]
fn extract_sam_appsync_chain() {
    let t = CfnSamAdapter
        .extract("template.yaml", &fixture("sam_appsync.yaml"))
        .expect("sam template parses");

    // 7 resources: function, role, api, datasource, two resolvers, queue.
    assert_eq!(t.resources.len(), 7, "got {:?}", t.resources);

    // The serverless function: literals read directly (no transform expansion),
    // role graded to the same-template role.
    let func = res_by_id(&t.resources, "PolicyFunction");
    assert_eq!(func.kind, InfraKind::LambdaFunction);
    assert_eq!(func.cfn_type, "AWS::Serverless::Function");
    assert_eq!(func.handler.as_deref(), Some("app.lambda_handler"));
    assert_eq!(
        func.code_uri.as_deref(),
        Some("functions/policy-operations/")
    );
    assert_eq!(
        func.role_refs,
        vec![RefValue::Resource("PolicyRole".into())]
    );

    // The IAM role.
    let role = res_by_id(&t.resources, "PolicyRole");
    assert_eq!(role.kind, InfraKind::IamRole);
    assert_eq!(role.cfn_type, "AWS::IAM::Role");

    // The AppSync API.
    let api = res_by_id(&t.resources, "Api");
    assert_eq!(api.kind, InfraKind::AppSyncApi);

    // The data source: lambda_ref -> the same-template function; api_ref -> Api.
    let ds = res_by_id(&t.resources, "PolicyDS");
    assert_eq!(ds.kind, InfraKind::AppSyncDataSource);
    assert_eq!(
        ds.lambda_ref,
        Some(RefValue::Resource("PolicyFunction".into()))
    );
    assert_eq!(ds.api_ref, Some(RefValue::Resource("Api".into())));

    // Both resolvers: TypeName Query, correct field names, DataSourceName -> DS.
    let r1 = res_by_id(&t.resources, "GetPolicyStatsResolver");
    assert_eq!(r1.kind, InfraKind::AppSyncResolver);
    assert_eq!(r1.type_name.as_deref(), Some("Query"));
    assert_eq!(r1.field_name.as_deref(), Some("getPolicyStats"));
    assert_eq!(
        r1.data_source_ref,
        Some(RefValue::Resource("PolicyDS".into()))
    );

    let r2 = res_by_id(&t.resources, "ListContentPoliciesResolver");
    assert_eq!(r2.field_name.as_deref(), Some("listContentPolicies"));
    assert_eq!(
        r2.data_source_ref,
        Some(RefValue::Resource("PolicyDS".into()))
    );

    // The unrelated queue -> Generic, inventory only.
    let q = res_by_id(&t.resources, "EventQueue");
    assert_eq!(q.kind, InfraKind::Generic);
    assert_eq!(q.cfn_type, "AWS::SQS::Queue");
    assert_eq!(q.handler, None);
    assert_eq!(q.role_refs, Vec::<RefValue>::new());
}

// ── Test 3: JSON long-form yields identical classification/refs (fixture 2) ──

#[test]
fn extract_json_long_form_matches_yaml() {
    let t = CfnSamAdapter
        .extract("template.json", &fixture("cfn_long_form.json"))
        .expect("json template parses");

    assert_eq!(t.resources.len(), 7, "got {:?}", t.resources);

    let func = res_by_id(&t.resources, "PolicyFunction");
    assert_eq!(func.kind, InfraKind::LambdaFunction);
    assert_eq!(func.handler.as_deref(), Some("app.lambda_handler"));
    assert_eq!(
        func.code_uri.as_deref(),
        Some("functions/policy-operations/")
    );
    assert_eq!(
        func.role_refs,
        vec![RefValue::Resource("PolicyRole".into())]
    );

    let ds = res_by_id(&t.resources, "PolicyDS");
    assert_eq!(
        ds.lambda_ref,
        Some(RefValue::Resource("PolicyFunction".into()))
    );
    assert_eq!(ds.api_ref, Some(RefValue::Resource("Api".into())));

    let r1 = res_by_id(&t.resources, "GetPolicyStatsResolver");
    assert_eq!(r1.type_name.as_deref(), Some("Query"));
    assert_eq!(r1.field_name.as_deref(), Some("getPolicyStats"));
    assert_eq!(
        r1.data_source_ref,
        Some(RefValue::Resource("PolicyDS".into()))
    );

    let q = res_by_id(&t.resources, "EventQueue");
    assert_eq!(q.kind, InfraKind::Generic);
}

// ── Test 4: reference grading honesty (fixture 3) ───────────────────────────

#[test]
fn reference_grading_is_honest() {
    let t = CfnSamAdapter
        .extract("template.yaml", &fixture("unresolved_refs.yaml"))
        .expect("edge-case template parses");

    // Role via ImportValue -> Unresolved (cross-stack, not a same-template id).
    let func = res_by_id(&t.resources, "PolicyFunction");
    assert_eq!(func.role_refs, vec![RefValue::Unresolved]);

    // DataSourceName is a Ref to a PARAMETER -> Unresolved, NOT an invented
    // Resource("DataSourceParam").
    let edge = res_by_id(&t.resources, "EdgeResolver");
    assert_eq!(edge.data_source_ref, Some(RefValue::Unresolved));

    // Sub interpolating a same-template resource -> Inferred("PolicyFunction").
    let sub = res_by_id(&t.resources, "SubResolver");
    assert_eq!(
        sub.data_source_ref,
        Some(RefValue::Inferred("PolicyFunction".into()))
    );

    // No reference field anywhere is a Resource for a non-resource id.
    for r in &t.resources {
        for rf in r
            .role_refs
            .iter()
            .map(Some)
            .chain([
                r.data_source_ref.as_ref(),
                r.lambda_ref.as_ref(),
                r.api_ref.as_ref(),
            ])
            .flatten()
        {
            if let RefValue::Resource(id) = rf {
                assert_ne!(id, "DataSourceParam", "parameter must never be a Resource");
                assert_ne!(
                    id, "SharedRoleArnExport",
                    "import target must never be a Resource"
                );
            }
        }
    }
}

// ── Test 5: malformed -> Parse (no panic); non-template -> Parse, not garbage ─

#[test]
fn malformed_template_yields_parse_error() {
    let err = CfnSamAdapter
        .extract("malformed.yaml", &fixture("malformed.yaml"))
        .expect_err("malformed YAML must error, not panic");
    let InfraError::Parse { path, .. } = err;
    assert_eq!(path, "malformed.yaml");
}

#[test]
fn non_template_extract_is_parse_error_not_garbage() {
    // A buildspec has no `Resources` map: extract must Parse-error (structural),
    // not silently emit garbage resources.
    let err = CfnSamAdapter
        .extract("buildspec.yml", &fixture("not_a_template.yml"))
        .expect_err("non-template must error");
    let InfraError::Parse { msg, .. } = err;
    assert!(
        msg.contains("Resources"),
        "expected a Resources structural error, got {msg:?}"
    );
}

// ── Test 6: GetAtt with a dotted attr keeps the id, drops the attr ──────────

#[test]
fn getatt_dotted_attr_yields_resource_id_only() {
    let t = CfnSamAdapter
        .extract("template.yaml", &fixture("sam_appsync.yaml"))
        .expect("parses");

    // Role: !GetAtt PolicyRole.Arn  ->  Resource("PolicyRole")  (".Arn" dropped).
    let func = res_by_id(&t.resources, "PolicyFunction");
    assert_eq!(
        func.role_refs,
        vec![RefValue::Resource("PolicyRole".into())]
    );

    // ApiId: !GetAtt Api.ApiId  ->  Resource("Api").
    let ds = res_by_id(&t.resources, "PolicyDS");
    assert_eq!(ds.api_ref, Some(RefValue::Resource("Api".into())));
}

// ── Test 7: real-world robustness (block scalars, conditions, flow, unknown) ──
//
// Distilled from a production 8,324-line SAM template that our OLD normalizer
// silently dropped: a block-scalar `!Sub |` was rewritten to `{"Fn::Sub": "|"}`,
// orphaning the indented body and killing the WHOLE parse; `!If`/`!Equals`/`!Not`
// were absent from the tag set; an unknown tag had no catch-all. This pins the
// invariant: a template AWS accepts must never hard-fail our parse on tag shape.

#[test]
fn real_world_template_survives_block_scalars_conditions_and_unknown_tags() {
    // The headline assertion: the template PARSES at all. On the old normalizer
    // this returned Err (invalid YAML from the corrupted block scalar).
    let t = CfnSamAdapter
        .extract("backend/template.yaml", &fixture("real_world.yaml"))
        .expect("real-world SAM template must parse (block scalars must not corrupt it)");

    // Both serverless functions are extracted, with handlers read directly.
    let policy_fn = res_by_id(&t.resources, "PolicyFunction");
    assert_eq!(policy_fn.kind, InfraKind::LambdaFunction);
    assert_eq!(policy_fn.handler.as_deref(), Some("app.lambda_handler"));
    assert_eq!(
        policy_fn.role_refs.first().cloned(),
        Some(RefValue::Resource("PolicyRole".into())),
        "role !GetAtt PolicyRole.Arn -> Resource, alongside the block-scalar policy"
    );
    let audit_fn = res_by_id(&t.resources, "AuditFunction");
    assert_eq!(audit_fn.kind, InfraKind::LambdaFunction);
    assert_eq!(audit_fn.handler.as_deref(), Some("audit.handler"));

    // The role with the inline block-scalar policy parsed as a plain resource;
    // the `!Sub |` body did not corrupt the surrounding structure.
    let role = res_by_id(&t.resources, "PolicyRole");
    assert_eq!(role.kind, InfraKind::IamRole);

    // The AppSync chain wired up normally despite the block-scalar mapping
    // template on the resolver.
    let ds = res_by_id(&t.resources, "PolicyDS");
    assert_eq!(ds.kind, InfraKind::AppSyncDataSource);
    assert_eq!(
        ds.lambda_ref,
        Some(RefValue::Resource("PolicyFunction".into()))
    );
    assert_eq!(ds.api_ref, Some(RefValue::Resource("Api".into())));

    // A resolver whose DataSourceName is a plain !GetAtt still links crisply,
    // even though it carries a block-scalar RequestMappingTemplate.
    let r1 = res_by_id(&t.resources, "GetPolicyStatsResolver");
    assert_eq!(r1.kind, InfraKind::AppSyncResolver);
    assert_eq!(r1.type_name.as_deref(), Some("Query"));
    assert_eq!(r1.field_name.as_deref(), Some("getPolicyStats"));
    assert_eq!(
        r1.data_source_ref,
        Some(RefValue::Resource("PolicyDS".into()))
    );

    // THE INVARIANT: a condition-wrapped reference (`DataSourceName: !If [...]`)
    // is NEVER a Resource invented by reaching through the !If. EARNED FLIP
    // (Slice 10, B2): the grader now collects both branches' same-template refs;
    // here both `!If` branches name `PolicyDS`, so it grades Inferred("PolicyDS")
    // (band 0.70) — both deployments surfaced, recall-biased. It was Unresolved
    // before the `Fn::If` grading arm existed. The honesty invariant holds:
    // Inferred is not Resource.
    let r2 = res_by_id(&t.resources, "ListContentPoliciesResolver");
    assert_eq!(
        r2.data_source_ref,
        Some(RefValue::Inferred("PolicyDS".into())),
        "a !If-wrapped DataSourceName grades Inferred (both branches name PolicyDS), never Resource"
    );
    // Belt-and-braces: NO reference field anywhere is a Resource for a
    // non-resource id reached through a condition (e.g. the parameter `Stage`).
    for r in &t.resources {
        for rf in r
            .role_refs
            .iter()
            .map(Some)
            .chain([
                r.data_source_ref.as_ref(),
                r.lambda_ref.as_ref(),
                r.api_ref.as_ref(),
            ])
            .flatten()
        {
            if let RefValue::Resource(id) = rf {
                assert_ne!(id, "Stage", "a parameter must never become a Resource");
                assert_ne!(
                    id, "SharedBus",
                    "a parameter must never become a Resource through !If"
                );
            }
        }
    }

    // The unknown future tag (`!SomeFutureTag policy-events`) survived: its
    // resource is present (Generic), the tag stripped and the value kept.
    let bus = res_by_id(&t.resources, "EventBus");
    assert_eq!(bus.kind, InfraKind::Generic);
    assert_eq!(bus.cfn_type, "AWS::Events::EventBus");
}

// ── Test 8: tags with BLOCK-style or UNTERMINATED-FLOW arguments ─────────────
//
// Distilled from two more production templates the line-based normalizer still
// corrupted after the block-scalar fix landed:
//
//   * SHAPE 1 — a short-form tag at the END of its line, whose argument is the
//     indented BLOCK below it (`Aliases: !If` / a `- !If` sequence item). The
//     normalizer wrapped the empty rest into `{"Fn::If": ""}`, turning the key
//     into an inline map and orphaning the block sequence ("did not find
//     expected '-' indicator").
//   * SHAPE 2 — a multi-line FLOW collection after a tag: the `[` opens on the
//     tag's line and closes on a LATER line (`!Join ["",` … `]]`). The rewrite
//     prepended an unbalanced `{`, so the flow never closed.
//
// The fix strips the tag in both cases instead of corrupting the document; the
// stripped value parses as a plain block/flow value (no `Fn::` map), so any
// graded reference reached only through it degrades to Unresolved — never an
// invented Resource. This pins the same invariant as Test 7 for the new shapes.

#[test]
fn real_world_template_survives_block_style_and_multiline_flow_tag_args() {
    // THE HEADLINE: the template PARSES at all. On the pre-fix normalizer this
    // returned Err (a `- {"Fn::If": ""}` / unbalanced-`{` corrupted the YAML).
    let t = CfnSamAdapter
        .extract(
            "backend/block_args.yaml",
            &fixture("real_world_block_args.yaml"),
        )
        .expect("block-style / multi-line-flow tag args must not corrupt the parse");

    // Normal wiring is unaffected: the function's same-template Role resolves,
    // alongside the multi-line `!Join` env var on the same resource.
    let func = res_by_id(&t.resources, "ApiFunction");
    assert_eq!(func.kind, InfraKind::LambdaFunction);
    assert_eq!(func.handler.as_deref(), Some("app.lambda_handler"));
    assert_eq!(func.role_refs, vec![RefValue::Resource("ApiRole".into())]);

    // The role with the `- !If` block-arg policy parsed as a plain resource.
    let role = res_by_id(&t.resources, "ApiRole");
    assert_eq!(role.kind, InfraKind::IamRole);

    // The AppSync chain wired up normally despite the block-arg/flow shapes
    // elsewhere in the document.
    let ds = res_by_id(&t.resources, "ApiDS");
    assert_eq!(ds.kind, InfraKind::AppSyncDataSource);
    assert_eq!(
        ds.lambda_ref,
        Some(RefValue::Resource("ApiFunction".into()))
    );
    assert_eq!(ds.api_ref, Some(RefValue::Resource("Api".into())));

    // A plain `!GetAtt` DataSourceName still links crisply.
    let r1 = res_by_id(&t.resources, "GetThingResolver");
    assert_eq!(r1.kind, InfraKind::AppSyncResolver);
    assert_eq!(r1.type_name.as_deref(), Some("Query"));
    assert_eq!(r1.field_name.as_deref(), Some("getThing"));
    assert_eq!(r1.data_source_ref, Some(RefValue::Resource("ApiDS".into())));

    // THE INVARIANT: a block-style `!If` on a GRADED field (DataSourceName) never
    // invents Resource("ApiDS"). EARNED FLIP (Slice 10, B2): the block-arg `!If`
    // now parses to a proper `{"Fn::If": [...]}` map (event loader), and the grader
    // collects both branches' same-template refs — both name `ApiDS`, so it grades
    // Inferred("ApiDS") (band 0.70). It was Unresolved before the grading arm. The
    // honesty invariant holds: Inferred is not Resource.
    let r2 = res_by_id(&t.resources, "ListThingsResolver");
    assert_eq!(
        r2.data_source_ref,
        Some(RefValue::Inferred("ApiDS".into())),
        "a block-style !If DataSourceName grades Inferred (both branches name ApiDS), never Resource"
    );

    // The CloudFront distribution with `Aliases: !If` (block-arg) is present as
    // a Generic resource — the block sequence under it was not orphaned.
    let dist = res_by_id(&t.resources, "Distribution");
    assert_eq!(dist.kind, InfraKind::Generic);
    assert_eq!(dist.cfn_type, "AWS::CloudFront::Distribution");

    // Belt-and-braces: no graded reference anywhere is a Resource for a
    // non-resource id reached through a condition (e.g. the `Stage`/`DomainName`
    // parameters that appear inside the wrapped/flow values).
    for r in &t.resources {
        for rf in r
            .role_refs
            .iter()
            .map(Some)
            .chain([
                r.data_source_ref.as_ref(),
                r.lambda_ref.as_ref(),
                r.api_ref.as_ref(),
            ])
            .flatten()
        {
            if let RefValue::Resource(id) = rf {
                assert_ne!(id, "Stage", "a parameter must never become a Resource");
                assert_ne!(
                    id, "DomainName",
                    "a parameter must never become a Resource through a block-arg !If"
                );
            }
        }
    }
}

// ── Test 9: multi-line FLOW `!If` with inner tags + trailing comma ───────────
//
// The latest production shape the line-based textual normalizer could not
// handle (it failed with "expected ',' or ']'"): a `!If` opening a MULTI-LINE
// FLOW sequence whose members are tagged (`!Ref`), one a QUOTED pseudo-parameter
// (`!Ref "AWS::NoValue"`), with a TRAILING COMMA before `]`. Event-level parsing
// builds the proper `{"Fn::If": [..., {"Ref": ...}]}` map directly. The
// structural correctness of that inner map is pinned by the `cfn.rs` unit test
// `inline_and_multiline_flow_if_preserve_inner_refs`; here we pin the end-to-end
// invariant: it PARSES, normal wiring is intact, and the `!If`-wrapped graded
// fields stay Unresolved — never a Resource invented through the condition.

#[test]
fn real_world_template_survives_multiline_flow_if_with_inner_tags() {
    // THE HEADLINE: the template PARSES. The textual normalizer errored here
    // ("expected ',' or ']'") on the multi-line flow + inner tags + trailing
    // comma; the event parser handles it natively.
    let t = CfnSamAdapter
        .extract("backend/flow_if.yaml", &fixture("real_world_flow_if.yaml"))
        .expect("multi-line flow !If with inner tags must parse at the event level");

    // Normal wiring is unaffected: the function's same-template Role resolves.
    let func = res_by_id(&t.resources, "ApiFunction");
    assert_eq!(func.kind, InfraKind::LambdaFunction);
    assert_eq!(func.handler.as_deref(), Some("app.lambda_handler"));
    assert_eq!(func.role_refs, vec![RefValue::Resource("ApiRole".into())]);

    // A plain `!GetAtt` DataSourceName still links crisply (control case).
    let r1 = res_by_id(&t.resources, "GetThingResolver");
    assert_eq!(r1.data_source_ref, Some(RefValue::Resource("ApiDS".into())));

    // THE INVARIANT (multi-line flow form): the `!If`-wrapped DataSourceName never
    // invents Resource("ApiDS"). EARNED FLIP (Slice 10, B2): the grader now has an
    // `Fn::If` arm — it collects both branches' same-template refs. Here `!Ref
    // ApiDS` resolves and `!Ref "AWS::NoValue"` (a pseudo-parameter) contributes
    // nothing, so it grades Inferred("ApiDS") (band 0.70). It was Unresolved before
    // the arm existed. The honesty invariant holds: Inferred is not Resource, and
    // the pseudo-parameter is never invented.
    let r2 = res_by_id(&t.resources, "ListThingsResolver");
    assert_eq!(
        r2.data_source_ref,
        Some(RefValue::Inferred("ApiDS".into())),
        "a multi-line-flow !If DataSourceName grades Inferred(ApiDS) (the only resolvable branch), never Resource"
    );

    // EARNED FLIP (inline one-line form): the `!If`-wrapped data-source
    // LambdaFunctionArn likewise grades Inferred("ApiFunction") — `!Ref ApiFunction`
    // resolves, the pseudo-parameter contributes nothing. Was Unresolved before.
    let ds = res_by_id(&t.resources, "ApiDS");
    assert_eq!(ds.api_ref, Some(RefValue::Resource("Api".into())));
    assert_eq!(
        ds.lambda_ref,
        Some(RefValue::Inferred("ApiFunction".into())),
        "an inline !If LambdaFunctionArn grades Inferred(ApiFunction) (the only resolvable branch), never Resource"
    );

    // Belt-and-braces: no graded reference anywhere is a Resource reached through
    // an `!If` (the inner `!Ref ApiFunction`/`!Ref ApiDS` live behind a condition,
    // and `AWS::NoValue` is a pseudo-parameter, never a resource).
    for r in &t.resources {
        for rf in r
            .role_refs
            .iter()
            .map(Some)
            .chain([
                r.data_source_ref.as_ref(),
                r.lambda_ref.as_ref(),
                r.api_ref.as_ref(),
            ])
            .flatten()
        {
            if let RefValue::Resource(id) = rf {
                assert_ne!(
                    id, "AWS::NoValue",
                    "a pseudo-parameter must never become a Resource"
                );
            }
        }
    }
}
