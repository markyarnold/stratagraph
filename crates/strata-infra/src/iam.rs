//! IAM policy-statement parsing for the permission-gap plane (Track D2, §6.4).
//!
//! Pure: turns a parsed IAM `PolicyDocument` (a `serde_json::Value`) into the set of
//! **Allow**-ed actions/wildcards it grants, plus honest **opaque** reasons for
//! anything it cannot enumerate — a `Deny` statement, a `NotAction`, a non-literal
//! (CFN intrinsic) action, or a malformed document. A role with any opaque reason is
//! treated as INDETERMINATE by the gap traversal: it never reports a confident gap
//! for grants it could not fully read. This is the never-confident-wrong rule
//! applied to security (a false "gap" is a false alarm; a false "covered" is false
//! security — so when unsure, we say so).

use serde_json::Value;

/// The grants parsed from one or more IAM policy documents: the concrete/wildcard
/// actions explicitly `Allow`-ed, and the reasons (if any) the grant set is
/// incomplete.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PolicyGrants {
    /// `Allow`-ed actions and wildcard patterns (`dynamodb:PutItem`, `dynamodb:*`,
    /// `*`), verbatim as written.
    pub allow_actions: Vec<String>,
    /// Reasons the grant set could not be fully enumerated. This parser emits
    /// `"deny-statement"`, `"not-action"`, `"dynamic-action"`, `"missing-action"`,
    /// `"unknown-effect"`, and `"malformed-policy"`; the IaC-format callers add
    /// source-level markers — `"managed-policy:<arn>"` / `"managed-policy:dynamic"`,
    /// `"sam-template:<name>"`, `"dynamic-policy"` / `"dynamic-policies"`,
    /// `"missing-policy-document"`, `"policy-attachment[:<arn>]"`, and
    /// `"unknown-at-plan-time"`. Non-empty ⇒ the owning role is INDETERMINATE.
    pub opaque_reasons: Vec<String>,
}

impl PolicyGrants {
    /// Merge another grant set into this one.
    pub fn merge(&mut self, other: PolicyGrants) {
        self.allow_actions.extend(other.allow_actions);
        self.opaque_reasons.extend(other.opaque_reasons);
    }

    /// Record an opaque reason (a grant source that cannot be enumerated).
    pub fn mark_opaque(&mut self, reason: impl Into<String>) {
        self.opaque_reasons.push(reason.into());
    }
}

/// Parse an IAM `PolicyDocument` value into [`PolicyGrants`]. A document whose
/// `Statement` is absent or not an object/array is `malformed-policy` (opaque): we
/// never guess what a non-literal document grants.
pub fn parse_policy_document(doc: &Value) -> PolicyGrants {
    let mut grants = PolicyGrants::default();
    let statements: Vec<&Value> = match doc.get("Statement") {
        Some(Value::Array(arr)) => arr.iter().collect(),
        Some(obj @ Value::Object(_)) => vec![obj],
        _ => {
            grants.mark_opaque("malformed-policy");
            return grants;
        }
    };
    for stmt in statements {
        parse_statement(stmt, &mut grants);
    }
    grants
}

/// Parse one statement, appending its `Allow`-ed actions or an opaque reason.
fn parse_statement(stmt: &Value, grants: &mut PolicyGrants) {
    match stmt.get("Effect").and_then(Value::as_str) {
        Some("Allow") => {}
        Some("Deny") => {
            // A `Deny` overrides `Allow`; v1 does not model deny precedence, so any
            // deny makes the role indeterminate (never a false "covered" / "gap").
            grants.mark_opaque("deny-statement");
            return;
        }
        _ => {
            grants.mark_opaque("unknown-effect");
            return;
        }
    }
    // A `NotAction` Allow grants the *complement* of the listed actions — unbounded
    // and not enumerable, so it is opaque (never guessed).
    if stmt.get("NotAction").is_some() {
        grants.mark_opaque("not-action");
        return;
    }
    match stmt.get("Action") {
        Some(Value::String(a)) => grants.allow_actions.push(a.clone()),
        Some(Value::Array(arr)) => {
            for a in arr {
                match a.as_str() {
                    Some(s) => grants.allow_actions.push(s.to_string()),
                    // A non-string element (a CFN intrinsic) is not literal.
                    None => grants.mark_opaque("dynamic-action"),
                }
            }
        }
        // A non-literal `Action` (a CFN `Ref`/`Sub` object).
        Some(_) => grants.mark_opaque("dynamic-action"),
        None => grants.mark_opaque("missing-action"),
    }
}

