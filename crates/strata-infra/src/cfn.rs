//! The AWS CloudFormation / SAM adapter.
//!
//! Templates arrive as YAML or JSON. JSON already carries intrinsics in
//! long-form maps (`{"Ref": …}`, `{"Fn::GetAtt": …}`). YAML may use CFN's
//! short-form tags (`!Ref X`, `!GetAtt A.B`, `!Sub`, `!Join`, …). `yaml-rust2`'s
//! *high-level* loader discards tags, but its lower-level event API
//! ([`yaml_rust2::parser::Parser`] driving a [`MarkedEventReceiver`]) carries
//! each tag on the event that opens its node. We therefore parse at the event
//! level and convert CFN tags to their long-form maps *during* construction (see
//! [`CfnEventLoader`]). After that, both encodings present one uniform shape,
//! walked by a single [`serde_json::Value`] traversal — mirroring
//! `strata-contract`'s adapter.
//!
//! Reference fields are graded by [`RefValue`] against the set of logical ids in
//! the template, so a `Resource(id)` is only ever produced for a `Ref`/`GetAtt`
//! whose target actually exists in the same template.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};
use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser, Tag};
use yaml_rust2::scanner::{Marker, TScalarStyle};

use crate::{
    CfnDetection, IacAdapter, InfraError, InfraKind, InfraResource, InfraTemplate, RefValue,
};

/// Adapter for AWS CloudFormation and SAM templates (JSON or YAML).
pub struct CfnSamAdapter;

/// The cheap textual signal that a file is *attempting* to be a CFN/SAM template:
/// both the `Resources` and `AWS::` substrings appear. It is a strong
/// discriminator (build specs, `samconfig.toml`, docker-compose, and arbitrary
/// JSON carry neither), so a file that has the signal but won't parse is treated
/// as a malformed template worth surfacing — not silently dropped as "not CFN".
fn has_cfn_textual_signal(content: &str) -> bool {
    content.contains("Resources") && content.contains("AWS::")
}

impl CfnSamAdapter {
    /// Classify `content` into a [`CfnDetection`], distinguishing a malformed
    /// template from a non-template so the indexer can surface the former.
    ///
    /// This is the richer companion to [`IacAdapter::detects`] (which collapses
    /// `Malformed` and `NotCfn` into a single `false`): a file with the CFN
    /// textual signal that fails to parse is [`CfnDetection::Malformed`] (report
    /// it), whereas a file lacking the signal — or one that parses but has no
    /// `Resources` map of `AWS::…` resources — is [`CfnDetection::NotCfn`] (skip
    /// it). A well-formed template is [`CfnDetection::Template`].
    pub fn detect_kind(&self, content: &str) -> CfnDetection {
        if !has_cfn_textual_signal(content) {
            return CfnDetection::NotCfn;
        }
        match parse_to_value(content) {
            Ok(value) if has_resource_shape(&value) => CfnDetection::Template,
            // Parsed, but it just mentions the words (no real Resources map) — not
            // a template, no failure to report.
            Ok(_) => CfnDetection::NotCfn,
            // The CFN signal is present but the document won't parse: a malformed
            // template. Surface the error rather than swallow it.
            Err(msg) => CfnDetection::Malformed(msg),
        }
    }
}

impl IacAdapter for CfnSamAdapter {
    /// Detect a CloudFormation / SAM template.
    ///
    /// A real template has a top-level `Resources` *map* whose entries carry a
    /// `Type: AWS::…` string. A cheap textual pre-check (both the substrings
    /// `Resources` and `AWS::` present) avoids the full parse for the common
    /// rejects (build specs, `samconfig.toml`, docker-compose, arbitrary
    /// YAML/JSON). The structural check then confirms the shape, so a file that
    /// merely mentions those words is still rejected.
    ///
    /// This is the boolean view of [`CfnSamAdapter::detect_kind`]: both a
    /// malformed template and a non-template collapse to `false` here. The indexer
    /// uses `detect_kind` instead so a malformed template is surfaced, not
    /// silently skipped.
    fn detects(&self, _filename: &str, content: &str) -> bool {
        matches!(self.detect_kind(content), CfnDetection::Template)
    }

    fn extract(&self, path: &str, content: &str) -> Result<InfraTemplate, InfraError> {
        let value = parse_to_value(content).map_err(|msg| InfraError::Parse {
            path: path.to_string(),
            msg,
        })?;

        // A template must carry a `Resources` map. Its absence (or a non-map
        // value) is a structural parse error rather than a silent empty result,
        // so a truncated / wrong-shaped document degrades visibly.
        let resources = value
            .get("Resources")
            .and_then(Value::as_object)
            .ok_or_else(|| InfraError::Parse {
                path: path.to_string(),
                msg: "missing or non-object `Resources`".to_string(),
            })?;

        // The id set drives reference grading: a Ref/GetAtt only resolves to a
        // `Resource` when its target is a logical id in this same template.
        let ids: BTreeSet<&str> = resources.keys().map(String::as_str).collect();

        let mut out = Vec::with_capacity(resources.len());
        for (logical_id, body) in resources {
            out.push(extract_resource(logical_id, body, &ids));
        }

        Ok(InfraTemplate {
            path: path.to_string(),
            resources: out,
        })
    }
}

