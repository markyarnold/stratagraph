//! The Terraform / OpenTofu adapter (Track D1, Slice 14).
//!
//! A `.tf`/`.tofu` file is HCL: a sequence of blocks. The ones this adapter
//! extracts are `resource "<type>" "<name>"`, `data "<type>" "<name>"`, and
//! `module "<name>"`. Each becomes an [`InfraResource`] with a Terraform-shaped
//! logical id — the `type.name` *address* (`aws_lambda_function.api`), the same
//! identity Terraform itself uses. The `cfn_type`-analogue field carries the TF
//! type string (`aws_lambda_function`), so the rest of the infra plane (node
//! kinds, wiring, the `Runs`/`PRODUCES` bridges in `strata-index`) is reused
//! unchanged.
//!
//! ## Scope: static structure only (NO evaluation)
//!
//! This is deliberately a *structural* extraction. We parse HCL and read the
//! blocks and their references; we do NOT evaluate variables (`var.`), locals
//! (`local.`), functions (`jsonencode(…)`, `find_in_parent_folders()`), module
//! outputs (`module.x.out`), or `for`/conditional expressions — evaluating those
//! is reimplementing Terraform/Terragrunt and is out of scope. An expression we
//! cannot tie to a same-file resource with certainty is surfaced honestly, never
//! invented (see grading below).
//!
//! ## Reference grading (honesty, mirrors [`RefValue`])
//!
//! A reference value is graded exactly like the CFN adapter's `Ref`/`GetAtt`:
//! - A traversal naming a same-file resource address (`role =
//!   aws_iam_role.x.arn`) → [`RefValue::Resource`] (Extracted) — a config *fact*.
//! - An interpolated string that *embeds* a same-file resource address
//!   (`"…/${aws_iam_role.x.name}"`) → [`RefValue::Inferred`] — a best-effort id
//!   recovered from interpolation, not a crisp reference.
//! - A `var.`/`local.`/`module.`/`data.`/`each.`/… reference, a cross-file
//!   reference, or an interpolation that embeds no same-file resource →
//!   [`RefValue::Unresolved`] — surfaced by its absence, never a guessed
//!   `Resource`.
//! - A plain string → [`RefValue::Literal`].

use std::collections::BTreeSet;

use hcl::expr::{ObjectKey, Traversal, TraversalOperator};
use hcl::structure::{Block, Body, Structure};
use hcl::template::Element;
use hcl::{Expression, Template};

use crate::iam::{parse_policy_document, PolicyGrants};
use crate::{IacAdapter, InfraError, InfraKind, InfraResource, InfraTemplate, RefValue};

/// Adapter for Terraform / OpenTofu configuration (`.tf` / `.tofu`).
pub struct TerraformAdapter;

/// The cheap textual signal that a file is *attempting* to be a Terraform config:
/// it mentions a `resource "`, `data "`, or `module "` block opener. A `.tfvars`
/// file (bare attributes) and arbitrary HCL/JSON carry none, so this avoids the
/// full HCL parse for the common rejects — mirroring the CFN adapter's
/// `Resources`+`AWS::` pre-check.
fn has_terraform_textual_signal(content: &str) -> bool {
    content.contains("resource \"") || content.contains("data \"") || content.contains("module \"")
}

impl IacAdapter for TerraformAdapter {
    /// Detect a Terraform config: the cheap textual signal AND a successful HCL
    /// parse that yields at least one `resource`/`data`/`module` block. A
    /// `.tfvars` (no such block) and non-HCL both return `false`.
    fn detects(&self, _filename: &str, content: &str) -> bool {
        if !has_terraform_textual_signal(content) {
            return false;
        }
        match hcl::from_str::<Body>(content) {
            Ok(body) => body.iter().any(is_terraform_block),
            Err(_) => false,
        }
    }

