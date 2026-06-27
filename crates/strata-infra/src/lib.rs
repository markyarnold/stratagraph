//! strata-infra: a format-agnostic infrastructure-as-code plane.
//!
//! An [`IacAdapter`] turns an IaC template file (its raw text) into a set of
//! typed [`InfraResource`]s plus the intra-template references between them. The
//! AWS CloudFormation / SAM adapter ([`CfnSamAdapter`]) was the first
//! implementation; the [`TerraformAdapter`] (Terraform/OpenTofu `.tf`, Track D1)
//! is the second, producing the SAME [`InfraResource`]s/[`RefValue`] grades so
//! both flow through `strata-index`'s one `build_infra_plane` path. The interface
//! is deliberately format-agnostic — it mirrors `strata-contract`'s
//! `ContractAdapter` shape (`detects` + `extract`, pure, no IO, fixture-tested).
//!
//! Pure: the caller reads the template file and hands the adapter its text, so
//! the same `(path, content)` always yields the same [`InfraTemplate`]
//! (determinism). A malformed template returns [`InfraError::Parse`] and never
//! panics, and never partially extracts a broken document (graceful, visible
//! degradation).
//!
//! ## Reference provenance (honesty)
//!
//! Every reference field is graded by [`RefValue`] with explicit provenance: a
//! same-template `Ref`/`GetAtt` becomes [`RefValue::Resource`]; a `Sub`/`Join`
//! from which a `${LogicalId}` can be recovered becomes [`RefValue::Inferred`];
//! a parameter / cross-stack import / opaque value is surfaced as
//! [`RefValue::Unresolved`]. A `Resource(id)` is NEVER invented for something
//! that is not a resource in the same template.

use thiserror::Error;

mod cfn;
mod iam;
mod terraform;
mod terragrunt;
mod tfplan;

pub use cfn::{raw_resource_signatures, CfnSamAdapter};
pub use iam::{parse_policy_document, parse_sam_policies, sam_template_actions, PolicyGrants};
pub use terraform::TerraformAdapter;
pub use terragrunt::{extract_unit, is_terragrunt_file, TerragruntDependency, TerragruntUnit};
pub use tfplan::{dedup_plan_over_hcl, extract_plan, is_plan_json};

/// The verdict of [`CfnSamAdapter::detect_kind`]: a richer classification than the
/// boolean [`IacAdapter::detects`], used so the indexer can tell a **malformed
/// template** (which must be surfaced as a visible failure, not silently dropped)
/// apart from a file that is simply not a CloudFormation/SAM template at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CfnDetection {
    /// The content has the structural shape of a template (a `Resources` map with
    /// at least one `AWS::…` typed resource): extract it.
    Template,
    /// The content carries the CFN textual signal (both `Resources` and `AWS::`
    /// substrings) but could **not** be parsed — almost certainly a real template
    /// that is malformed/truncated. Carries the parse error so the caller can
    /// report `path + error` rather than skip it silently.
    Malformed(String),
    /// Not a CloudFormation/SAM template (the textual signal is absent, or the
    /// content parses but has no `Resources` map of `AWS::…` resources): skip it.
    NotCfn,
}

/// An error from parsing an IaC template.
#[derive(Debug, Error)]
pub enum InfraError {
    /// The template text could not be parsed (malformed JSON/YAML, or a
    /// structurally invalid template such as a non-map `Resources`). Carries the
    /// template path and a human-readable reason.
    #[error("parse error in {path}: {msg}")]
    Parse {
        /// The template path (caller-supplied; repo-relative).
        path: String,
        /// Human-readable reason the template could not be parsed.
        msg: String,
    },
}

/// How a reference inside a template resolved, with explicit provenance.
///
/// The grading is deliberately conservative: a [`Resource`](RefValue::Resource)
/// is only produced for a `Ref`/`GetAtt` whose target is a logical id present in
/// the same template. Anything that cannot be tied to a same-template resource
/// with certainty is either best-effort [`Inferred`](RefValue::Inferred) (from a
/// `Sub`/`Join` interpolation) or honestly [`Unresolved`](RefValue::Unresolved)
/// — never a guessed `Resource`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefValue {
    /// A plain string value (e.g. an inline ARN/handler). Provenance: extracted.
    Literal(String),
    /// A `Ref`/`GetAtt` to a logical id present in the same template — the id is
    /// carried (any `GetAtt` attribute is dropped). Provenance: extracted.
    Resource(String),
    /// A best-effort identifier recovered from a `Sub`/`Join` interpolation
    /// (e.g. the `LogicalId` inside `${LogicalId.Arn}`), or the single
    /// same-template resource a `Fn::If` could reach. Provenance: inferred.
    Inferred(String),
    /// Two or more DISTINCT same-template resources a `Fn::If` could reach (one
    /// per branch) — both possible deployments surfaced, recall-biased (Slice 10,
    /// B2). The builder emits an edge per id, each at the Inferred tier. Ids are
    /// deduplicated, in first-seen order; the single-id case collapses to
    /// [`Inferred`](RefValue::Inferred), the zero-id case to
    /// [`Unresolved`](RefValue::Unresolved). Provenance: inferred. An `Fn::If`
    /// reference is NEVER [`Resource`](RefValue::Resource) — we cannot pin which
    /// branch deploys without evaluating the condition.
    InferredMulti(Vec<String>),
    /// A parameter reference, cross-stack `ImportValue`, or otherwise opaque
    /// value. Surfaced honestly rather than guessed.
    Unresolved,
}