/// `logical id → a lossless signature of the resource's raw sub-tree`, for every
/// resource in a CFN/SAM template, or `None` when `content` is not a template.
///
/// Unlike [`InfraTemplate`]/[`InfraResource`] (which capture only the typed
/// fields the graph wires — handler, role refs, the AppSync chain), this signs
/// the FULL parsed sub-tree of each `Resources.<id>`, so `detect_changes` sees a
/// modification to ANY property — a Lambda `Timeout`/`MemorySize`/`Environment`,
/// an SQS `QueueName`, a tag — not just the wired ones. The signature is the
/// resource value re-serialized through the one parser (`parse_to_value`), so
/// old↔new comparisons are stable: identical content yields identical strings,
/// and the comparison degrades to "no signature" identically on both sides for a
/// non-template. (`serde_json` is `preserve_order` workspace-wide, so the parse
/// is deterministic; a genuine source key-reorder reads as a change — the safe
/// direction for a change-detector that must never miss an edit.)
pub fn raw_resource_signatures(
    content: &str,
) -> Option<std::collections::BTreeMap<String, String>> {
    let value = parse_to_value(content).ok()?;
    let resources = value.get("Resources").and_then(Value::as_object)?;
    let mut map = std::collections::BTreeMap::new();
    for (logical_id, body) in resources {
        // `to_string` on the sub-value is total (no IO, no cycles in parsed
        // JSON), so the signature is always available for a present resource.
        map.insert(logical_id.clone(), body.to_string());
    }
    Some(map)
}

/// Whether `value` has the structural shape of a template: a `Resources` map
/// with at least one entry whose `Type` is an `AWS::…` string.
fn has_resource_shape(value: &Value) -> bool {
    let Some(resources) = value.get("Resources").and_then(Value::as_object) else {
        return false;
    };
    resources.values().any(|body| {
        body.get("Type")
            .and_then(Value::as_str)
            .map(|t| t.starts_with("AWS::"))
            .unwrap_or(false)
    })
}

/// Classify a CFN resource type into an [`InfraKind`]. The SAM serverless
/// function shorthand maps to the same kind as a plain Lambda — its
/// `Handler`/`CodeUri`/`Role` are read directly with NO transform expansion.
fn classify(cfn_type: &str) -> InfraKind {
    match cfn_type {
        "AWS::Lambda::Function" | "AWS::Serverless::Function" => InfraKind::LambdaFunction,
        "AWS::IAM::Role" => InfraKind::IamRole,
        "AWS::IAM::Policy" => InfraKind::IamPolicy,
        "AWS::AppSync::GraphQLApi" => InfraKind::AppSyncApi,
        "AWS::AppSync::Resolver" => InfraKind::AppSyncResolver,
        "AWS::AppSync::DataSource" => InfraKind::AppSyncDataSource,
        _ => InfraKind::Generic,
    }
}

/// Aggregate a CFN `AWS::IAM::Role`'s inline `Policies` + `ManagedPolicyArns` into
/// [`PolicyGrants`](crate::iam::PolicyGrants). Each inline policy's `PolicyDocument`
/// is parsed; each managed-policy ARN is recorded as opaque — its action set is not
/// enumerable from the template, so the role becomes indeterminate (never a false
/// gap).
fn parse_role_inline_grants(props: &serde_json::Map<String, Value>) -> crate::iam::PolicyGrants {
    let mut grants = crate::iam::PolicyGrants::default();
    match props.get("Policies") {
        Some(Value::Array(policies)) => {
            for p in policies {
                match p.get("PolicyDocument") {
                    Some(doc) => grants.merge(crate::iam::parse_policy_document(doc)),
                    // A `Policies` entry with no parseable `PolicyDocument` (an
                    // intrinsic/`Fn::If`, a malformed entry): opaque, never skipped.
                    None => grants.mark_opaque("missing-policy-document"),
                }
            }
        }
        // `Policies` present but not a literal array (a `Ref`/intrinsic): opaque, so
        // the role is indeterminate rather than appearing to have no inline policies.
        Some(_) => grants.mark_opaque("dynamic-policies"),
        None => {}
    }
    match props.get("ManagedPolicyArns") {
        Some(Value::Array(arns)) => {
            for arn in arns {
                match arn.as_str() {
                    Some(s) => grants.mark_opaque(format!("managed-policy:{s}")),
                    None => grants.mark_opaque("managed-policy:dynamic"),
                }
            }
        }
        // `ManagedPolicyArns` present but not a literal array (a `Ref` to a
        // `List<String>` param): opaque, never treated as "no managed policies".
        Some(_) => grants.mark_opaque("managed-policy:dynamic"),
        None => {}
    }
    grants
}