    /// Parse a `.tf` config into an [`InfraTemplate`]. A malformed config returns
    /// [`InfraError::Parse`] so the caller degrades (skips it, keeps indexing)
    /// rather than crashing, and never yields a partial extraction.
    fn extract(&self, path: &str, content: &str) -> Result<InfraTemplate, InfraError> {
        let body = hcl::from_str::<Body>(content).map_err(|e| InfraError::Parse {
            path: path.to_string(),
            msg: format!("invalid HCL: {e}"),
        })?;

        // The id set drives reference grading: a traversal only resolves to a
        // `Resource` when its address names a block in THIS same file. Built in a
        // first pass so a forward reference (a resource defined later in the file)
        // still resolves — declaration order is irrelevant in HCL.
        let ids = collect_addresses(&body);

        let mut out = Vec::new();
        for structure in body.iter() {
            if let Structure::Block(block) = structure {
                if let Some(res) = extract_block(block, &ids) {
                    out.push(res);
                }
            }
        }

        Ok(InfraTemplate {
            path: path.to_string(),
            resources: out,
        })
    }
}

/// Whether a block is one we extract (`resource`/`data`/`module`).
fn is_terraform_block(structure: &Structure) -> bool {
    matches!(structure, Structure::Block(b) if matches!(b.identifier(), "resource" | "data" | "module"))
}

/// The set of same-file block addresses, for reference grading. A `resource`/
/// `data`/`module` block's address is the identity a traversal can name:
/// - `resource "T" "N"` → `T.N`
/// - `data "T" "N"` → `data.T.N`
/// - `module "N"` → `module.N`
fn collect_addresses(body: &Body) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for structure in body.iter() {
        if let Structure::Block(block) = structure {
            if let Some(addr) = block_address(block) {
                ids.insert(addr);
            }
        }
    }
    ids
}

/// The address (logical id) of an extractable block, or `None` for a block we do
/// not extract (`variable`/`locals`/`provider`/`terraform`/…) or a malformed one
/// missing its labels.
fn block_address(block: &Block) -> Option<String> {
    let labels: Vec<&str> = block.labels().iter().map(|l| l.as_str()).collect();
    match block.identifier() {
        // resource "T" "N" → T.N
        "resource" => match labels.as_slice() {
            [ty, name] => Some(format!("{ty}.{name}")),
            _ => None,
        },
        // data "T" "N" → data.T.N (the traversal head for a data ref is `data`).
        "data" => match labels.as_slice() {
            [ty, name] => Some(format!("data.{ty}.{name}")),
            _ => None,
        },
        // module "N" → module.N
        "module" => match labels.as_slice() {
            [name] => Some(format!("module.{name}")),
            _ => None,
        },
        _ => None,
    }
}

