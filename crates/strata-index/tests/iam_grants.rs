//! IAM permission-gap GRANTS extraction (Track D2, C1): a CFN `AWS::IAM::Role`'s
//! inline policy actions become `Grants` edges (role → `CloudAction`), while a
//! managed-policy ARN or a `Deny` statement becomes an `<opaque:…>` indeterminacy
//! marker — never a guessed concrete grant, so the role can never produce a false
//! permission gap (the never-confident-wrong rule applied to security).

use std::collections::BTreeMap;

use strata_core::{Direction, EdgeKind, Graph, NodeKind, Uid};
use strata_index::build_infra_plane;
use strata_infra::{CfnSamAdapter, IacAdapter, TerraformAdapter};

const REPO: &str = "iam-grants";

fn build(template_yaml: &str) -> Graph {
    let tmpl = CfnSamAdapter
        .extract("template.yaml", template_yaml)
        .expect("extract template");
    let mut g = Graph::new();
    build_infra_plane(&mut g, REPO, &[tmpl], &BTreeMap::new());
    g
}

fn role_uid(logical_id: &str) -> Uid {
    Uid::new("infra", REPO, "template.yaml", logical_id, "")
}

fn action_uid(action: &str) -> Uid {
    Uid::new("iam", REPO, "", action, "")
}

/// The action strings a role `Grants`, sorted.
fn granted_actions(g: &Graph, role: &Uid) -> Vec<String> {
    let mut v: Vec<String> = g
        .neighbors(role, Direction::Outgoing, &[EdgeKind::Grants])
        .into_iter()
        .filter_map(|(e, _)| g.get_node(&e.dst).map(|n| n.name.clone()))
        .collect();
    v.sort();
    v
}

const TEMPLATE: &str = r#"
Resources:
  ExecRole:
    Type: AWS::IAM::Role
    Properties:
      Policies:
        - PolicyName: app
          PolicyDocument:
            Statement:
              - Effect: Allow
                Action:
                  - dynamodb:PutItem
                  - dynamodb:GetItem
                Resource: "*"
      ManagedPolicyArns:
        - arn:aws:iam::aws:policy/AmazonS3ReadOnlyAccess
"#;

#[test]
fn role_inline_policy_actions_become_grants() {
    let g = build(TEMPLATE);
    let actions = granted_actions(&g, &role_uid("ExecRole"));
    assert!(
        actions.contains(&"dynamodb:PutItem".to_string()),
        "GRANTS dynamodb:PutItem expected, got {actions:?}"
    );
    assert!(actions.contains(&"dynamodb:GetItem".to_string()));
    // Each granted action is a CloudAction node.
    let put = g
        .get_node(&action_uid("dynamodb:PutItem"))
        .expect("CloudAction node for dynamodb:PutItem");
    assert_eq!(put.kind, NodeKind::CloudAction);
}

#[test]
fn managed_policy_arn_is_opaque_not_a_concrete_grant() {
    let g = build(TEMPLATE);
    let actions = granted_actions(&g, &role_uid("ExecRole"));
    // A managed-policy ARN cannot be enumerated → an `<opaque:…>` marker, never a
    // guessed concrete action (so the role is INDETERMINATE, never a false gap).
    assert!(
        actions
            .iter()
            .any(|a| a.starts_with("<opaque:managed-policy:")),
        "expected an opaque managed-policy marker, got {actions:?}"
    );
    assert!(
        !actions.iter().any(|a| a.starts_with("s3:")),
        "must NOT invent concrete s3 actions from a managed ARN, got {actions:?}"
    );
}

#[test]
fn deny_statement_makes_the_role_opaque() {
    let g = build(
        r#"
Resources:
  DenyRole:
    Type: AWS::IAM::Role
    Properties:
      Policies:
        - PolicyName: p
          PolicyDocument:
            Statement:
              - Effect: Deny
                Action: dynamodb:DeleteItem
                Resource: "*"
"#,
    );
    let actions = granted_actions(&g, &role_uid("DenyRole"));
    assert!(
        actions
            .iter()
            .any(|a| a.starts_with("<opaque:deny-statement")),
        "a Deny statement must mark the role opaque, got {actions:?}"
    );
    assert!(
        !actions.iter().any(|a| a == "dynamodb:DeleteItem"),
        "a Deny must NOT become a concrete grant, got {actions:?}"
    );
}