/// Extract one resource. The vertical fields are filled only for the matching
/// kind; a `Generic` resource carries just its identity.
fn extract_resource(logical_id: &str, body: &Value, ids: &BTreeSet<&str>) -> InfraResource {
    let cfn_type = body
        .get("Type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let kind = classify(&cfn_type);
    let mut res = InfraResource::new(logical_id.to_string(), cfn_type, kind);

    // `Properties` is where every vertical field lives. A missing/non-object
    // Properties simply leaves all fields `None`.
    let props = body.get("Properties").and_then(Value::as_object);
    let Some(props) = props else {
        return res;
    };

    match kind {
        InfraKind::LambdaFunction => {
            res.handler = literal(props.get("Handler"));
            res.code_uri = literal(props.get("CodeUri")).or_else(|| {
                // Plain Lambda uses `Code: { S3Key: … }`; surface a literal S3Key
                // as the code path when present (best-effort, optional field).
                props
                    .get("Code")
                    .and_then(Value::as_object)
                    .and_then(|c| literal(c.get("S3Key")))
            });
            // SAM (`AWS::Serverless::Function`) `Policies` grant actions via the
            // function's implicit execution role: managed ARNs, inline policy
            // documents, and SAM policy templates (e.g. `DynamoDBCrudPolicy`).
            // Known templates expand to their actions; unknown templates / managed
            // ARNs are opaque, never guessed (Track D2, C2). Plain `AWS::Lambda::
            // Function` has no `Policies`, so this is a no-op there.
            if let Some(policies) = props.get("Policies") {
                let grants = crate::iam::parse_sam_policies(policies);
                res.granted_actions = grants.allow_actions;
                res.grants_opaque = grants.opaque_reasons;
            }
        }
        InfraKind::AppSyncResolver => {
            res.type_name = literal(props.get("TypeName"));
            res.field_name = literal(props.get("FieldName"));
            res.data_source_ref = props.get("DataSourceName").map(|v| grade(v, ids));
            res.api_ref = props.get("ApiId").map(|v| grade(v, ids));
        }
        InfraKind::AppSyncDataSource => {
            res.api_ref = props.get("ApiId").map(|v| grade(v, ids));
            // LambdaConfig.LambdaFunctionArn — the data source -> function edge.
            res.lambda_ref = props
                .get("LambdaConfig")
                .and_then(Value::as_object)
                .and_then(|lc| lc.get("LambdaFunctionArn"))
                .map(|v| grade(v, ids));
        }
        InfraKind::IamRole => {
            // Inline `Policies` grant actions directly to this role; managed-policy
            // ARNs are grants we cannot enumerate (opaque → indeterminate).
            let grants = parse_role_inline_grants(props);
            res.granted_actions = grants.allow_actions;
            res.grants_opaque = grants.opaque_reasons;
        }
        InfraKind::IamPolicy => {
            // A standalone policy resource: its `PolicyDocument` statements grant to
            // the roles in `Roles` (collected as role_refs, attached at build time).
            let mut grants = props
                .get("PolicyDocument")
                .map(crate::iam::parse_policy_document)
                .unwrap_or_default();
            if props.get("PolicyDocument").is_none() {
                grants.mark_opaque("missing-policy-document");
            }
            res.granted_actions = grants.allow_actions;
            res.grants_opaque = grants.opaque_reasons;
            if let Some(Value::Array(roles)) = props.get("Roles") {
                for r in roles {
                    res.role_refs.push(grade(r, ids));
                }
            }
        }
        // The AppSync API and Generic resources carry no kind-specific reference
        // fields; Generic is inventory (plus the role scan below).
        InfraKind::AppSyncApi | InfraKind::Generic => {}
    }

    // Role references are NOT kind-specific: AWS spreads them across many
    // property names (`Role` on functions, `ServiceRoleArn` on AppSync data
    // sources, `RoleArn` on Step Functions / EventBridge, execution/task roles
    // on ECS, and AppSync's nested `LogConfig.CloudWatchLogsRoleArn`). Scan
    // every resource for all of them, graded individually. Dogfood regression:
    // only `Role` was scanned, so most role references (and every `assumed_by`
    // answer) were missing.
    for prop in ROLE_PROPS {
        if let Some(v) = props.get(prop) {
            res.role_refs.push(grade(v, ids));
        }
    }
    if let Some(v) = props
        .get("LogConfig")
        .and_then(Value::as_object)
        .and_then(|lc| lc.get("CloudWatchLogsRoleArn"))
    {
        res.role_refs.push(grade(v, ids));
    }

    res
}

/// The top-level property names that reference an IAM role across CFN resource
/// types. (AppSync's `LogConfig.CloudWatchLogsRoleArn` is nested and handled
/// separately.)
const ROLE_PROPS: [&str; 5] = [
    "Role",
    "RoleArn",
    "ServiceRoleArn",
    "ExecutionRoleArn",
    "TaskRoleArn",
];

/// A literal string property, or `None` if the value is absent or not a plain
/// string (e.g. it is an intrinsic map — that is not a literal).
fn literal(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).map(str::to_string)
}

/// Grade a reference value with provenance per [`RefValue`].
///
/// - A plain string → [`RefValue::Literal`].
/// - `{"Ref": id}` where `id` is a same-template resource → [`Resource`].
/// - `{"Fn::GetAtt": [id, …]}` (or `"id.attr"`) where `id` is a same-template
///   resource → [`Resource`] (the attribute is dropped).
/// - `{"Ref"/"Fn::GetAtt": …}` to a non-resource (a parameter, etc.) →
///   [`Unresolved`].
/// - `{"Fn::Sub"/"Fn::Join": …}` → recover an embedded `${LogicalId}` /
///   `${LogicalId.Attr}` that is a same-template resource → [`Inferred`(id)];
///   otherwise [`Unresolved`].
/// - `{"Fn::ImportValue": …}` and any other intrinsic → [`Unresolved`].
///
/// [`Resource`]: RefValue::Resource
/// [`Unresolved`]: RefValue::Unresolved
/// [`Inferred`]: RefValue::Inferred
fn grade(v: &Value, ids: &BTreeSet<&str>) -> RefValue {
    if let Some(s) = v.as_str() {
        return RefValue::Literal(s.to_string());
    }
    let Some(obj) = v.as_object() else {
        // Arrays / numbers / bools in a reference position are not something we
        // can tie to a resource; surface honestly.
        return RefValue::Unresolved;
    };

    if let Some(ref_val) = obj.get("Ref").and_then(Value::as_str) {
        return resource_or_unresolved(ref_val, ids);
    }

    if let Some(getatt) = obj.get("Fn::GetAtt") {
        if let Some(id) = getatt_logical_id(getatt) {
            return resource_or_unresolved(id, ids);
        }
        return RefValue::Unresolved;
    }

    if let Some(sub) = obj.get("Fn::Sub") {
        return grade_sub(sub, ids);
    }

    if let Some(join) = obj.get("Fn::Join") {
        return grade_join(join, ids);
    }

    if let Some(if_arm) = obj.get("Fn::If") {
        return grade_if(if_arm, ids);
    }

    // ImportValue, FindInMap, Select, and any other intrinsic: not a
    // same-template resource we can name. Honest Unresolved.
    RefValue::Unresolved
}