/// Extract one `resource`/`data`/`module` block into an [`InfraResource`], or
/// `None` for a non-extractable / malformed block. The vertical fields are filled
/// only for the matching kind; everything else is inventory (id + type).
fn extract_block(block: &Block, ids: &BTreeSet<String>) -> Option<InfraResource> {
    let logical_id = block_address(block)?;
    let tf_type = terraform_type(block);
    let kind = classify(&tf_type);
    let mut res = InfraResource::new(logical_id, tf_type, kind);

    let body = block.body();

    match kind {
        InfraKind::LambdaFunction => {
            res.handler = literal_attr(body, "handler");
            // The local artifact path: `filename` (a zip) is the closest TF
            // analogue of CFN's `CodeUri`. `s3_key` is the S3-packaged path.
            res.code_uri = literal_attr(body, "filename").or_else(|| literal_attr(body, "s3_key"));
        }
        InfraKind::AppSyncResolver => {
            // TF AppSync resolver: `type`/`field` are the GraphQL type/field, and
            // `data_source` names the backing data source (by its `name`).
            res.type_name = literal_attr(body, "type");
            res.field_name = literal_attr(body, "field");
            res.data_source_ref = graded_attr(body, "data_source", ids);
            res.api_ref = graded_attr(body, "api_id", ids);
        }
        InfraKind::AppSyncDataSource => {
            res.api_ref = graded_attr(body, "api_id", ids);
            // `lambda_config { function_arn = … }` — the data source → function
            // edge (a nested block in TF, an attribute path in CFN).
            res.lambda_ref = nested_block_attr(body, "lambda_config", "function_arn")
                .map(|expr| grade(&expr, ids));
        }
        InfraKind::IamRole => {
            // An `aws_iam_role`'s own grants: inline `inline_policy { policy = … }`
            // blocks (parsed) and `managed_policy_arns` (opaque — not enumerated).
            // Track D2, C3.
            let grants = role_inline_grants(body);
            res.granted_actions = grants.allow_actions;
            res.grants_opaque = grants.opaque_reasons;
        }
        InfraKind::IamPolicy => {
            // `aws_iam_role_policy` / `aws_iam_policy`: a `policy` document grants to
            // the role(s) it targets (the `role` ref, captured by the role scan
            // below). `aws_iam_role_policy_attachment` has no `policy` → opaque, so
            // its role is INDETERMINATE rather than silently under-reported (which
            // would manufacture false gaps). Track D2, C3.
            let grants = match attr_expr(body, "policy") {
                Some(expr) => tf_policy_grants(expr),
                None => {
                    let reason = match literal_attr(body, "policy_arn") {
                        Some(arn) => format!("policy-attachment:{arn}"),
                        None => "policy-attachment".to_string(),
                    };
                    opaque_grants(&reason)
                }
            };
            res.granted_actions = grants.allow_actions;
            res.grants_opaque = grants.opaque_reasons;
        }
        InfraKind::AppSyncApi | InfraKind::Generic => {}
    }

    // Role references are NOT kind-specific (mirrors the CFN scan): TF spreads
    // them across `role`, `role_arn`, `service_role_arn`, `execution_role_arn`,
    // `task_role_arn` on many resource types. Scan every block for all of them,
    // graded individually, so a role node always has its assumers.
    for attr in ROLE_ATTRS {
        if let Some(graded) = graded_attr(body, attr, ids) {
            res.role_refs.push(graded);
        }
    }

    Some(res)
}

/// The Terraform type string of a block: the FIRST label of a `resource`/`data`
/// block (`aws_lambda_function`), or `module` for a module block (whose "type" is
/// the module itself). An empty string for a malformed block with no labels.
fn terraform_type(block: &Block) -> String {
    match block.identifier() {
        "resource" | "data" => block
            .labels()
            .first()
            .map(|l| l.as_str().to_string())
            .unwrap_or_default(),
        "module" => "module".to_string(),
        other => other.to_string(),
    }
}

/// The top-level attribute names that reference an IAM role across TF resource
/// types (the snake_case analogues of the CFN `ROLE_PROPS`). Shared with the
/// plan-JSON path so both grade role references from the same attribute set.
pub(crate) const ROLE_ATTRS: [&str; 5] = [
    "role",
    "role_arn",
    "service_role_arn",
    "execution_role_arn",
    "task_role_arn",
];

/// Classify a Terraform type string into an [`InfraKind`]. The infrastructure
/// vertical's types are first-class; every other `aws_*` (and every unknown
/// provider — `google_*`, `azurerm_*`, …) is [`Generic`](InfraKind::Generic)
/// inventory, NEVER dropped. Shared with the plan-JSON path ([`crate::tfplan`]),
/// so a resource is classified identically whether read from raw HCL or a
/// resolved plan.
pub(crate) fn classify(tf_type: &str) -> InfraKind {
    match tf_type {
        "aws_lambda_function" => InfraKind::LambdaFunction,
        "aws_iam_role" => InfraKind::IamRole,
        // The TF analogues of a CFN inline/managed policy: each grants to a role
        // (Track D2, C3). `aws_iam_role_policy` (inline) and `aws_iam_policy`
        // (standalone) carry a `policy` document; `aws_iam_role_policy_attachment`
        // links a role to a managed/foreign policy we cannot enumerate (opaque).
        "aws_iam_role_policy" | "aws_iam_policy" | "aws_iam_role_policy_attachment" => {
            InfraKind::IamPolicy
        }
        "aws_appsync_graphql_api" => InfraKind::AppSyncApi,
        "aws_appsync_resolver" => InfraKind::AppSyncResolver,
        "aws_appsync_datasource" => InfraKind::AppSyncDataSource,
        _ => InfraKind::Generic,
    }
}