/// A SAM function whose `Policies` mix a known policy template, an unknown
/// template, and a managed-policy ARN (Track D2, C2). The function grants via its
/// implicit execution role, so the `Grants` edges hang off the LambdaFn node.
const SAM_FN: &str = r#"
Resources:
  CrudFn:
    Type: AWS::Serverless::Function
    Properties:
      Handler: app.handler
      Runtime: python3.12
      Policies:
        - DynamoDBCrudPolicy:
            TableName: orders
        - FancyCustomPolicy:
            Foo: bar
        - arn:aws:iam::aws:policy/AmazonS3ReadOnlyAccess
"#;

#[test]
fn sam_policy_template_expands_to_grants() {
    let g = build(SAM_FN);
    let actions = granted_actions(&g, &role_uid("CrudFn"));
    // DynamoDBCrudPolicy expands (verbatim from SAM's policy_templates.json).
    for expected in ["dynamodb:PutItem", "dynamodb:GetItem", "dynamodb:Query"] {
        assert!(
            actions.contains(&expected.to_string()),
            "SAM DynamoDBCrudPolicy should grant {expected}, got {actions:?}"
        );
    }
    // The expanded action is a CloudAction node.
    let put = g
        .get_node(&action_uid("dynamodb:PutItem"))
        .expect("CloudAction node for dynamodb:PutItem");
    assert_eq!(put.kind, NodeKind::CloudAction);
}

#[test]
fn unknown_sam_template_is_opaque_never_guessed() {
    let g = build(SAM_FN);
    let actions = granted_actions(&g, &role_uid("CrudFn"));
    assert!(
        actions
            .iter()
            .any(|a| a == "<opaque:sam-template:FancyCustomPolicy>"),
        "an unknown SAM policy template must be opaque, got {actions:?}"
    );
}

#[test]
fn sam_managed_policy_arn_is_opaque() {
    let g = build(SAM_FN);
    let actions = granted_actions(&g, &role_uid("CrudFn"));
    assert!(
        actions
            .iter()
            .any(|a| a.starts_with("<opaque:managed-policy:")),
        "a managed-policy ARN in SAM Policies must be opaque, got {actions:?}"
    );
}

// ── Terraform IAM grants (Track D2, C3) ─────────────────────────────────────

fn build_tf(tf: &str) -> Graph {
    let tmpl = TerraformAdapter.extract("main.tf", tf).expect("extract tf");
    let mut g = Graph::new();
    build_infra_plane(&mut g, REPO, &[tmpl], &BTreeMap::new());
    g
}

fn tf_uid(logical_id: &str) -> Uid {
    Uid::new("infra", REPO, "main.tf", logical_id, "")
}

const TF_IAM: &str = r#"
resource "aws_iam_role" "exec" {
  name                = "exec"
  managed_policy_arns = ["arn:aws:iam::aws:policy/AmazonS3ReadOnlyAccess"]
}

resource "aws_iam_role_policy" "inline" {
  role   = aws_iam_role.exec.id
  policy = jsonencode({
    Statement = [{
      Effect   = "Allow"
      Action   = ["dynamodb:PutItem", "dynamodb:GetItem"]
      Resource = aws_dynamodb_table.t.arn
    }]
  })
}
"#;

#[test]
fn tf_inline_role_policy_grants_role_ignoring_dynamic_resource() {
    let g = build_tf(TF_IAM);
    let actions = granted_actions(&g, &tf_uid("aws_iam_role.exec"));
    // jsonencode actions are recovered and attached to the role; the dynamic
    // `Resource = aws_dynamodb_table.t.arn` is ignored, not an error.
    assert!(
        actions.contains(&"dynamodb:PutItem".to_string()),
        "got {actions:?}"
    );
    assert!(
        actions.contains(&"dynamodb:GetItem".to_string()),
        "got {actions:?}"
    );
}