/// Grade a `Fn::If`. CFN: `[condition, then, else]`. Collect the same-template
/// resource ids each branch (`then`/`else`) could reach via a `Ref`/`GetAtt`
/// (recursively, so a nested `!If` or `!Sub`/`!Join` still contributes), deduped
/// in first-seen order:
/// - 0 ids → [`Unresolved`] (a condition between two parameters / pseudo-params /
///   imports — never a Resource invented by reaching through the `!If`).
/// - 1 id → [`Inferred`(id)] (band 0.70 downstream).
/// - ≥2 distinct ids → [`InferredMulti`(ids)] — both possible deployments
///   surfaced, recall-biased; the builder emits an edge per id.
///
/// An `Fn::If` reference is deliberately NEVER [`Resource`] (a fact): we cannot
/// pin which branch deploys without evaluating the condition (Slice 10, B2).
///
/// [`Resource`]: RefValue::Resource
/// [`Unresolved`]: RefValue::Unresolved
/// [`Inferred`]: RefValue::Inferred
/// [`InferredMulti`]: RefValue::InferredMulti
fn grade_if(if_arm: &Value, ids: &BTreeSet<&str>) -> RefValue {
    let Some(arr) = if_arm.as_array() else {
        return RefValue::Unresolved;
    };
    // Element 0 is the condition name (not a value); the branches are 1.. .
    let mut found: Vec<String> = Vec::new();
    for branch in arr.iter().skip(1) {
        collect_branch_ids(branch, ids, &mut found);
    }
    match found.len() {
        0 => RefValue::Unresolved,
        1 => RefValue::Inferred(found.into_iter().next().unwrap()),
        _ => RefValue::InferredMulti(found),
    }
}

/// Recursively collect the same-template resource ids a `Fn::If` branch could
/// reach, appending each new id (deduped, first-seen order) to `out`. A branch is
/// graded with the existing [`grade`]: a `Resource(id)` / `Inferred(id)`
/// contributes its id; an `InferredMulti` (a nested `!If`) contributes each id; a
/// `Literal`/`Unresolved` contributes nothing (never invented).
fn collect_branch_ids(branch: &Value, ids: &BTreeSet<&str>, out: &mut Vec<String>) {
    let mut push = |id: String| {
        if !out.contains(&id) {
            out.push(id);
        }
    };
    match grade(branch, ids) {
        RefValue::Resource(id) | RefValue::Inferred(id) => push(id),
        RefValue::InferredMulti(more) => {
            for id in more {
                push(id);
            }
        }
        RefValue::Literal(_) | RefValue::Unresolved => {}
    }
}

/// Resolve a logical id to [`RefValue::Resource`] iff it is a template resource;
/// otherwise (a parameter, pseudo-parameter, etc.) [`RefValue::Unresolved`].
fn resource_or_unresolved(id: &str, ids: &BTreeSet<&str>) -> RefValue {
    if ids.contains(id) {
        RefValue::Resource(id.to_string())
    } else {
        RefValue::Unresolved
    }
}

/// The logical id targeted by a `Fn::GetAtt` value. CFN allows two shapes:
/// `["LogicalId", "Attr", …]` (list — both JSON and the `!GetAtt` event loader's
/// split of a dotted scalar produce this) or a bare `"LogicalId.Attr"` scalar.
/// The id is the first element (list) or the substring before the first `.`
/// (scalar).
fn getatt_logical_id(getatt: &Value) -> Option<&str> {
    if let Some(arr) = getatt.as_array() {
        return arr.first().and_then(Value::as_str);
    }
    if let Some(s) = getatt.as_str() {
        return Some(s.split('.').next().unwrap_or(s));
    }
    None
}

/// Grade a `Fn::Sub`. The substitution string is either a bare string or
/// `[string, {vars}]`; we read the string and recover the first `${…}` whose
/// head is a same-template resource → [`Inferred`]. None found → [`Unresolved`].
fn grade_sub(sub: &Value, ids: &BTreeSet<&str>) -> RefValue {
    let template = match sub {
        Value::String(s) => Some(s.as_str()),
        Value::Array(items) => items.first().and_then(Value::as_str),
        _ => None,
    };
    match template.and_then(|t| recover_sub_ref(t, ids)) {
        Some(id) => RefValue::Inferred(id),
        None => RefValue::Unresolved,
    }
}

/// Grade a `Fn::Join`. CFN: `[delimiter, [parts…]]`. We scan the parts for the
/// first `{"Ref"/"Fn::GetAtt"}` to a same-template resource → [`Inferred`(id)]
/// (best-effort: a join is composite, so this is an interpolation hint, not a
/// crisp `Resource`). None found → [`Unresolved`].
fn grade_join(join: &Value, ids: &BTreeSet<&str>) -> RefValue {
    let Some(arr) = join.as_array() else {
        return RefValue::Unresolved;
    };
    let Some(parts) = arr.get(1).and_then(Value::as_array) else {
        return RefValue::Unresolved;
    };
    for part in parts {
        if let RefValue::Resource(id) = grade(part, ids) {
            return RefValue::Inferred(id);
        }
    }
    RefValue::Unresolved
}

/// Recover the first `${Head…}` interpolation in a `Sub` string whose head id is
/// a same-template resource, returning that id. `${Head.Attr}` keeps `Head`;
/// `${!Literal}` (an escaped, non-interpolating `${}`) is skipped.
fn recover_sub_ref(s: &str, ids: &BTreeSet<&str>) -> Option<String> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'$' && bytes[i + 1] == b'{' {
            let inner_start = i + 2;
            if let Some(rel_end) = s[inner_start..].find('}') {
                let inner = &s[inner_start..inner_start + rel_end];
                // `${!Foo}` is a literal `${Foo}`, not an interpolation.
                if !inner.starts_with('!') {
                    let head = inner.split('.').next().unwrap_or(inner).trim();
                    if ids.contains(head) {
                        return Some(head.to_string());
                    }
                }
                i = inner_start + rel_end + 1;
                continue;
            }
        }
        i += 1;
    }
    None
}