/// A literal string attribute of `body`, or `None` if absent or not a plain
/// string (an interpolation/traversal is not a literal).
fn literal_attr(body: &Body, key: &str) -> Option<String> {
    match attr_expr(body, key)? {
        Expression::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// Grade attribute `key` of `body` if present, else `None`.
fn graded_attr(body: &Body, key: &str, ids: &BTreeSet<String>) -> Option<RefValue> {
    attr_expr(body, key).map(|e| grade(e, ids))
}

/// The expression of attribute `key` directly under `body` (not recursing into
/// nested blocks), or `None`.
fn attr_expr<'a>(body: &'a Body, key: &str) -> Option<&'a Expression> {
    body.iter().find_map(|s| match s {
        Structure::Attribute(a) if a.key() == key => Some(a.expr()),
        _ => None,
    })
}

/// The expression of `attr` inside the FIRST nested block named `block_name`
/// under `body` (e.g. `lambda_config { function_arn = … }`), cloned so the
/// caller can grade it. `None` if the nested block or attribute is absent.
fn nested_block_attr(body: &Body, block_name: &str, attr: &str) -> Option<Expression> {
    body.iter().find_map(|s| match s {
        Structure::Block(b) if b.identifier() == block_name => attr_expr(b.body(), attr).cloned(),
        _ => None,
    })
}

/// Grade a reference expression with provenance per [`RefValue`].
///
/// - A plain string → [`Literal`](RefValue::Literal).
/// - A traversal whose `type.name` (or `data.type.name`) address is a same-file
///   block → [`Resource`](RefValue::Resource); a traversal to anything else
///   (`var.`/`local.`/`module.`/`each.`/a cross-file resource) →
///   [`Unresolved`](RefValue::Unresolved).
/// - A template (interpolated string) embedding a same-file resource address →
///   [`Inferred`](RefValue::Inferred); embedding none → `Unresolved`.
/// - Any other expression (function call, object, for-expr, …) → `Unresolved`
///   (we do not evaluate, so we cannot name a target — never invented).
fn grade(expr: &Expression, ids: &BTreeSet<String>) -> RefValue {
    match expr {
        Expression::String(s) => RefValue::Literal(s.clone()),
        Expression::Traversal(t) => match traversal_resource_address(t) {
            Some(addr) if ids.contains(&addr) => RefValue::Resource(addr),
            // A traversal that names no same-file resource (a variable, local,
            // module output, or a resource from another file) — Unresolved, never
            // invented.
            _ => RefValue::Unresolved,
        },
        Expression::TemplateExpr(te) => match Template::from_expr(te) {
            Ok(tmpl) => match recover_template_resource(&tmpl, ids) {
                Some(addr) => RefValue::Inferred(addr),
                None => RefValue::Unresolved,
            },
            // A template that will not re-parse is opaque to us — Unresolved.
            Err(_) => RefValue::Unresolved,
        },
        // Function calls, objects, arrays, conditionals, for-exprs, raw numbers/
        // bools: not a same-file resource we can name. Honest Unresolved.
        _ => RefValue::Unresolved,
    }
}

/// The same-file resource address a traversal names, if any. A traversal's head
/// `Variable` plus its first `GetAttr` form the `type.name` address
/// (`aws_iam_role.lambda_exec.arn` → `aws_iam_role.lambda_exec`). A `data.` head
/// is special: the address is `data.type.name` (head + first TWO attrs). A
/// special head with no resource shape (`var`/`local`/`module`/`each`/`count`/
/// `self`/`path`/`terraform`/`dependency`) yields `None`.
fn traversal_resource_address(t: &Traversal) -> Option<String> {
    let Expression::Variable(head) = &t.expr else {
        return None;
    };
    let head = head.as_str();
    let attrs = get_attrs(t);

    match head {
        // A reference to a `data` source: data.<type>.<name>.
        "data" => match attrs.as_slice() {
            [ty, name, ..] => Some(format!("data.{ty}.{name}")),
            _ => None,
        },
        // A `module.<name>` output reference is cross-module — we never evaluate
        // module outputs, so it is NOT a same-file resource address.
        "module" => None,
        // Variable / local / meta heads never name a same-file resource.
        "var" | "local" | "each" | "count" | "self" | "path" | "terraform" | "dependency" => None,
        // Otherwise the head is a resource TYPE (`aws_iam_role`, `google_*`, …):
        // head + first attr is the resource address `type.name`.
        _ => attrs.first().map(|name| format!("{head}.{name}")),
    }
}