/// The granted IAM actions for a known SAM **policy template** name, or `None`.
///
/// SAM (`AWS::Serverless::Function` and friends) lets `Policies` name a *policy
/// template* (e.g. `DynamoDBCrudPolicy`) that the SAM transform expands into a
/// fixed set of IAM actions. This is a **curated subset** of the common
/// templates, with action lists taken verbatim from AWS SAM's
/// `policy_templates.json`. A template **not** in this map returns `None` and is
/// recorded opaque (`sam-template:<name>`) by the caller — never guessed, so an
/// unknown template can never produce a false "covered" (never-confident-wrong).
pub fn sam_template_actions(name: &str) -> Option<&'static [&'static str]> {
    let actions: &'static [&'static str] = match name {
        "DynamoDBCrudPolicy" => &[
            "dynamodb:GetItem",
            "dynamodb:DeleteItem",
            "dynamodb:PutItem",
            "dynamodb:Scan",
            "dynamodb:Query",
            "dynamodb:UpdateItem",
            "dynamodb:BatchWriteItem",
            "dynamodb:BatchGetItem",
            "dynamodb:DescribeTable",
            "dynamodb:ConditionCheckItem",
        ],
        "DynamoDBReadPolicy" => &[
            "dynamodb:GetItem",
            "dynamodb:Scan",
            "dynamodb:Query",
            "dynamodb:BatchGetItem",
            "dynamodb:DescribeTable",
        ],
        "DynamoDBWritePolicy" => &[
            "dynamodb:PutItem",
            "dynamodb:UpdateItem",
            "dynamodb:BatchWriteItem",
        ],
        "S3ReadPolicy" => &[
            "s3:GetObject",
            "s3:ListBucket",
            "s3:GetBucketLocation",
            "s3:GetObjectVersion",
            "s3:GetLifecycleConfiguration",
        ],
        "S3CrudPolicy" => &[
            "s3:GetObject",
            "s3:ListBucket",
            "s3:GetBucketLocation",
            "s3:GetObjectVersion",
            "s3:PutObject",
            "s3:PutObjectAcl",
            "s3:GetLifecycleConfiguration",
            "s3:PutLifecycleConfiguration",
            "s3:DeleteObject",
        ],
        "SQSSendMessagePolicy" => &["sqs:SendMessage*"],
        "SQSPollerPolicy" => &[
            "sqs:ChangeMessageVisibility",
            "sqs:ChangeMessageVisibilityBatch",
            "sqs:DeleteMessage",
            "sqs:DeleteMessageBatch",
            "sqs:GetQueueAttributes",
            "sqs:ReceiveMessage",
        ],
        "SNSPublishMessagePolicy" => &["sns:Publish"],
        "LambdaInvokePolicy" => &["lambda:InvokeFunction"],
        "KMSDecryptPolicy" => &["kms:Decrypt"],
        "SSMParameterReadPolicy" => &[
            "ssm:DescribeParameters",
            "ssm:GetParameters",
            "ssm:GetParameter",
            "ssm:GetParametersByPath",
        ],
        "AWSSecretsManagerGetSecretValuePolicy" => &["secretsmanager:GetSecretValue"],
        _ => return None,
    };
    Some(actions)
}