// ── Parsing: text → serde_json::Value (one downstream walker) ───────────────

/// Parse template text into a generic [`serde_json::Value`]. JSON is tried first
/// (unambiguous and already long-form); otherwise the text is parsed as YAML at
/// the **event level** so CFN short-form tags are converted to long-form maps
/// during construction (see [`CfnEventLoader`]). Both encodings then present the
/// same `{"Ref": …}` / `{"Fn::GetAtt": …}` shape to the single downstream walker.
fn parse_to_value(content: &str) -> Result<Value, String> {
    if let Ok(value) = serde_json::from_str::<Value>(content) {
        return Ok(value);
    }
    parse_yaml_to_value(content)
}

/// Parse a YAML template into a [`serde_json::Value`] via `yaml-rust2`'s event
/// API, converting CFN short-form tags as nodes are built. A scanner error
/// (malformed YAML) is surfaced as `Err` so the caller degrades visibly.
fn parse_yaml_to_value(content: &str) -> Result<Value, String> {
    let mut parser = Parser::new_from_str(content);
    let mut loader = CfnEventLoader::default();
    parser
        .load(&mut loader, false)
        .map_err(|e| format!("invalid YAML: {e}"))?;
    if let Some(err) = loader.error {
        return Err(err);
    }
    loader.root.ok_or_else(|| "empty YAML document".to_string())
}

// ── Event-driven loader: yaml-rust2 events → serde_json::Value ──────────────

/// A partially-built container on the loader stack: either a sequence
/// accumulating its items, or a mapping accumulating key/value pairs. Each frame
/// remembers the CFN [`Tag`] (if any) that opened it and its anchor id, so the
/// tag conversion and anchor binding are applied when the container closes.
enum Frame {
    /// A sequence and the items gathered so far.
    Seq {
        tag: Option<Tag>,
        anchor: usize,
        items: Vec<Value>,
    },
    /// A mapping: its built entries, plus a pending key awaiting its value
    /// (mappings alternate key, value, key, value …).
    Map {
        tag: Option<Tag>,
        anchor: usize,
        entries: Map<String, Value>,
        pending_key: Option<String>,
    },
}

/// A [`MarkedEventReceiver`] that builds a [`serde_json::Value`] from the YAML
/// event stream, converting CFN short-form tags to long-form maps as each node
/// is completed (see [`apply_cfn_tag`]).
///
/// ## Tag conversion (honesty-preserving)
///
/// A tag whose handle is `"!"` is a CFN application tag. Its suffix selects the
/// long-form key — `Ref`/`Condition` keep their bare name, `GetAtt` splits a
/// dotted scalar into `["A","B"]`, and **every other** suffix (known wiring/
/// condition intrinsics *and any future one*) maps uniformly to `Fn::<suffix>`.
/// A tag with any other handle (a core YAML tag such as `!!str`, handle
/// `tag:yaml.org,2002:`) is **ignored** and the value kept as-is. The downstream
/// grader consumes these long-form maps unchanged, so an `!If`-wrapped reference
/// has no `Fn::If` arm and grades `Unresolved` — a `Resource` is never invented
/// by reaching through a condition.
///
/// ## Anchors & aliases
///
/// Anchored nodes (`&id`) are recorded in [`anchor_map`](Self::anchor_map) by
/// their anchor id as they complete; an `Alias(id)` resolves to a clone of that
/// value. A forward/unknown alias resolves to `Value::Null` rather than
/// panicking. CFN templates rarely use anchors, but they are handled because the
/// event stream carries the ids for free.
#[derive(Default)]
struct CfnEventLoader {
    /// The stack of open containers (innermost last).
    stack: Vec<Frame>,
    /// The completed root document value (set once, at the outermost insert).
    root: Option<Value>,
    /// Constructed values by anchor id, for alias resolution.
    anchor_map: BTreeMap<usize, Value>,
    /// The first structural error encountered (e.g. a duplicate mapping key),
    /// checked after the parse drains.
    error: Option<String>,
}

impl MarkedEventReceiver for CfnEventLoader {
    fn on_event(&mut self, ev: Event, _mark: Marker) {
        if self.error.is_some() {
            return;
        }
        match ev {
            // Document/stream framing carries no value.
            Event::StreamStart
            | Event::StreamEnd
            | Event::DocumentStart
            | Event::DocumentEnd
            | Event::Nothing => {}

            Event::Scalar(value, style, anchor, tag) => {
                let node = scalar_to_value(value, style, tag);
                self.insert(node, anchor);
            }

            Event::SequenceStart(anchor, tag) => {
                self.stack.push(Frame::Seq {
                    tag,
                    anchor,
                    items: Vec::new(),
                });
            }
            Event::SequenceEnd => {
                let Some(Frame::Seq { tag, anchor, items }) = self.stack.pop() else {
                    self.error = Some("unbalanced sequence in YAML event stream".to_string());
                    return;
                };
                let node = apply_cfn_tag(tag.as_ref(), Value::Array(items));
                self.insert(node, anchor);
            }

            Event::MappingStart(anchor, tag) => {
                self.stack.push(Frame::Map {
                    tag,
                    anchor,
                    entries: Map::new(),
                    pending_key: None,
                });
            }
            Event::MappingEnd => {
                let Some(Frame::Map {
                    tag,
                    anchor,
                    entries,
                    ..
                }) = self.stack.pop()
                else {
                    self.error = Some("unbalanced mapping in YAML event stream".to_string());
                    return;
                };
                let node = apply_cfn_tag(tag.as_ref(), Value::Object(entries));
                self.insert(node, anchor);
            }

            Event::Alias(id) => {
                // A known anchor resolves to its constructed value; an unknown
                // (e.g. forward) alias resolves to null rather than panicking.
                let node = self.anchor_map.get(&id).cloned().unwrap_or(Value::Null);
                // Aliased nodes have no anchor id of their own.
                self.insert(node, 0);
            }
        }
    }
}