/// The `GetAttr` identifiers of a traversal, in order (index/splat ops skipped) —
/// e.g. `aws_x.y[0].z` → `["y", "z"]`.
fn get_attrs(t: &Traversal) -> Vec<&str> {
    t.operators
        .iter()
        .filter_map(|op| match op {
            TraversalOperator::GetAttr(id) => Some(id.as_str()),
            _ => None,
        })
        .collect()
}

/// Recover the first same-file resource address embedded in a template's `${…}`
/// interpolations, if any (`"…/${aws_iam_role.r.name}"` → `aws_iam_role.r`). An
/// interpolation embedding only a `var.`/`local.`/… recovers nothing.
fn recover_template_resource(tmpl: &Template, ids: &BTreeSet<String>) -> Option<String> {
    for element in tmpl.elements() {
        if let Element::Interpolation(interp) = element {
            if let Expression::Traversal(t) = &interp.expr {
                if let Some(addr) = traversal_resource_address(t) {
                    if ids.contains(&addr) {
                        return Some(addr);
                    }
                }
            }
        }
    }
    None
}

/// A [`PolicyGrants`] carrying a single opaque reason. Shared with the plan-JSON
/// path ([`crate::tfplan`]) so HCL and resolved plans mark indeterminacy alike.
pub(crate) fn opaque_grants(reason: &str) -> PolicyGrants {
    let mut g = PolicyGrants::default();
    g.mark_opaque(reason);
    g
}

/// Parse a JSON-string policy document; a non-JSON string is `malformed-policy`.
/// Shared with the plan-JSON path ([`crate::tfplan`]), where resolved `policy`
/// attribute values arrive as JSON strings.
pub(crate) fn json_string_grants(s: &str) -> PolicyGrants {
    match serde_json::from_str::<serde_json::Value>(s) {
        Ok(v) => parse_policy_document(&v),
        Err(_) => opaque_grants("malformed-policy"),
    }
}

/// The literal string of a template with no interpolation/directive, else `None`
/// (a no-`${…}` heredoc is just a literal JSON document we can parse).
fn literal_template(tmpl: &Template) -> Option<String> {
    let mut s = String::new();
    for el in tmpl.elements() {
        match el {
            Element::Literal(lit) => s.push_str(lit),
            _ => return None,
        }
    }
    Some(s)
}

/// Convert a (partially-)literal HCL expression to JSON, substituting **null** for
/// any non-literal sub-expression (a traversal like `aws_dynamodb_table.x.arn`, a
/// function call, an interpolated template). This recovers the literal parts of an
/// IAM policy (notably the `Action` list) while a dynamic `Resource` ARN degrades
/// to null — which [`parse_policy_document`] ignores. A non-literal `Action` itself
/// becomes null and is then graded opaque, never guessed.
fn expr_to_json(expr: &Expression) -> serde_json::Value {
    use serde_json::Value as J;
    match expr {
        Expression::Null => J::Null,
        Expression::Bool(b) => J::Bool(*b),
        Expression::Number(n) => serde_json::to_value(n).unwrap_or(J::Null),
        Expression::String(s) => J::String(s.clone()),
        Expression::Array(arr) => J::Array(arr.iter().map(expr_to_json).collect()),
        Expression::Object(map) => {
            let mut obj = serde_json::Map::new();
            for (k, v) in map.iter() {
                let key = match k {
                    ObjectKey::Identifier(id) => id.as_str().to_string(),
                    ObjectKey::Expression(Expression::String(s)) => s.clone(),
                    // A non-literal key (a traversal/var) or a future key kind: skip
                    // the entry, never guessed.
                    _ => continue,
                };
                obj.insert(key, expr_to_json(v));
            }
            J::Object(obj)
        }
        Expression::Parenthesis(inner) => expr_to_json(inner),
        // Traversals, function calls, templates, conditionals, operations,
        // for-exprs, bare variables: non-literal → null placeholder.
        _ => J::Null,
    }
}