/// The classification of a template resource.
///
/// The infrastructure vertical's resource types are first-class; everything else
/// is [`Generic`](InfraKind::Generic) (inventory only — logical id + CFN type).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfraKind {
    /// `AWS::Lambda::Function` or the SAM shorthand `AWS::Serverless::Function`.
    LambdaFunction,
    /// `AWS::IAM::Role`.
    IamRole,
    /// `AWS::IAM::Policy` — a managed/inline policy resource that grants actions to
    /// the roles listed in its `Roles` property (Track D2). Not a graph node itself;
    /// its grants are attached as `Grants` edges to the referenced role nodes at
    /// build time. Its target roles are carried in [`InfraResource::role_refs`].
    IamPolicy,
    /// `AWS::AppSync::GraphQLApi`.
    AppSyncApi,
    /// `AWS::AppSync::Resolver`.
    AppSyncResolver,
    /// `AWS::AppSync::DataSource`.
    AppSyncDataSource,
    /// Any other resource type: carried as inventory (id + CFN type) only.
    Generic,
}

/// A single resource extracted from a template.
///
/// `logical_id` and `cfn_type` are always populated. The vertical-specific
/// fields are populated only for the matching [`InfraKind`] and only when the
/// corresponding property is present in the template; they are `None` otherwise.
#[derive(Debug, Clone, PartialEq)]
pub struct InfraResource {
    /// The resource's logical id (its key under `Resources`).
    pub logical_id: String,
    /// The CFN resource type, e.g. `"AWS::Serverless::Function"`.
    pub cfn_type: String,
    /// The classification derived from `cfn_type`.
    pub kind: InfraKind,

    // ── Lambda / serverless function ────────────────────────────────────────
    /// Lambda `Handler` (literal), when present.
    pub handler: Option<String>,
    /// Lambda `CodeUri` / local code path (literal), when present.
    pub code_uri: Option<String>,
    /// Every role-referencing property on this resource, graded — collected on
    /// EVERY resource kind from `Role`, `RoleArn`, `ServiceRoleArn`,
    /// `ExecutionRoleArn`, `TaskRoleArn`, plus AppSync's nested
    /// `LogConfig.CloudWatchLogsRoleArn`. AWS spreads role references across
    /// many property names (dogfood: a real template had 13× `ServiceRoleArn`
    /// vs 1× `Role`); collecting only `Role` left real role nodes with nothing
    /// assuming them.
    pub role_refs: Vec<RefValue>,

    // ── AppSync resolver ────────────────────────────────────────────────────
    /// Resolver `TypeName` (literal), when present.
    pub type_name: Option<String>,
    /// Resolver `FieldName` (literal), when present.
    pub field_name: Option<String>,
    /// Resolver `DataSourceName`, graded.
    pub data_source_ref: Option<RefValue>,

    // ── AppSync data source ─────────────────────────────────────────────────
    /// Data source `LambdaConfig.LambdaFunctionArn`, graded.
    pub lambda_ref: Option<RefValue>,
    /// Resolver / data source `ApiId`, graded.
    pub api_ref: Option<RefValue>,

    // ── IAM permission-gap (Track D2) ───────────────────────────────────────
    /// Allow-ed IAM actions/wildcards this resource grants: an `IamRole`'s inline
    /// `Policies` statements, or an `IamPolicy`'s statements. Each entry is a
    /// concrete action (`dynamodb:PutItem`), a service wildcard (`dynamodb:*`), or
    /// `*`. Only `Effect: Allow` actions appear here (a `Deny` sets `grants_opaque`).
    pub granted_actions: Vec<String>,
    /// Reasons this resource's grants cannot be fully enumerated — a managed-policy
    /// ARN, a SAM policy template not resolved here, or a `Deny` statement. When
    /// non-empty, the permission-gap traversal treats the role as INDETERMINATE
    /// (suppresses gaps) rather than risk a confident-wrong alarm (Track D2).
    pub grants_opaque: Vec<String>,
}

impl InfraResource {
    /// A bare resource with only its identity populated (all vertical fields
    /// `None`). The classification is derived from `cfn_type` via
    /// [`InfraKind`]'s mapping in the adapter.
    fn new(logical_id: String, cfn_type: String, kind: InfraKind) -> Self {
        InfraResource {
            logical_id,
            cfn_type,
            kind,
            handler: None,
            code_uri: None,
            role_refs: Vec::new(),
            type_name: None,
            field_name: None,
            data_source_ref: None,
            lambda_ref: None,
            api_ref: None,
            granted_actions: Vec::new(),
            grants_opaque: Vec::new(),
        }
    }
}

/// The result of extracting one template: its path plus the resources found.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InfraTemplate {
    /// The template path (caller-supplied; repo-relative).
    pub path: String,
    /// The resources extracted, in template (map) order.
    pub resources: Vec<InfraResource>,
}

/// An infrastructure-as-code adapter. CloudFormation/SAM is the first
/// implementation; the interface mirrors `strata-contract`'s `ContractAdapter`.
pub trait IacAdapter {
    /// Cheap heuristic: does `filename`/`content` look like this adapter's IaC
    /// format? Used to pick template files out of a repo before the (more
    /// expensive) [`extract`](IacAdapter::extract). Must reject non-templates
    /// (build specs, `samconfig.toml`, docker-compose, arbitrary YAML/JSON).
    fn detects(&self, filename: &str, content: &str) -> bool;

    /// Parse a template's text into an [`InfraTemplate`]. A malformed template
    /// returns [`InfraError::Parse`] so the caller can degrade (skip it, keep
    /// indexing) rather than crash, and never yields a partial extraction.
    fn extract(&self, path: &str, content: &str) -> Result<InfraTemplate, InfraError>;
}