/// Parse a SAM `Policies` value into [`PolicyGrants`].
///
/// SAM `Policies` is polymorphic: a string (a managed-policy name/ARN), a single
/// object (an inline policy document or a SAM policy template), or a list mixing
/// those. Each entry:
///
/// - a **string** → a managed policy we cannot enumerate → opaque
///   (`managed-policy:<s>`);
/// - an inline **policy document** (`{Statement: …}`, or `{PolicyName,
///   PolicyDocument}`) → parsed via [`parse_policy_document`];
/// - a single-key **SAM policy template** (`{DynamoDBCrudPolicy: {…}}`) → expanded
///   via [`sam_template_actions`] when known, else opaque (`sam-template:<name>`);
/// - anything else → opaque (`dynamic-policy`).
///
/// The parameter values of a template (e.g. `{TableName: …}`) are intentionally
/// ignored: the grant is action-level, and SAM templates do not vary their action
/// set by parameter.
pub fn parse_sam_policies(policies: &Value) -> PolicyGrants {
    let mut grants = PolicyGrants::default();
    let entries: Vec<&Value> = match policies {
        Value::Array(arr) => arr.iter().collect(),
        other => vec![other],
    };
    for entry in entries {
        match entry {
            Value::String(s) => grants.mark_opaque(format!("managed-policy:{s}")),
            Value::Object(map) => {
                if map.contains_key("Statement") {
                    grants.merge(parse_policy_document(entry));
                } else if let Some(doc) = map.get("PolicyDocument") {
                    grants.merge(parse_policy_document(doc));
                } else if map.len() == 1 {
                    let name = map.keys().next().expect("len == 1");
                    match sam_template_actions(name) {
                        Some(actions) => grants
                            .allow_actions
                            .extend(actions.iter().map(|a| a.to_string())),
                        None => grants.mark_opaque(format!("sam-template:{name}")),
                    }
                } else {
                    grants.mark_opaque("dynamic-policy");
                }
            }
            _ => grants.mark_opaque("dynamic-policy"),
        }
    }
    grants
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn allow_single_and_list_actions() {
        let g = parse_policy_document(&json!({
            "Statement": [
                { "Effect": "Allow", "Action": "dynamodb:PutItem", "Resource": "*" },
                { "Effect": "Allow", "Action": ["s3:GetObject", "s3:PutObject"] },
            ]
        }));
        assert_eq!(
            g.allow_actions,
            vec!["dynamodb:PutItem", "s3:GetObject", "s3:PutObject"]
        );
        assert!(g.opaque_reasons.is_empty());
    }

    #[test]
    fn single_statement_object_is_accepted() {
        let g = parse_policy_document(&json!({
            "Statement": { "Effect": "Allow", "Action": "sqs:SendMessage" }
        }));
        assert_eq!(g.allow_actions, vec!["sqs:SendMessage"]);
        assert!(g.opaque_reasons.is_empty());
    }

    #[test]
    fn wildcards_pass_through_verbatim() {
        let g = parse_policy_document(&json!({
            "Statement": [{ "Effect": "Allow", "Action": ["dynamodb:*", "*"] }]
        }));
        assert_eq!(g.allow_actions, vec!["dynamodb:*", "*"]);
    }

    #[test]
    fn deny_makes_it_opaque_and_grants_nothing() {
        let g = parse_policy_document(&json!({
            "Statement": [{ "Effect": "Deny", "Action": "dynamodb:DeleteItem" }]
        }));
        assert!(g.allow_actions.is_empty());
        assert_eq!(g.opaque_reasons, vec!["deny-statement"]);
    }

    #[test]
    fn not_action_is_opaque() {
        let g = parse_policy_document(&json!({
            "Statement": [{ "Effect": "Allow", "NotAction": "iam:*", "Resource": "*" }]
        }));
        assert!(g.allow_actions.is_empty());
        assert_eq!(g.opaque_reasons, vec!["not-action"]);
    }

    #[test]
    fn dynamic_action_is_opaque_never_guessed() {
        let g = parse_policy_document(&json!({
            "Statement": [{ "Effect": "Allow", "Action": { "Ref": "ActionParam" } }]
        }));
        assert!(g.allow_actions.is_empty());
        assert_eq!(g.opaque_reasons, vec!["dynamic-action"]);
    }

    #[test]
    fn missing_statement_is_malformed_opaque() {
        let g = parse_policy_document(&json!({ "Ref": "SomePolicy" }));
        assert!(g.allow_actions.is_empty());
        assert_eq!(g.opaque_reasons, vec!["malformed-policy"]);
    }

    #[test]
    fn sam_template_known_returns_actions_unknown_returns_none() {
        let crud = sam_template_actions("DynamoDBCrudPolicy").expect("known template");
        assert!(crud.contains(&"dynamodb:PutItem"));
        assert!(crud.contains(&"dynamodb:GetItem"));
        assert_eq!(crud.len(), 10);
        assert_eq!(
            sam_template_actions("SQSSendMessagePolicy"),
            Some(&["sqs:SendMessage*"][..])
        );
        assert!(sam_template_actions("TotallyMadeUpPolicy").is_none());
    }

    #[test]
    fn sam_policies_known_template_expands() {
        let g = parse_sam_policies(&json!([{ "DynamoDBReadPolicy": { "TableName": "t" } }]));
        assert_eq!(
            g.allow_actions,
            vec![
                "dynamodb:GetItem",
                "dynamodb:Scan",
                "dynamodb:Query",
                "dynamodb:BatchGetItem",
                "dynamodb:DescribeTable",
            ]
        );
        assert!(g.opaque_reasons.is_empty());
    }

    #[test]
    fn sam_policies_unknown_template_and_managed_arn_are_opaque() {
        let g = parse_sam_policies(&json!([
            "arn:aws:iam::aws:policy/AdministratorAccess",
            { "MysteryPolicy": { "X": 1 } },
        ]));
        assert!(g.allow_actions.is_empty());
        assert!(g
            .opaque_reasons
            .contains(&"managed-policy:arn:aws:iam::aws:policy/AdministratorAccess".to_string()));
        assert!(g
            .opaque_reasons
            .contains(&"sam-template:MysteryPolicy".to_string()));
    }

    #[test]
    fn sam_policies_inline_documents_grant_actions() {
        // Bare inline policy document.
        let bare = parse_sam_policies(&json!([
            { "Statement": [{ "Effect": "Allow", "Action": "s3:GetObject" }] }
        ]));
        assert_eq!(bare.allow_actions, vec!["s3:GetObject"]);
        // Named `{PolicyName, PolicyDocument}` inline policy.
        let named = parse_sam_policies(&json!([
            {
                "PolicyName": "p",
                "PolicyDocument": { "Statement": [{ "Effect": "Allow", "Action": "sqs:SendMessage" }] }
            }
        ]));
        assert_eq!(named.allow_actions, vec!["sqs:SendMessage"]);
    }
}