impl CfnEventLoader {
    /// Place a completed `node` into the innermost open container — as the next
    /// sequence item, or as a mapping key/value (alternating) — or, if the stack
    /// is empty, as the root document. Records the node under its `anchor` id (if
    /// non-zero) for later alias resolution.
    fn insert(&mut self, node: Value, anchor: usize) {
        if anchor > 0 {
            self.anchor_map.insert(anchor, node.clone());
        }
        match self.stack.last_mut() {
            None => {
                // Outermost value: the document root. (A second top-level node
                // would be a second document, which `multi = false` precludes.)
                if self.root.is_none() {
                    self.root = Some(node);
                }
            }
            Some(Frame::Seq { items, .. }) => items.push(node),
            Some(Frame::Map {
                entries,
                pending_key,
                ..
            }) => match pending_key.take() {
                // No key yet: this node is the key. Mapping keys in CFN are always
                // strings; a non-string key is coerced to a stable textual form so
                // no entry is silently dropped.
                None => *pending_key = Some(value_to_map_key(&node)),
                // Have a key: this node is its value.
                Some(key) => {
                    if entries.insert(key, node).is_some() {
                        // yaml-rust2 also rejects duplicate keys; mirror that as a
                        // parse error rather than silently overwriting.
                        self.error = Some("duplicated key in mapping".to_string());
                    }
                }
            },
        }
    }
}

/// Convert a scalar event into a [`Value`], honouring its CFN tag.
///
/// A **quoted/blocked** scalar is always a string (its style proves the author
/// meant text, never a bool/number/null) — but a CFN tag on it still applies
/// (`!Sub "x"`, `!Ref "AWS::NoValue"`). A **plain** scalar with no CFN tag gets
/// YAML's usual scalar typing (`true`/`123`/`~` → bool/int/null).
fn scalar_to_value(value: String, style: TScalarStyle, tag: Option<Tag>) -> Value {
    if let Some(tag) = tag.as_ref() {
        if is_cfn_handle(tag) {
            // A CFN application tag: the value is the (string) argument, converted
            // to its long-form map.
            return apply_cfn_tag(Some(tag), Value::String(value));
        }
    }
    // No CFN tag (a core YAML tag is ignored): type the scalar by style.
    if style == TScalarStyle::Plain {
        plain_scalar_to_value(&value)
    } else {
        Value::String(value)
    }
}

/// Whether `tag` is a CFN application tag (local handle `"!"`), as opposed to a
/// core YAML tag (`!!str` → handle `tag:yaml.org,2002:`), which we ignore.
fn is_cfn_handle(tag: &Tag) -> bool {
    tag.handle == "!"
}

/// Apply a CFN short-form tag to an already-built `value`, returning the
/// long-form map. A non-CFN handle (or `None`) returns the value unchanged.
///
/// - `!Ref v` → `{"Ref": v}`
/// - `!Condition v` → `{"Condition": v}`
/// - `!GetAtt "A.B"` → `{"Fn::GetAtt": ["A","B"]}` (dotted scalar split on every
///   `.`; the grader reads element 0 as the logical id); `!GetAtt [A, B]` →
///   `{"Fn::GetAtt": [A, B]}` (sequence kept as-is).
/// - any other `!X v` → `{"Fn::X": v}` — uniform for every known intrinsic
///   (`Sub`/`Join`/`If`/`Equals`/`Not`/`And`/`Or`/`FindInMap`/`Select`/`Split`/
///   `ImportValue`/`GetAZs`/`Base64`/`Cidr`/`Transform`) **and any future tag**.
fn apply_cfn_tag(tag: Option<&Tag>, value: Value) -> Value {
    let Some(tag) = tag else {
        return value;
    };
    if !is_cfn_handle(tag) {
        return value;
    }
    let suffix = tag.suffix.as_str();
    match suffix {
        "Ref" => single("Ref", value),
        "Condition" => single("Condition", value),
        "GetAtt" => single("Fn::GetAtt", getatt_value(value)),
        // Every other CFN tag (known or future) maps uniformly to `Fn::<suffix>`.
        _ => single(&format!("Fn::{suffix}"), value),
    }
}

/// Normalize a `!GetAtt` argument to its long-form list shape: a dotted scalar
/// `"A.B.C"` splits into `["A","B","C"]`; a value that is already a list (the
/// `!GetAtt [A, B]` sequence form) is kept as-is; anything else is wrapped in a
/// single-element list so the grader's first-element rule still applies.
fn getatt_value(value: Value) -> Value {
    match value {
        Value::String(s) => {
            Value::Array(s.split('.').map(|p| Value::String(p.to_string())).collect())
        }
        arr @ Value::Array(_) => arr,
        other => Value::Array(vec![other]),
    }
}

/// Build a single-key long-form map `{key: value}`.
fn single(key: &str, value: Value) -> Value {
    let mut m = Map::with_capacity(1);
    m.insert(key.to_string(), value);
    Value::Object(m)
}

