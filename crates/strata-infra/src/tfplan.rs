//! Terraform plan-JSON ingestion (Track D1, Slice 14, M2) — the PREFERRED source.
//!
//! `terraform show -json <plan>` (or `terraform show -json` over state) emits a
//! fully-RESOLVED view of the configuration: every resource that will exist,
//! including module-/`count`-/`for_each`-expanded instances raw HCL cannot
//! statically enumerate. Ingesting it is the design's "resolved over raw" win —
//! when a plan file is committed alongside the `.tf`, its resources are a
//! higher-fidelity inventory than the HCL parse.
//!
//! ## What we read (and what we don't)
//!
//! - **Inventory** comes from `planned_values.root_module` (recursing
//!   `child_modules`): each `resources[]` entry's `address` is the logical id and
//!   `type` drives the [`InfraKind`]. This is the resolved resource set.
//! - **Wiring** comes from the `configuration` block, whose
//!   `resources[].expressions.<attr>.references` arrays name the *source-level*
//!   dependencies (`["aws_iam_role.x.arn", "aws_iam_role.x"]`). A reference whose
//!   resolved address is in the plan's address set grades [`RefValue::Resource`]
//!   (Extracted) — the plan literally records the dependency, so this is a fact.
//!   A reference to something outside the plan (a `var.`/`data` input) yields no
//!   edge, never invented.
//!
//! We do NOT evaluate the plan's resolved attribute *values* (resolved ARNs are
//! opaque strings we cannot tie back to a resource); the honest, structural
//! signal is the `references` graph the `configuration` block already provides.

use serde_json::Value;

use crate::terraform::{classify, json_string_grants, opaque_grants, ROLE_ATTRS};
use crate::{
    parse_policy_document, InfraError, InfraKind, InfraResource, InfraTemplate, PolicyGrants,
    RefValue,
};

/// Whether `content` is a Terraform plan/state JSON document (as opposed to a CFN
/// template or arbitrary JSON): it has BOTH a `terraform_version` string and a
/// `planned_values` (a plan) or `values` (a state show) object. The two-key shape
/// is a strong discriminator — a CFN template (a `Resources` map) carries neither.
pub fn is_plan_json(content: &str) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(content) else {
        return false;
    };
    let has_version = v.get("terraform_version").and_then(Value::as_str).is_some();
    let has_values = v.get("planned_values").is_some() || v.get("values").is_some();
    has_version && has_values
}

/// Extract a Terraform plan/state JSON into an [`InfraTemplate`]. The resources are
/// the resolved inventory from `planned_values`/`values`; their references are
/// recovered from the `configuration` block and graded against the resolved
/// address set. A document that is not plan JSON, or will not parse, is an
/// [`InfraError::Parse`] so the caller degrades visibly.
pub fn extract_plan(path: &str, content: &str) -> Result<InfraTemplate, InfraError> {
    let v: Value = serde_json::from_str(content).map_err(|e| InfraError::Parse {
        path: path.to_string(),
        msg: format!("invalid JSON: {e}"),
    })?;

    // The resolved root module: a plan uses `planned_values`, a `terraform show
    // -json` over state uses `values`. Either way, `root_module` holds resources.
    let root = v
        .get("planned_values")
        .or_else(|| v.get("values"))
        .and_then(|pv| pv.get("root_module"))
        .ok_or_else(|| InfraError::Parse {
            path: path.to_string(),
            msg: "plan JSON missing planned_values/values.root_module".to_string(),
        })?;

    // ── Inventory: every resolved resource address + type. ──
    let mut resources: Vec<InfraResource> = Vec::new();
    collect_planned_resources(root, &mut resources);

    // The resolved address set drives reference grading: a configuration-block
    // reference resolves to `Resource` only when its address names a resource in
    // THIS plan.
    let addresses: std::collections::BTreeSet<String> =
        resources.iter().map(|r| r.logical_id.clone()).collect();

    // ── Wiring: references from the `configuration` block, keyed by address. ──
    if let Some(config_root) = v.get("configuration").and_then(|c| c.get("root_module")) {
        let refs_by_addr = collect_config_references(config_root);
        for res in &mut resources {
            apply_references(res, &refs_by_addr, &addresses);
        }
    }

    Ok(InfraTemplate {
        path: path.to_string(),
        resources,
    })
}