#[test]
fn tf_managed_policy_arns_are_opaque() {
    let g = build_tf(TF_IAM);
    let actions = granted_actions(&g, &tf_uid("aws_iam_role.exec"));
    assert!(
        actions
            .iter()
            .any(|a| a.starts_with("<opaque:managed-policy:")),
        "managed_policy_arns must be opaque, got {actions:?}"
    );
}

const TF_ATTACH: &str = r#"
resource "aws_iam_role" "r2" {
  name = "r2"
}

resource "aws_iam_policy" "p" {
  name   = "p"
  policy = jsonencode({
    Statement = [{ Effect = "Allow", Action = "s3:GetObject", Resource = "*" }]
  })
}

resource "aws_iam_role_policy_attachment" "a" {
  role       = aws_iam_role.r2.name
  policy_arn = aws_iam_policy.p.arn
}
"#;

#[test]
fn tf_policy_attachment_makes_role_opaque_never_invented() {
    let g = build_tf(TF_ATTACH);
    let actions = granted_actions(&g, &tf_uid("aws_iam_role.r2"));
    // The attachment marks the role INDETERMINATE; v1 does not resolve the
    // attached policy's actions, so s3:GetObject is never claimed for this role.
    assert!(
        actions
            .iter()
            .any(|a| a.starts_with("<opaque:policy-attachment")),
        "an attachment must mark the role opaque, got {actions:?}"
    );
    assert!(
        !actions.iter().any(|a| a == "s3:GetObject"),
        "must NOT invent the attached policy's actions on the role, got {actions:?}"
    );
}

// ── Terraform plan-JSON IAM grants (Track D2, C3) ───────────────────────────
// A resolved `terraform show -json` is the PREFERRED source (it renders policies
// raw HCL cannot). These prove the resolved grants flow end-to-end through
// `build_infra_plane` — including the standalone-policy → role redirect — so a
// committed plan does not silently lose the grants the HCL parse would have found.

/// A resolved plan: a role with a managed ARN, and a standalone
/// `aws_iam_role_policy` (resolved `policy` string) targeting that role.
const TF_PLAN_IAM: &str = r#"{
  "format_version": "1.0",
  "terraform_version": "1.6.0",
  "planned_values": {
    "root_module": {
      "resources": [
        { "address": "aws_iam_role.exec", "type": "aws_iam_role", "name": "exec",
          "values": { "name": "exec", "managed_policy_arns": ["arn:aws:iam::aws:policy/AmazonS3ReadOnlyAccess"] } },
        { "address": "aws_iam_role_policy.inline", "type": "aws_iam_role_policy", "name": "inline",
          "values": { "policy": "{\"Statement\":[{\"Effect\":\"Allow\",\"Action\":[\"dynamodb:PutItem\"],\"Resource\":\"*\"}]}" } }
      ]
    }
  },
  "configuration": {
    "root_module": {
      "resources": [
        { "address": "aws_iam_role_policy.inline", "type": "aws_iam_role_policy",
          "expressions": { "role": { "references": ["aws_iam_role.exec.id", "aws_iam_role.exec"] } } }
      ]
    }
  }
}"#;

#[test]
fn tf_plan_json_resolved_policy_grants_reach_the_role_end_to_end() {
    let tmpl = strata_infra::extract_plan("plan.json", TF_PLAN_IAM).expect("extract plan");
    let mut g = Graph::new();
    build_infra_plane(&mut g, REPO, &[tmpl], &BTreeMap::new());
    let role = Uid::new("infra", REPO, "plan.json", "aws_iam_role.exec", "");
    let actions = granted_actions(&g, &role);
    // The standalone policy's RESOLVED action is attached to the role it targets —
    // the "resolved over raw" grant source, end-to-end through build_infra_plane.
    assert!(
        actions.contains(&"dynamodb:PutItem".to_string()),
        "plan-resolved policy must grant its action to the targeted role, got {actions:?}"
    );
    // The role's managed ARN stays opaque, never enumerated into concrete actions.
    assert!(
        actions
            .iter()
            .any(|a| a.starts_with("<opaque:managed-policy:")),
        "managed arn opaque on the role, got {actions:?}"
    );
}