/// Type a *plain* (unquoted, untagged) YAML scalar the way YAML 1.2 cores do:
/// `null`/`~`/empty → null, `true`/`false` (any case) → bool, an integer or
/// float literal → number, otherwise a string. This mirrors the high-level
/// loader's plain-scalar resolution so the event path types literals identically.
fn plain_scalar_to_value(s: &str) -> Value {
    match s {
        "" | "~" | "null" | "Null" | "NULL" => return Value::Null,
        "true" | "True" | "TRUE" => return Value::Bool(true),
        "false" | "False" | "FALSE" => return Value::Bool(false),
        _ => {}
    }
    if let Ok(i) = s.parse::<i64>() {
        return Value::from(i);
    }
    if let Ok(f) = s.parse::<f64>() {
        // Reject the non-finite spellings `parse::<f64>` accepts (`inf`, `nan`):
        // a CFN literal like `NaN` is data, not a float, and JSON has no such
        // number — keep it a string.
        if f.is_finite() {
            if let Some(n) = serde_json::Number::from_f64(f) {
                return Value::Number(n);
            }
        }
    }
    Value::String(s.to_string())
}

/// Coerce a completed node used as a mapping key to a `String`. Template keys
/// (`Resources`, logical ids, `Type`, …) are always strings; the rare non-string
/// key (legal in YAML, absent in CFN) gets a stable textual form so no entry is
/// dropped.
fn value_to_map_key(node: &Value) -> String {
    match node {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Parse YAML through the event loader (the path under test), panicking with
    /// the parse error on failure so a regression reads clearly.
    fn yaml(src: &str) -> Value {
        parse_yaml_to_value(src).unwrap_or_else(|e| panic!("parse {src:?}: {e}"))
    }

    #[test]
    fn ref_and_condition_become_bare_long_form_maps() {
        assert_eq!(yaml("v: !Ref Thing\n"), json!({ "v": { "Ref": "Thing" } }));
        assert_eq!(
            yaml("v: !Condition IsProd\n"),
            json!({ "v": { "Condition": "IsProd" } })
        );
    }

    #[test]
    fn getatt_dotted_scalar_splits_into_list() {
        assert_eq!(
            yaml("v: !GetAtt Role.Arn\n"),
            json!({ "v": { "Fn::GetAtt": ["Role", "Arn"] } })
        );
        // A nested attribute path splits on every `.`.
        assert_eq!(
            yaml("v: !GetAtt A.B.C\n"),
            json!({ "v": { "Fn::GetAtt": ["A", "B", "C"] } })
        );
    }

    #[test]
    fn getatt_sequence_form_is_kept_as_list() {
        assert_eq!(
            yaml("v: !GetAtt [Role, Arn]\n"),
            json!({ "v": { "Fn::GetAtt": ["Role", "Arn"] } })
        );
    }

    #[test]
    fn arbitrary_and_future_tags_map_uniformly_to_fn_prefix() {
        // Known wiring/condition intrinsics …
        assert_eq!(
            yaml(r#"v: !Sub "api-${Stage}""#),
            json!({ "v": { "Fn::Sub": "api-${Stage}" } })
        );
        // … and an unknown FUTURE tag, same uniform rule (no enumerated list).
        assert_eq!(
            yaml("v: !SomeFutureTag payload\n"),
            json!({ "v": { "Fn::SomeFutureTag": "payload" } })
        );
    }

    #[test]
    fn inline_and_multiline_flow_if_preserve_inner_refs() {
        // The inline form: inner `!Ref`s convert; the quoted pseudo-parameter is
        // kept verbatim as the Ref argument.
        let inline = yaml(r#"v: !If [Cond, !Ref A, !Ref "AWS::NoValue"]"#);
        assert_eq!(
            inline,
            json!({ "v": { "Fn::If": ["Cond", { "Ref": "A" }, { "Ref": "AWS::NoValue" }] } })
        );
        // The multi-line flow form with a trailing comma (the shape that defeated
        // the textual normalizer) parses to the identical structure.
        let multiline = yaml("v: !If [\n  Cond,\n  !Ref A,\n  !Ref \"AWS::NoValue\",\n]\n");
        assert_eq!(multiline, inline);
    }

    #[test]
    fn block_scalar_sub_keeps_body_and_converts() {
        // A `!Sub |` block scalar: the body is preserved intact, the tag applied —
        // the case that orphaned the body under textual rewriting.
        let v = yaml("v: !Sub |\n  line1 ${Stage}\n  line2\n");
        assert_eq!(v, json!({ "v": { "Fn::Sub": "line1 ${Stage}\nline2\n" } }));
    }

    #[test]
    fn block_sequence_if_arg_is_a_normal_list() {
        // The block-sequence argument form (`!If` then an indented `- a / - b`)
        // converts uniformly with the flow form.
        let v = yaml("v: !If\n  - Cond\n  - !Ref A\n  - !Ref B\n");
        assert_eq!(
            v,
            json!({ "v": { "Fn::If": ["Cond", { "Ref": "A" }, { "Ref": "B" }] } })
        );
    }

    #[test]
    fn core_yaml_tag_is_ignored_never_a_cfn_intrinsic() {
        // `!!str 123` is a core YAML tag (handle `tag:yaml.org,2002:`), not a CFN
        // application tag: it is ignored — crucially it is NEVER misread as a CFN
        // intrinsic `{"Fn::str": …}`. The (plain) scalar keeps its ordinary YAML
        // typing; the only contract that matters here is the absence of an `Fn::`
        // wrapper (the value is not a reference either way).
        let v = yaml("v: !!str 123\n");
        assert_eq!(v, json!({ "v": 123 }));
        assert!(
            v["v"].as_object().is_none(),
            "a core YAML tag must never become a CFN intrinsic map"
        );
    }

    #[test]
    fn plain_scalars_are_typed_but_quoted_ones_stay_strings() {
        // Plain scalars get YAML typing …
        assert_eq!(
            yaml("a: true\nb: 42\nc: 3.5\nd: ~\n"),
            json!({ "a": true, "b": 42, "c": 3.5, "d": null })
        );
        // … but a quoted scalar is always a string, even when it looks numeric.
        assert_eq!(yaml(r#"v: "42""#), json!({ "v": "42" }));
    }

    #[test]
    fn anchors_and_aliases_resolve() {
        // A scalar anchor reused by alias resolves to the same value.
        assert_eq!(
            yaml("a: &x hello\nb: *x\n"),
            json!({ "a": "hello", "b": "hello" })
        );
        // A mapping anchor likewise resolves to a clone of the whole mapping.
        assert_eq!(
            yaml("a: &m\n  k: v\nb: *m\n"),
            json!({ "a": { "k": "v" }, "b": { "k": "v" } })
        );
    }

    #[test]
    fn unknown_alias_surfaces_as_parse_error_not_panic() {
        // An alias to an undefined anchor is rejected by `yaml-rust2`'s scanner
        // itself (the `Alias` event never reaches the loader): it surfaces as a
        // parse error — never a panic. (The loader's own null fallback is a
        // belt-and-braces guard for an id the scanner accepted but we did not
        // record, which valid YAML never produces.)
        let err = parse_yaml_to_value("a: *missing\n").unwrap_err();
        assert!(err.contains("invalid YAML"), "got {err:?}");
    }

    #[test]
    fn malformed_yaml_surfaces_an_error() {
        // An unterminated flow is a scanner error, surfaced (never a panic).
        let err = parse_yaml_to_value("v: !If [\n  Cond,\n").unwrap_err();
        assert!(err.contains("invalid YAML"), "got {err:?}");
    }

    // ── Fn::If grading (Slice 10, B2) ──
    //
    // `grade()` gains an `Fn::If` arm: collect Ref/GetAtt ids from BOTH branches;
    // each id naming a same-template resource → Inferred (band 0.70 downstream),
    // surfacing both possible deployments (recall-biased). Ids resolving to
    // nothing contribute nothing (never invented). Distinct branch targets →
    // `InferredMulti` so the builder emits an edge per target.

    /// Grade a single property value the way `extract_resource` does.
    fn grade_prop(yaml_src: &str, ids: &[&str]) -> RefValue {
        let v = yaml(yaml_src);
        let id_set: BTreeSet<&str> = ids.iter().copied().collect();
        grade(&v["v"], &id_set)
    }

    #[test]
    fn fn_if_same_resource_both_branches_is_single_inferred() {
        // !If [Cond, !GetAtt DS.Name, !GetAtt DS.Name] → Inferred("DS") (the
        // common shape: a condition toggling a property but the same target).
        let r = grade_prop("v: !If [Cond, !GetAtt DS.Name, !GetAtt DS.Name]\n", &["DS"]);
        assert_eq!(r, RefValue::Inferred("DS".to_string()));
    }

    #[test]
    fn fn_if_one_resolvable_branch_is_single_inferred() {
        // !If [Cond, !Ref Fn, !Ref "AWS::NoValue"] → Inferred("Fn"): the pseudo-
        // parameter resolves to nothing and contributes nothing (never invented).
        let r = grade_prop("v: !If [Cond, !Ref Fn, !Ref \"AWS::NoValue\"]\n", &["Fn"]);
        assert_eq!(r, RefValue::Inferred("Fn".to_string()));
    }

    #[test]
    fn fn_if_distinct_branch_targets_is_inferred_multi() {
        // !If [Cond, !Ref Blue, !Ref Green] with both same-template resources →
        // InferredMulti(["Blue", "Green"]) — both deployments surfaced.
        let r = grade_prop("v: !If [Cond, !Ref Blue, !Ref Green]\n", &["Blue", "Green"]);
        match r {
            RefValue::InferredMulti(ids) => {
                assert_eq!(ids, vec!["Blue".to_string(), "Green".to_string()]);
            }
            other => panic!("expected InferredMulti, got {other:?}"),
        }
    }

    #[test]
    fn fn_if_no_resolvable_branch_is_unresolved() {
        // !If [Cond, !Ref ParamA, !Ref "AWS::NoValue"] where ParamA is NOT a
        // template resource → Unresolved (never a Resource invented).
        let r = grade_prop(
            "v: !If [Cond, !Ref ParamA, !Ref \"AWS::NoValue\"]\n",
            &["SomethingElse"],
        );
        assert_eq!(r, RefValue::Unresolved);
    }

    #[test]
    fn fn_if_never_yields_resource_only_inferred() {
        // The honesty invariant: an `!If`-wrapped reference is NEVER `Resource`
        // (a fact), always at most `Inferred` — we cannot pin which branch
        // deploys without evaluating the condition.
        let r = grade_prop("v: !If [Cond, !GetAtt DS.Name, !GetAtt DS.Name]\n", &["DS"]);
        assert!(
            !matches!(r, RefValue::Resource(_)),
            "an Fn::If reference must never grade Resource, got {r:?}"
        );
    }

    #[test]
    fn fn_if_dedups_repeated_distinct_targets() {
        // Branches mentioning the same id more than once collapse to one entry;
        // two distinct ids stay distinct, in first-seen order.
        let r = grade_prop(
            "v: !If [Cond, !Ref Blue, !If [Inner, !Ref Green, !Ref Blue]]\n",
            &["Blue", "Green"],
        );
        match r {
            RefValue::InferredMulti(ids) => {
                assert_eq!(ids, vec!["Blue".to_string(), "Green".to_string()]);
            }
            // A single distinct id would be Inferred; here there are two.
            other => panic!("expected InferredMulti([Blue, Green]), got {other:?}"),
        }
    }

    #[test]
    fn json_path_is_unchanged() {
        // JSON is still parsed directly (already long-form) — the event loader is
        // YAML-only.
        let v = parse_to_value(r#"{"Resources": {"R": {"Type": "AWS::X::Y"}}}"#).unwrap();
        assert_eq!(v, json!({ "Resources": { "R": { "Type": "AWS::X::Y" } } }));
    }
}