/// Recursively collect every resource in a `planned_values` module subtree
/// (`root_module` then each `child_modules[]`), as an inventory [`InfraResource`]
/// (id = `address`, kind from `type`). Literal `handler`/`filename` values are
/// captured for Lambdas; references are filled later from the configuration block.
fn collect_planned_resources(module: &Value, out: &mut Vec<InfraResource>) {
    if let Some(arr) = module.get("resources").and_then(Value::as_array) {
        for r in arr {
            let Some(address) = r.get("address").and_then(Value::as_str) else {
                continue; // a resource with no address is unusable — skip honestly.
            };
            let tf_type = r.get("type").and_then(Value::as_str).unwrap_or("");
            let kind = classify(tf_type);
            let mut res = InfraResource::new(address.to_string(), tf_type.to_string(), kind);

            // Resolved literal values (a plan fully resolves these).
            match kind {
                // The Lambda's handler/filename are inventory the report surfaces.
                InfraKind::LambdaFunction => {
                    let values = r.get("values");
                    res.handler = string_value(values, "handler");
                    res.code_uri =
                        string_value(values, "filename").or_else(|| string_value(values, "s3_key"));
                }
                // IAM grants from the RESOLVED policy documents (Track D2, C3): the
                // plan renders the `policy` strings / `inline_policy` blocks raw HCL
                // cannot, so a plan is a higher-fidelity grant source — graded with
                // the SAME never-confident-wrong discipline as the HCL path.
                InfraKind::IamRole | InfraKind::IamPolicy => {
                    let grants = plan_iam_grants(tf_type, r.get("values"));
                    res.granted_actions = grants.allow_actions;
                    res.grants_opaque = grants.opaque_reasons;
                }
                _ => {}
            }
            out.push(res);
        }
    }
    if let Some(children) = module.get("child_modules").and_then(Value::as_array) {
        for child in children {
            collect_planned_resources(child, out);
        }
    }
}