// ── Review fixes: never silently drop indeterminacy (Track D2, C1–C3 review) ──
// An independent review found several paths that silently skipped a grant source
// instead of marking it opaque, plus a standalone policy whose role we cannot
// resolve dropping its grants with no trace, plus duplicate Grants edges. Each
// would let a role look grant-complete (or its grant set look bigger) when it is
// not — exactly the never-confident-wrong failure. These pin the fixes.

#[test]
fn cfn_role_non_array_managed_policy_arns_is_opaque_not_dropped() {
    // `ManagedPolicyArns: { Ref: Param }` (a List<String> parameter) is not a
    // literal array; it must mark the role opaque, never look like "no managed
    // policies" (which would under-report the role's grants).
    let g = build(
        r#"
Resources:
  R:
    Type: AWS::IAM::Role
    Properties:
      ManagedPolicyArns:
        Ref: ArnListParam
"#,
    );
    let actions = granted_actions(&g, &role_uid("R"));
    assert!(
        actions
            .iter()
            .any(|a| a.starts_with("<opaque:managed-policy")),
        "a non-array ManagedPolicyArns must be opaque, got {actions:?}"
    );
}

#[test]
fn cfn_role_policies_entry_without_document_is_opaque_not_dropped() {
    // A `Policies` entry lacking a parseable `PolicyDocument` must be opaque, not
    // silently skipped.
    let g = build(
        r#"
Resources:
  R:
    Type: AWS::IAM::Role
    Properties:
      Policies:
        - PolicyName: p
"#,
    );
    let actions = granted_actions(&g, &role_uid("R"));
    assert!(
        actions.iter().any(|a| a.starts_with("<opaque:")),
        "a Policies entry without a PolicyDocument must be opaque, got {actions:?}"
    );
}

#[test]
fn tf_role_non_array_managed_policy_arns_is_opaque_not_dropped() {
    // `managed_policy_arns = var.arns` is not a literal array; the role must be
    // marked opaque rather than appear to have no managed policies.
    let g = build_tf(
        r#"
resource "aws_iam_role" "r" {
  name                = "r"
  managed_policy_arns = var.arns
}
"#,
    );
    let actions = granted_actions(&g, &tf_uid("aws_iam_role.r"));
    assert!(
        actions
            .iter()
            .any(|a| a.starts_with("<opaque:managed-policy")),
        "a dynamic managed_policy_arns must be opaque, got {actions:?}"
    );
}

#[test]
fn standalone_policy_with_unresolved_role_is_tallied_not_silently_dropped() {
    // `aws_iam_role_policy.role = var.role_name`: the grant target is unknown, so
    // the parsed actions attach to no role node. They must be TALLIED (so coverage
    // is honest and the gap traversal can stay conservative), never silently lost.
    let tmpl = TerraformAdapter
        .extract(
            "main.tf",
            r#"
resource "aws_iam_role_policy" "p" {
  role   = var.role_name
  policy = jsonencode({ Statement = [{ Effect = "Allow", Action = "dynamodb:PutItem", Resource = "*" }] })
}
"#,
        )
        .expect("extract tf");
    let mut g = Graph::new();
    let cov = build_infra_plane(&mut g, REPO, &[tmpl], &BTreeMap::new());
    assert_eq!(
        cov.iam_policy_grants_unattributed, 1,
        "a policy granting to an unresolved role must be tallied, not dropped"
    );
}

#[test]
fn duplicate_actions_grant_a_single_edge() {
    // Two inline policy blocks granting the same action must yield ONE Grants edge,
    // not two — a role's grant set is a set.
    let g = build(
        r#"
Resources:
  R:
    Type: AWS::IAM::Role
    Properties:
      Policies:
        - PolicyName: a
          PolicyDocument:
            Statement:
              - Effect: Allow
                Action: dynamodb:PutItem
                Resource: "*"
        - PolicyName: b
          PolicyDocument:
            Statement:
              - Effect: Allow
                Action: dynamodb:PutItem
                Resource: "*"
"#,
    );
    let puts = granted_actions(&g, &role_uid("R"))
        .into_iter()
        .filter(|a| a == "dynamodb:PutItem")
        .count();
    assert_eq!(
        puts, 1,
        "a duplicated action must produce a single Grants edge"
    );
}