/// Parse a Terraform `policy` attribute expression into [`PolicyGrants`] (Track
/// D2, C3):
/// - a heredoc / quoted **literal JSON** policy document → parsed;
/// - `jsonencode({ Statement = [...] })` → literal parts recovered via
///   [`expr_to_json`] (a dynamic `Resource` is ignored; a dynamic `Action` →
///   opaque);
/// - a `data.aws_iam_policy_document` / `var.` / `local.` reference → opaque
///   (`dynamic-policy`), never evaluated, never guessed.
fn tf_policy_grants(expr: &Expression) -> PolicyGrants {
    match expr {
        Expression::String(s) => json_string_grants(s),
        Expression::TemplateExpr(te) => {
            match Template::from_expr(te)
                .ok()
                .and_then(|t| literal_template(&t))
            {
                Some(s) => json_string_grants(&s),
                None => opaque_grants("dynamic-policy"),
            }
        }
        Expression::FuncCall(fc) if fc.name.name.as_str() == "jsonencode" && fc.args.len() == 1 => {
            parse_policy_document(&expr_to_json(&fc.args[0]))
        }
        _ => opaque_grants("dynamic-policy"),
    }
}

/// Grants on an `aws_iam_role` from its inline `inline_policy { policy = … }`
/// blocks (parsed) and `managed_policy_arns` (opaque — AWS-managed/foreign
/// policies are not enumerated). Track D2, C3.
fn role_inline_grants(body: &Body) -> PolicyGrants {
    let mut grants = PolicyGrants::default();
    for s in body.iter() {
        if let Structure::Block(b) = s {
            if b.identifier() == "inline_policy" {
                match attr_expr(b.body(), "policy") {
                    Some(expr) => grants.merge(tf_policy_grants(expr)),
                    // An inline_policy block with no literal `policy` attribute:
                    // opaque, never silently skipped.
                    None => grants.mark_opaque("dynamic-policy"),
                }
            }
        }
    }
    match attr_expr(body, "managed_policy_arns") {
        Some(Expression::Array(arr)) => {
            for el in arr {
                match el {
                    Expression::String(arn) => grants.mark_opaque(format!("managed-policy:{arn}")),
                    _ => grants.mark_opaque("managed-policy:dynamic"),
                }
            }
        }
        // `managed_policy_arns` present but not a literal array (`= var.arns`, a
        // `local`, an expression): opaque, never treated as "no managed policies".
        Some(_) => grants.mark_opaque("managed-policy:dynamic"),
        None => {}
    }
    grants
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Grade the `v` attribute of a one-line HCL snippet against an id set.
    fn grade_v(src: &str, ids: &[&str]) -> RefValue {
        let body: Body = hcl::from_str(src).expect("parse");
        let id_set: BTreeSet<String> = ids.iter().map(|s| s.to_string()).collect();
        let expr = attr_expr(&body, "v").expect("v attribute");
        grade(expr, &id_set)
    }

    #[test]
    fn grade_same_file_resource_is_resource() {
        assert_eq!(
            grade_v("v = aws_iam_role.r.arn\n", &["aws_iam_role.r"]),
            RefValue::Resource("aws_iam_role.r".to_string())
        );
    }

    #[test]
    fn grade_unknown_resource_is_unresolved() {
        // The address is not in the id set (a resource from another file) → never
        // invented, Unresolved.
        assert_eq!(
            grade_v("v = aws_iam_role.other.arn\n", &["aws_iam_role.r"]),
            RefValue::Unresolved
        );
    }

    #[test]
    fn grade_var_local_module_is_unresolved() {
        for src in [
            "v = var.x\n",
            "v = local.x\n",
            "v = module.m.out\n",
            "v = each.value\n",
        ] {
            assert_eq!(
                grade_v(src, &["aws_iam_role.r"]),
                RefValue::Unresolved,
                "{src}"
            );
        }
    }

    #[test]
    fn grade_data_source_ref_resolves_to_data_address() {
        assert_eq!(
            grade_v(
                "v = data.aws_caller_identity.current.account_id\n",
                &["data.aws_caller_identity.current"]
            ),
            RefValue::Resource("data.aws_caller_identity.current".to_string())
        );
    }

    #[test]
    fn grade_interpolated_resource_is_inferred() {
        assert_eq!(
            grade_v("v = \"arn::${aws_iam_role.r.name}\"\n", &["aws_iam_role.r"]),
            RefValue::Inferred("aws_iam_role.r".to_string())
        );
    }

    #[test]
    fn grade_interpolated_var_only_is_unresolved() {
        assert_eq!(
            grade_v("v = \"arn::${var.role}\"\n", &["aws_iam_role.r"]),
            RefValue::Unresolved
        );
    }

    #[test]
    fn grade_function_call_is_unresolved_never_evaluated() {
        // We never evaluate functions; a jsonencode(...) / try(...) is Unresolved.
        assert_eq!(
            grade_v("v = jsonencode({ a = 1 })\n", &["aws_iam_role.r"]),
            RefValue::Unresolved
        );
        assert_eq!(
            grade_v("v = try(aws_iam_role.r.arn, \"\")\n", &["aws_iam_role.r"]),
            RefValue::Unresolved,
            "a resource buried in a function arg is NOT promoted to Resource"
        );
    }

    #[test]
    fn grade_plain_string_is_literal() {
        assert_eq!(
            grade_v("v = \"nodejs18.x\"\n", &[]),
            RefValue::Literal("nodejs18.x".to_string())
        );
    }

    /// Parse the `policy` attribute of a one-attribute HCL snippet (Track D2, C3).
    fn policy_grants_of(src: &str) -> PolicyGrants {
        let body: Body = hcl::from_str(src).expect("parse");
        let expr = attr_expr(&body, "policy").expect("policy attr");
        tf_policy_grants(expr)
    }

    #[test]
    fn tf_jsonencode_recovers_literal_actions_ignoring_dynamic_resource() {
        let g = policy_grants_of(
            "policy = jsonencode({ Statement = [{ Effect = \"Allow\", Action = [\"dynamodb:PutItem\", \"s3:GetObject\"], Resource = aws_s3_bucket.b.arn }] })\n",
        );
        assert_eq!(g.allow_actions, vec!["dynamodb:PutItem", "s3:GetObject"]);
        assert!(
            g.opaque_reasons.is_empty(),
            "a dynamic Resource is ignored, not opaque: {:?}",
            g.opaque_reasons
        );
    }

    #[test]
    fn tf_heredoc_literal_json_policy_is_parsed() {
        let g = policy_grants_of(
            "policy = <<EOT\n{\"Statement\": [{\"Effect\": \"Allow\", \"Action\": \"sqs:SendMessage\"}]}\nEOT\n",
        );
        assert_eq!(g.allow_actions, vec!["sqs:SendMessage"]);
        assert!(g.opaque_reasons.is_empty());
    }

    #[test]
    fn tf_data_source_policy_is_opaque_never_evaluated() {
        let g = policy_grants_of("policy = data.aws_iam_policy_document.doc.json\n");
        assert!(g.allow_actions.is_empty());
        assert_eq!(g.opaque_reasons, vec!["dynamic-policy"]);
    }

    #[test]
    fn tf_jsonencode_dynamic_action_is_opaque_never_guessed() {
        // A non-literal `Action` (a `var`) becomes null → graded opaque.
        let g = policy_grants_of(
            "policy = jsonencode({ Statement = [{ Effect = \"Allow\", Action = var.actions }] })\n",
        );
        assert!(g.allow_actions.is_empty());
        assert_eq!(g.opaque_reasons, vec!["dynamic-action"]);
    }
}