/// A resolved string attribute under `values`, or `None`.
fn string_value(values: Option<&Value>, key: &str) -> Option<String> {
    values?.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Grants from a RESOLVED IAM resource's `values` (Track D2, C3). A plan renders
/// the policy documents raw HCL cannot, so these are higher-fidelity than the HCL
/// parse — but graded with the SAME never-confident-wrong discipline (the shared
/// [`json_string_grants`]/[`parse_policy_document`]/[`opaque_grants`] helpers), so a
/// resolved plan and raw HCL grade an identical policy identically:
/// - `aws_iam_role_policy` / `aws_iam_policy`: a resolved `policy` JSON string is
///   parsed; a `policy` ABSENT from `values` is known-after-apply → opaque
///   (`unknown-at-plan-time`), so the targeted role is INDETERMINATE rather than
///   falsely grant-less (which would manufacture a false permission gap).
/// - `aws_iam_role_policy_attachment`: no `policy`; the attached managed/foreign
///   policy is not enumerated → opaque (`policy-attachment:<arn>`).
/// - `aws_iam_role`: resolved `inline_policy[].policy` documents are parsed;
///   `managed_policy_arns` are opaque (`managed-policy:<arn>`), never enumerated.
fn plan_iam_grants(tf_type: &str, values: Option<&Value>) -> PolicyGrants {
    match tf_type {
        "aws_iam_role_policy" | "aws_iam_policy" => match values.and_then(|v| v.get("policy")) {
            Some(Value::String(s)) => json_string_grants(s),
            Some(doc @ Value::Object(_)) => parse_policy_document(doc),
            // A required `policy` absent from resolved values is known-after-apply.
            _ => opaque_grants("unknown-at-plan-time"),
        },
        "aws_iam_role_policy_attachment" => {
            let reason = match values
                .and_then(|v| v.get("policy_arn"))
                .and_then(Value::as_str)
            {
                Some(arn) => format!("policy-attachment:{arn}"),
                None => "policy-attachment".to_string(),
            };
            opaque_grants(&reason)
        }
        "aws_iam_role" => {
            let mut grants = PolicyGrants::default();
            if let Some(arr) = values
                .and_then(|v| v.get("inline_policy"))
                .and_then(Value::as_array)
            {
                for ip in arr {
                    match ip.get("policy") {
                        Some(Value::String(s)) => grants.merge(json_string_grants(s)),
                        Some(doc @ Value::Object(_)) => grants.merge(parse_policy_document(doc)),
                        // present but null/unknown (known-after-apply) or an absent
                        // `policy`: opaque, never silently dropped.
                        _ => grants.mark_opaque("unknown-at-plan-time"),
                    }
                }
            }
            if let Some(arr) = values
                .and_then(|v| v.get("managed_policy_arns"))
                .and_then(Value::as_array)
            {
                for el in arr {
                    match el.as_str() {
                        Some(arn) => grants.mark_opaque(format!("managed-policy:{arn}")),
                        None => grants.mark_opaque("managed-policy:dynamic"),
                    }
                }
            }
            grants
        }
        // The caller gates on IamRole/IamPolicy kinds, so every IAM tf_type is
        // covered above; any other type contributes no grants.
        _ => PolicyGrants::default(),
    }
}

/// `resource address → its expression references`, harvested from the
/// `configuration` block (root + every nested `module_calls[].module`). The
/// `configuration` block addresses resources WITHOUT a `module.` prefix even
/// inside a module call, so we re-qualify a child resource's address with the
/// module-call path to match the `planned_values` `module.<name>.…` address.
fn collect_config_references(config_module: &Value) -> std::collections::BTreeMap<String, Refs> {
    let mut out = std::collections::BTreeMap::new();
    collect_config_module(config_module, "", &mut out);
    out
}

/// The references a single resource declares, split into role refs (any
/// [`ROLE_ATTRS`] attribute) and the AppSync-chain refs (`api_id`,
/// `data_source`, `lambda_config`'s `function_arn`).
#[derive(Default)]
struct Refs {
    role: Vec<String>,
    api: Option<String>,
    data_source: Option<String>,
    lambda: Option<String>,
}

/// Walk one configuration module, qualifying resource addresses with `prefix`
/// (the `module.<name>` path, empty at the root) and recursing into
/// `module_calls`.
fn collect_config_module(
    module: &Value,
    prefix: &str,
    out: &mut std::collections::BTreeMap<String, Refs>,
) {
    if let Some(arr) = module.get("resources").and_then(Value::as_array) {
        for r in arr {
            let Some(local_addr) = r.get("address").and_then(Value::as_str) else {
                continue;
            };
            let address = if prefix.is_empty() {
                local_addr.to_string()
            } else {
                format!("{prefix}.{local_addr}")
            };
            let expressions = r.get("expressions");
            let mut refs = Refs::default();
            for attr in ROLE_ATTRS {
                if let Some(first) = first_reference(expressions, attr) {
                    refs.role.push(first);
                }
            }
            refs.api = first_reference(expressions, "api_id");
            refs.data_source = first_reference(expressions, "data_source");
            // `lambda_config` is a nested block: its `function_arn` references live
            // under `expressions.lambda_config[0].function_arn.references` (a block
            // is an array of objects in the configuration schema).
            refs.lambda = nested_block_reference(expressions, "lambda_config", "function_arn");
            out.insert(address, refs);
        }
    }
    // Nested module calls: `module_calls.<name>.module` is the child configuration.
    if let Some(calls) = module.get("module_calls").and_then(Value::as_object) {
        for (name, call) in calls {
            if let Some(child) = call.get("module") {
                let child_prefix = if prefix.is_empty() {
                    format!("module.{name}")
                } else {
                    format!("{prefix}.module.{name}")
                };
                collect_config_module(child, &child_prefix, out);
            }
        }
    }
}

/// The FIRST `references[]` entry of `expressions.<attr>` that is a resource
/// address (head + name, e.g. `aws_iam_role.x` recovered from `aws_iam_role.x.arn`),
/// or `None`. The configuration block lists both the attribute-qualified and the
/// bare-resource reference; we normalize to the resource address.
fn first_reference(expressions: Option<&Value>, attr: &str) -> Option<String> {
    let refs = expressions?.get(attr)?.get("references")?.as_array()?;
    refs.iter()
        .filter_map(Value::as_str)
        .find_map(reference_to_address)
}

/// A `function_arn` reference inside a nested `lambda_config` block expression.
fn nested_block_reference(expressions: Option<&Value>, block: &str, attr: &str) -> Option<String> {
    let block_val = expressions?.get(block)?;
    // A nested block is an array of block instances in the configuration schema.
    let first = block_val
        .as_array()
        .and_then(|a| a.first())
        .unwrap_or(block_val);
    first
        .get(attr)?
        .get("references")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .find_map(reference_to_address)
}

/// Normalize a configuration `references` string to a resource address we can
/// match against the plan's address set. `"aws_iam_role.x.arn"` → `aws_iam_role.x`;
/// `"aws_iam_role.x"` → `aws_iam_role.x`; a `var.`/`local.`/`each.`/`count.`/
/// `data.` reference → `None` (not a managed resource address). `module.x.out` is
/// kept as `module.x.out`-free — a module-output reference resolves to no single
/// managed resource here, so `None`.
fn reference_to_address(reference: &str) -> Option<String> {
    let mut parts = reference.split('.');
    let head = parts.next()?;
    match head {
        "var" | "local" | "each" | "count" | "self" | "path" | "terraform" | "module" | "data" => {
            None
        }
        // A managed resource reference: `<type>.<name>[.<attr>…]` → `<type>.<name>`.
        _ => {
            let name = parts.next()?;
            Some(format!("{head}.{name}"))
        }
    }
}

/// Apply a resource's harvested references (graded against the plan address set)
/// to its [`InfraResource`] fields. A reference naming a plan resource →
/// [`RefValue::Resource`] (Extracted — the plan records the dependency as a fact);
/// anything else contributes no ref (never invented).
fn apply_references(
    res: &mut InfraResource,
    refs_by_addr: &std::collections::BTreeMap<String, Refs>,
    addresses: &std::collections::BTreeSet<String>,
) {
    let Some(refs) = refs_by_addr.get(&res.logical_id) else {
        return;
    };
    let graded = |addr: &str| -> Option<RefValue> {
        if addresses.contains(addr) {
            Some(RefValue::Resource(addr.to_string()))
        } else {
            None
        }
    };

    for role in &refs.role {
        if let Some(rv) = graded(role) {
            res.role_refs.push(rv);
        }
    }
    match res.kind {
        InfraKind::AppSyncDataSource => {
            res.api_ref = refs.api.as_deref().and_then(graded);
            res.lambda_ref = refs.lambda.as_deref().and_then(graded);
        }
        InfraKind::AppSyncResolver => {
            res.api_ref = refs.api.as_deref().and_then(graded);
            res.data_source_ref = refs.data_source.as_deref().and_then(graded);
        }
        _ => {}
    }
}

/// Merge plan-JSON templates into the HCL-extracted set, deduping by resource
/// address: a plan-JSON resource SUPERSEDES an HCL one with the same address (the
/// "resolved over raw" preference). An HCL resource with no plan counterpart is
/// kept. The result is per-source [`InfraTemplate`]s with HCL duplicates pruned.
///
/// This is additive and order-stable: `plan_templates` win on conflict, HCL
/// templates keep their surviving (non-superseded) resources in original order.
pub fn dedup_plan_over_hcl(
    hcl_templates: Vec<InfraTemplate>,
    plan_templates: Vec<InfraTemplate>,
) -> Vec<InfraTemplate> {
    // The set of addresses the plan(s) resolve — these win.
    let plan_addrs: std::collections::BTreeSet<String> = plan_templates
        .iter()
        .flat_map(|t| t.resources.iter().map(|r| r.logical_id.clone()))
        .collect();

    let mut out = Vec::new();
    for mut t in hcl_templates {
        t.resources.retain(|r| !plan_addrs.contains(&r.logical_id));
        if !t.resources.is_empty() {
            out.push(t);
        }
    }
    out.extend(plan_templates);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal but realistic `terraform show -json` plan: a Lambda assuming a
    /// role, both module-expanded, with the references in the configuration block.
    const PLAN: &str = r#"{
      "format_version": "1.0",
      "terraform_version": "1.5.7",
      "planned_values": {
        "root_module": {
          "resources": [
            { "address": "aws_iam_role.exec", "type": "aws_iam_role", "name": "exec", "values": {} },
            { "address": "aws_lambda_function.api", "type": "aws_lambda_function", "name": "api",
              "values": { "handler": "index.handler", "filename": "build/api.zip" } }
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
              "expressions": {
                "role": { "references": ["aws_iam_role.exec.arn", "aws_iam_role.exec"] },
                "handler": { "constant_value": "index.handler" }
              }
            }
          ]
        }
      }
    }"#;

    #[test]
    fn detects_plan_json_shape() {
        assert!(is_plan_json(PLAN));
        // A CFN template is not plan JSON.
        assert!(!is_plan_json(
            r#"{"Resources": {"R": {"Type": "AWS::X::Y"}}}"#
        ));
        // Arbitrary JSON is not plan JSON.
        assert!(!is_plan_json(r#"{"name": "x", "version": "1.0"}"#));
    }

    #[test]
    fn extracts_resolved_inventory_including_module_expanded() {
        let t = extract_plan("plan.json", PLAN).expect("plan parses");
        let ids: Vec<&str> = t.resources.iter().map(|r| r.logical_id.as_str()).collect();
        // Root resources AND the module-expanded one (raw HCL cannot enumerate the
        // latter without evaluating the module) — the resolved-over-raw win.
        assert!(ids.contains(&"aws_iam_role.exec"));
        assert!(ids.contains(&"aws_lambda_function.api"));
        assert!(
            ids.contains(&"module.q.aws_sqs_queue.main"),
            "module-expanded resource present: {ids:?}"
        );
    }

    #[test]
    fn grades_configuration_references_as_resource() {
        let t = extract_plan("plan.json", PLAN).expect("parses");
        let lambda = t
            .resources
            .iter()
            .find(|r| r.logical_id == "aws_lambda_function.api")
            .expect("lambda present");
        // The `role` reference names a plan resource → Resource (Extracted), the
        // resolved-dependency fact.
        assert_eq!(
            lambda.role_refs,
            vec![RefValue::Resource("aws_iam_role.exec".to_string())]
        );
        // The resolved literal handler is captured inventory.
        assert_eq!(lambda.handler.as_deref(), Some("index.handler"));
        assert_eq!(lambda.code_uri.as_deref(), Some("build/api.zip"));
        assert_eq!(lambda.kind, InfraKind::LambdaFunction);
    }

    /// A resolved plan carrying IAM resources (Track D2, C3): a role with an inline
    /// policy + a managed ARN; a standalone `aws_iam_role_policy` (resolved `policy`
    /// string) targeting that role; a role-policy whose `policy` is unknown at plan
    /// time (absent from `values`); and an `aws_iam_role_policy_attachment`.
    const PLAN_IAM: &str = r#"{
      "format_version": "1.0",
      "terraform_version": "1.6.0",
      "planned_values": {
        "root_module": {
          "resources": [
            { "address": "aws_iam_role.exec", "type": "aws_iam_role", "name": "exec",
              "values": {
                "name": "exec",
                "managed_policy_arns": ["arn:aws:iam::aws:policy/AmazonS3ReadOnlyAccess"],
                "inline_policy": [
                  { "name": "logs", "policy": "{\"Statement\":[{\"Effect\":\"Allow\",\"Action\":\"logs:CreateLogStream\",\"Resource\":\"*\"}]}" }
                ]
              }
            },
            { "address": "aws_iam_role_policy.inline", "type": "aws_iam_role_policy", "name": "inline",
              "values": {
                "policy": "{\"Statement\":[{\"Effect\":\"Allow\",\"Action\":[\"dynamodb:PutItem\",\"dynamodb:GetItem\"],\"Resource\":\"*\"}]}"
              }
            },
            { "address": "aws_iam_role_policy.unknown", "type": "aws_iam_role_policy", "name": "unknown",
              "values": {}
            },
            { "address": "aws_iam_role_policy_attachment.a", "type": "aws_iam_role_policy_attachment", "name": "a",
              "values": { "policy_arn": "arn:aws:iam::aws:policy/AmazonSQSFullAccess" }
            }
          ]
        }
      },
      "configuration": {
        "root_module": {
          "resources": [
            { "address": "aws_iam_role_policy.inline", "type": "aws_iam_role_policy",
              "expressions": { "role": { "references": ["aws_iam_role.exec.id", "aws_iam_role.exec"] } } },
            { "address": "aws_iam_role_policy.unknown", "type": "aws_iam_role_policy",
              "expressions": { "role": { "references": ["aws_iam_role.exec.id", "aws_iam_role.exec"] } } }
          ]
        }
      }
    }"#;

    /// Find a resolved resource by address (panics if absent — the test wants it).
    fn find_res<'a>(t: &'a InfraTemplate, addr: &str) -> &'a InfraResource {
        t.resources
            .iter()
            .find(|r| r.logical_id == addr)
            .unwrap_or_else(|| panic!("resource {addr} present"))
    }

    #[test]
    fn plan_standalone_role_policy_grants_and_targets_role() {
        let t = extract_plan("plan.json", PLAN_IAM).expect("parses");
        let p = find_res(&t, "aws_iam_role_policy.inline");
        // The plan's RESOLVED `policy` string is parsed to concrete actions.
        assert!(
            p.granted_actions.contains(&"dynamodb:PutItem".to_string()),
            "resolved policy actions, got {:?}",
            p.granted_actions
        );
        assert!(p.granted_actions.contains(&"dynamodb:GetItem".to_string()));
        // Its `role` reference (the grant TARGET) is graded against the address set.
        assert_eq!(
            p.role_refs,
            vec![RefValue::Resource("aws_iam_role.exec".to_string())]
        );
    }

    #[test]
    fn plan_role_inline_policy_and_managed_arns() {
        let t = extract_plan("plan.json", PLAN_IAM).expect("parses");
        let r = find_res(&t, "aws_iam_role.exec");
        // The role's own inline_policy is parsed; managed ARNs are opaque (not
        // enumerated), never invented as concrete actions.
        assert!(
            r.granted_actions
                .contains(&"logs:CreateLogStream".to_string()),
            "inline_policy parsed, got {:?}",
            r.granted_actions
        );
        assert!(
            r.grants_opaque
                .iter()
                .any(|o| o.starts_with("managed-policy:")),
            "managed arn opaque, got {:?}",
            r.grants_opaque
        );
    }

    #[test]
    fn plan_unknown_policy_is_opaque_never_empty() {
        let t = extract_plan("plan.json", PLAN_IAM).expect("parses");
        let u = find_res(&t, "aws_iam_role_policy.unknown");
        // A required `policy` absent from resolved values is known-after-apply →
        // opaque, so the targeted role is INDETERMINATE, never falsely grant-less
        // (which would manufacture a false permission gap).
        assert!(
            u.granted_actions.is_empty(),
            "no concrete actions invented, got {:?}",
            u.granted_actions
        );
        assert_eq!(u.grants_opaque, vec!["unknown-at-plan-time".to_string()]);
    }

    #[test]
    fn plan_role_inline_policy_unknown_value_is_opaque() {
        // An `inline_policy[].policy` that resolves to null (known-after-apply) must
        // be opaque, never silently dropped (which would under-report the role).
        const P: &str = r#"{
          "terraform_version": "1.6.0",
          "planned_values": { "root_module": { "resources": [
            { "address": "aws_iam_role.r", "type": "aws_iam_role", "name": "r",
              "values": { "inline_policy": [ { "name": "x", "policy": null } ] } }
          ] } }
        }"#;
        let t = extract_plan("plan.json", P).expect("parses");
        let r = find_res(&t, "aws_iam_role.r");
        assert_eq!(
            r.grants_opaque,
            vec!["unknown-at-plan-time".to_string()],
            "an unknown inline_policy value must be opaque, got {:?}",
            r.grants_opaque
        );
    }

    #[test]
    fn plan_policy_attachment_is_opaque() {
        let t = extract_plan("plan.json", PLAN_IAM).expect("parses");
        let a = find_res(&t, "aws_iam_role_policy_attachment.a");
        assert!(
            a.grants_opaque
                .iter()
                .any(|o| o.starts_with("policy-attachment:")),
            "attachment opaque, got {:?}",
            a.grants_opaque
        );
    }

    #[test]
    fn dedup_prefers_plan_over_hcl() {
        let hcl = InfraTemplate {
            path: "main.tf".to_string(),
            resources: vec![
                InfraResource::new(
                    "aws_lambda_function.api".to_string(),
                    "aws_lambda_function".to_string(),
                    InfraKind::LambdaFunction,
                ),
                InfraResource::new(
                    "aws_iam_role.only_in_hcl".to_string(),
                    "aws_iam_role".to_string(),
                    InfraKind::IamRole,
                ),
            ],
        };
        let plan = InfraTemplate {
            path: "plan.json".to_string(),
            resources: vec![InfraResource::new(
                "aws_lambda_function.api".to_string(),
                "aws_lambda_function".to_string(),
                InfraKind::LambdaFunction,
            )],
        };
        let merged = dedup_plan_over_hcl(vec![hcl], vec![plan]);
        let all: Vec<&str> = merged
            .iter()
            .flat_map(|t| t.resources.iter().map(|r| r.logical_id.as_str()))
            .collect();
        // The HCL Lambda is superseded by the plan's; the HCL-only role survives.
        assert_eq!(
            all.iter()
                .filter(|a| **a == "aws_lambda_function.api")
                .count(),
            1
        );
        assert!(all.contains(&"aws_iam_role.only_in_hcl"));
    }
}
