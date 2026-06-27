//! Infrastructure-plane assembly: typed IaC nodes + wiring edges + the
//! `Runs`/`PRODUCES` bridges that connect AWS infrastructure to the code and
//! contract planes.
//!
//! This runs **after** the code plane (it bridges Lambdas to code `Module`
//! nodes) and **after** the contract plane (the `GraphqlField` nodes the money
//! link points at must already exist). It is a pure, deterministic function of
//! the detected [`InfraTemplate`]s, the analyzed-file set, and the graph built so
//! far — no IO — so it is unit-testable in isolation and reproducible (R3).
//!
//! **Honest provenance (R1).** Every edge is graded from the source template's
//! [`RefValue`]: a same-template `Ref`/`GetAtt` ([`RefValue::Resource`]) is a
//! template *fact* → `Extracted` 0.95; a `Sub`/`Join`-recovered id, or the single
//! same-template resource a `Fn::If` could reach ([`RefValue::Inferred`]), is a
//! best-effort inference → `Inferred` 0.70; a `Fn::If` over two or more DISTINCT
//! same-template targets ([`RefValue::InferredMulti`]) emits one `Inferred` 0.70
//! edge **per branch target** — both possible deployments surfaced, recall-biased
//! (a `Fn::If` reference is never `Extracted`: we cannot pin which branch deploys
//! without evaluating the condition); an [`RefValue::Unresolved`] (parameter,
//! cross-stack import, dynamic ARN) or a reference to a non-resource produces
//! **no edge** — surfaced by its absence and tallied in coverage, never invented.
//! The `Runs` bridge is `Extracted` 0.95
//! only when exactly one indexed module matches the handler path; an ambiguous or
//! absent handler produces no edge (counted). The money-link `PRODUCES` source is
//! the **Lambda** when the resolver→datasource→lambda chain is wholly
//! `Resource`-graded (a fact), else the resolver node itself; a resolver whose
//! `{type}.{field}` key names no `GraphqlField` produces no edge (counted).

use std::collections::{BTreeMap, BTreeSet};

use strata_core::{
    AnalyzedFile, Confidence, Direction, Edge, EdgeKind, Graph, Node, NodeKind, Provenance, Span,
    Uid,
};
use strata_infra::{InfraKind, InfraResource, InfraTemplate, RefValue, TerragruntUnit};

use crate::build::uid_module;

/// The language/plane tag for infra-plane UIDs (distinct from code `"ts"` and
/// contract `"contract"`).
const INFRA_LANG: &str = "infra";

/// A `Resource`-graded reference is a same-template fact → `Extracted` 0.95 (the
/// EXTRACTED band floor). The template *names* the target resource by logical id.
pub const CONF_REF_RESOURCE: f32 = 0.95;
/// An `Inferred`-graded reference (recovered from a `Sub`/`Join` interpolation)
/// → `Inferred` 0.70, comfortably inside the Inferred band [0.40, 0.80]. A
/// best-effort identifier, not a crisp `Resource`.
pub const CONF_REF_INFERRED: f32 = 0.70;
/// A `Runs` bridge (Lambda → its handler's code `Module`) is an `Extracted`
/// template fact when exactly one indexed module matches the handler path.
pub const CONF_RUNS: f32 = 0.95;
/// The money-link `PRODUCES` confidence. `TypeName`/`FieldName` are template
/// literals, so the resolver→field link is itself a fact (`Extracted` 0.95)
/// regardless of which node sources it; an `Inferred` hop *in the chain* only
/// downgrades the edge when the **Lambda** sources it (see [`produces_source`]).
pub const CONF_PRODUCES_EXTRACTED: f32 = 0.95;
/// The money-link `PRODUCES` confidence when the resolver→datasource→lambda chain
/// contains an `Inferred` hop and the **Lambda** sources the edge: the weakest
/// hop dominates, so the whole link drops to `Inferred` 0.70.
pub const CONF_PRODUCES_INFERRED: f32 = 0.70;

/// The candidate handler-file extensions a Lambda `Handler` stem may resolve to,
/// in priority order. TS/JS **and Python** (`.py`/`.pyi`) — the Python code plane
/// (Slice 9) now indexes `.py` handlers, so a Lambda whose handler resolves to a
/// Python file links to its `Module` node just like a TS handler. A handler in a
/// language with no plane yet (Go, Java, …) still stays unresolved, counted
/// honestly.
///
/// **C# (`.cs`) is deliberately NOT here (Slice 11).** A .NET Lambda `Handler` is
/// a CLR reference, not a file path: `Assembly::Namespace.Type::Method` (e.g.
/// `MyFunctions::MyFunctions.Handlers::Handle`). The first segment is the *built
/// assembly* name, not a source file — resolving it to the `.cs` `Module` node the
/// C# plane indexed needs the `.csproj`/assembly-name mapping, which is out of
/// scope this slice. So a C# Lambda keeps the honest `lambdas_handler_unresolved`
/// count (the handler-shaped `::` string never matches a `stem.ext` candidate),
/// exactly the design's "surface a miss, never invent". The C# `Module` nodes DO
/// exist in the graph (the plane indexed them); only the infra `Runs` bridge to
/// them is deferred.
///
/// **Rust (`.rs`) is likewise deliberately NOT here (Slice 21).** A Rust
/// (cargo-lambda) Lambda's `Handler` is conventionally `bootstrap` (the
/// provided.al2 runtime entrypoint) and the deployed artifact maps to a Cargo
/// **binary name** — `[[bin]] name` / `package.name` in `Cargo.toml`, often via
/// `cargo lambda build --bin <name>` — NOT a `.rs` source-file path. Resolving that
/// binary name to the `main.rs`/`bin/*.rs` `Module` node the Rust plane indexed
/// needs the `Cargo.toml` target table, which is out of scope this slice. So a Rust
/// Lambda also keeps the honest `lambdas_handler_unresolved` count; its `Module`
/// nodes exist in the graph, only the `Runs` bridge is deferred (the Python plane,
/// whose file-path handlers EARNED their `Runs` edge in Slice 9, is the contrast).
const HANDLER_EXTS: [&str; 8] = ["ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "pyi"];

/// Per-repo infrastructure link coverage (R4): the headline numbers the committed
/// `docs/accuracy/infra-linking.md` report publishes and the CI gate floors.
///
/// Counts are honest: a resolver that names no schema field is `resolvers_total`
/// but not `resolvers_linked` (it is `resolvers_unlinked`); a Lambda whose
/// handler path matches zero or several modules is `lambdas_handler_unresolved`,
/// not `lambdas_runs_linked`. An unresolved reference (a dropped `Assumes`/
/// `Routes` edge) is implied by the absence of the edge — surfaced by the graph,
/// not separately tallied here (the resolver/Lambda buckets are the report's
/// headline honesty signal).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct InfraLinkCoverage {
    /// CFN/SAM templates detected and extracted in this repo.
    pub templates_detected: usize,
    /// Files carrying the CFN textual signal that could not be parsed (a
    /// malformed/truncated template, classified `Malformed` by
    /// `CfnSamAdapter::detect_kind`). These are **skipped** so the rest of the repo
    /// indexes (R2), but the count is surfaced here and the per-template
    /// (path, error) diagnostics are carried in
    /// [`IndexStats::infra_diagnostics`](crate::IndexStats) — a silently dropped
    /// template must never happen again. Note `build_infra_plane` only sees the
    /// templates that parsed, so it always leaves this `0`; the indexer sets the
    /// real count from extraction (see `index_impl`).
    pub templates_failed: usize,
    /// Total resources across all detected templates (every `InfraResource`).
    pub resources_total: usize,
    /// Total AppSync resolvers with a root `TypeName`+`FieldName` (the money-link
    /// candidates).
    pub resolvers_total: usize,
    /// Resolvers that produced a `PRODUCES` edge (their `{type}.{field}` named a
    /// `GraphqlField` in the graph).
    pub resolvers_linked: usize,
    /// Resolvers whose `{type}.{field}` named no `GraphqlField` (a field no schema
    /// declares, or a non-root type) → no edge. Surfaced, never invented.
    pub resolvers_unlinked: usize,
    /// Lambdas that produced a `Runs` edge (exactly one indexed module matched
    /// their handler path).
    pub lambdas_runs_linked: usize,
    /// Lambdas whose handler path matched zero or several indexed modules → no
    /// `Runs` edge. With the Python plane (Slice 9), `.py`/`.pyi` handlers now
    /// resolve; what lands here is a handler in a language with no plane yet
    /// (Go, Java, …), a `CodeUri` outside the indexed tree, or a genuine miss.
    pub lambdas_handler_unresolved: usize,
    /// Standalone/inline IAM policies (CFN `AWS::IAM::Policy`, TF
    /// `aws_iam_role_policy`/`aws_iam_policy`/`_attachment`) that carry grants or
    /// `<opaque:…>` markers but whose target role(s) could not be resolved to a node
    /// (`role = var.x`, `Roles: [!Ref Param]`, an ARN string, a cross-stack ref) →
    /// the grants attach to no role. Tallied, never silently dropped: a dropped
    /// grant would make a role look grant-complete when it is not, which the
    /// (future) permission-gap traversal must treat conservatively rather than
    /// raise a false gap (Track D2, never-confident-wrong).
    pub iam_policy_grants_unattributed: usize,
}

/// Add the infrastructure plane to a graph that already has its code plane and
/// contract plane: typed nodes for every resource, `Assumes`/`Routes` wiring
/// edges, `Runs` bridges to code `Module`s, and the money-link `PRODUCES` edges
/// from resolver chains to `GraphqlField`s. Returns the [`InfraLinkCoverage`].
///
/// `templates` are all the [`InfraTemplate`]s detected in this repo (in path
/// order, the caller's responsibility). `analyzed` is the same map the code plane
/// was built from; it is the existence oracle for the `Runs` handler match.
/// Idempotent by UID: building twice over the same inputs yields the same graph.
pub fn build_infra_plane(
    g: &mut Graph,
    repo_name: &str,
    templates: &[InfraTemplate],
    analyzed: &BTreeMap<String, AnalyzedFile>,
) -> InfraLinkCoverage {
    let mut cov = InfraLinkCoverage::default();

    // The set of indexed module paths — the existence oracle for the `Runs`
    // handler match (we never invent a Module the code plane did not create).
    let module_paths: BTreeSet<&str> = analyzed.keys().map(String::as_str).collect();

    // The GraphqlField key ("Query.getUser") → its node UID, harvested from the
    // graph the contract plane built. Scanning the graph (not the OperationDefs)
    // makes this correct for BOTH the per-repo node UIDs and the estate-canonical
    // ones — the money link points at whatever GraphqlField node is present.
    let gql_fields: BTreeMap<String, Uid> = g
        .nodes()
        .filter(|n| n.kind == NodeKind::GraphqlField)
        .map(|n| (n.fqn.clone(), n.uid.clone()))
        .collect();

    // ── Pass 1: nodes (Extracted 1.0). Idempotent by UID. ──
    cov.templates_detected = templates.len();
    for template in templates {
        for res in &template.resources {
            cov.resources_total += 1;
            // An IamPolicy is NOT a graph node — its grants attach to the roles it
            // targets (pass 2). Creating a node would be spurious inventory.
            if res.kind != InfraKind::IamPolicy {
                g.add_node(infra_node(repo_name, &template.path, res));
            }
        }
    }

    // ── Pass 2: wiring + Runs + the money link (edges only; nodes all exist). ──
    for template in templates {
        // logical_id → resource, for walking the resolver→datasource→lambda chain
        // (the money link's source selection) within this template.
        let by_id: BTreeMap<&str, &InfraResource> = template
            .resources
            .iter()
            .map(|r| (r.logical_id.as_str(), r))
            .collect();

        for res in &template.resources {
            let src = infra_uid(repo_name, &template.path, &res.logical_id);
            // resource —Assumes→ role, for EVERY role-referencing property on
            // every kind (Role, RoleArn, ServiceRoleArn, execution/task roles,
            // AppSync LogConfig) — a data source assumes its service role just
            // as a function assumes its execution role.
            // An IamPolicy is excluded here: its `role_refs` are grant TARGETS (it
            // grants TO them), not roles it assumes — never `Assumes` edges.
            if res.kind != InfraKind::IamPolicy {
                for role_ref in &res.role_refs {
                    add_ref_edge(
                        g,
                        &src,
                        Some(role_ref),
                        repo_name,
                        &template.path,
                        EdgeKind::Assumes,
                    );
                }
                // GRANTS from a resource that owns IAM grants on its OWN node: an
                // `IamRole` (inline `Policies` / managed ARNs) or a SAM function
                // (its `Policies`, incl. SAM policy templates, via its implicit
                // execution role; Track D2, C2). Each Allow-ed action → role →
                // `CloudAction`, plus an `<opaque:reason>` marker per un-enumerable
                // source so the permission-gap traversal stays never-confident-wrong.
                // (An `IamPolicy` is excluded: its grants attach to the roles it
                // targets, handled with the gap traversal.)
                for action in &res.granted_actions {
                    add_grant(g, repo_name, &src, action);
                }
                for reason in &res.grants_opaque {
                    add_grant(g, repo_name, &src, &format!("<opaque:{reason}>"));
                }
            }
            match res.kind {
                InfraKind::LambdaFunction => {
                    // LambdaFn —Runs→ the code Module its handler resolves to.
                    link_runs(
                        g,
                        &src,
                        res,
                        repo_name,
                        &template.path,
                        &module_paths,
                        &mut cov,
                    );
                }
                InfraKind::AppSyncDataSource => {
                    // datasource —Routes→ lambda (graded).
                    add_ref_edge(
                        g,
                        &src,
                        res.lambda_ref.as_ref(),
                        repo_name,
                        &template.path,
                        EdgeKind::Routes,
                    );
                    // api —Contains→ datasource (from its ApiId; B3).
                    add_contains_edge(g, &src, res.api_ref.as_ref(), repo_name, &template.path);
                }
                InfraKind::AppSyncResolver => {
                    // resolver —Routes→ datasource (graded).
                    add_ref_edge(
                        g,
                        &src,
                        res.data_source_ref.as_ref(),
                        repo_name,
                        &template.path,
                        EdgeKind::Routes,
                    );
                    // api —Contains→ resolver (from its ApiId; B3).
                    add_contains_edge(g, &src, res.api_ref.as_ref(), repo_name, &template.path);
                    // The money link: resolver chain —PRODUCES→ GraphqlField.
                    link_produces(
                        g,
                        res,
                        repo_name,
                        &template.path,
                        &by_id,
                        &gql_fields,
                        &mut cov,
                    );
                }
                InfraKind::IamPolicy => {
                    // A standalone/inline policy (CFN `AWS::IAM::Policy`; TF
                    // `aws_iam_role_policy` / `aws_iam_policy` / `_attachment`)
                    // grants to the role(s) it targets (its `Roles` / `role` refs).
                    // Emit its actions + `<opaque:reason>` markers as `Grants` FROM
                    // each same-template role node (Track D2). The policy itself is
                    // never a node, so we only attach to roles that ARE nodes.
                    let mut attached = false;
                    for role_ref in &res.role_refs {
                        for (role_id, _) in ref_edge_targets(Some(role_ref)) {
                            let role_uid = infra_uid(repo_name, &template.path, role_id);
                            if g.get_node(&role_uid).is_none() {
                                continue;
                            }
                            attached = true;
                            for action in &res.granted_actions {
                                add_grant(g, repo_name, &role_uid, action);
                            }
                            for reason in &res.grants_opaque {
                                add_grant(g, repo_name, &role_uid, &format!("<opaque:{reason}>"));
                            }
                        }
                    }
                    // never-confident-wrong: a policy that grants (or is opaque) but
                    // whose target role(s) we could not resolve to a node (`role =
                    // var.x`, `Roles: [!Ref Param]`, an ARN string, a cross-stack ref)
                    // would otherwise vanish, making a role look grant-complete when it
                    // is not. Tally it so coverage is honest and the (future) gap
                    // traversal can stay conservative rather than raise a false gap.
                    if !attached
                        && (!res.granted_actions.is_empty() || !res.grants_opaque.is_empty())
                    {
                        cov.iam_policy_grants_unattributed += 1;
                    }
                }
                // GRANTS for an `IamRole` and a SAM function's `Policies` are emitted
                // above (from the resource's own node) before this match. The AppSync
                // API and Generic resources carry no OUTGOING infra edges (the API's
                // `Contains` edges are added from the resolver/datasource side).
                InfraKind::IamRole | InfraKind::AppSyncApi | InfraKind::Generic => {}
            }
        }
    }

    cov
}

/// Build a full graph — the code plane **plus** the contract plane **plus** the
/// infra plane — from an in-memory analyzed-file set, the operations extracted
/// from specs, and the detected templates.
///
/// A convenience for hermetic tests (and any in-memory caller) that want the full
/// three-plane graph without reading a repo from disk. The build order matches
/// `index_impl` exactly (code → contract → infra) so the `GraphqlField` nodes the
/// money link points at already exist. Returns the graph and its
/// [`InfraLinkCoverage`].
pub fn assemble_graph_with_infra(
    analyzed: &BTreeMap<String, AnalyzedFile>,
    repo_name: &str,
    opts: &strata_lang_ts::ResolveOptions,
    operations: &[strata_contract::OperationDef],
    templates: &[InfraTemplate],
) -> (Graph, InfraLinkCoverage) {
    let mut g =
        crate::contract::assemble_graph_with_contracts(analyzed, repo_name, opts, operations);
    let cov = build_infra_plane(&mut g, repo_name, templates, analyzed);
    (g, cov)
}

/// The money link. A resolver with a root `TypeName` ∈ {Query, Mutation,
/// Subscription} and a `FieldName` keys on `"{TypeName}.{FieldName}"`; if a
/// `GraphqlField` with that key exists, add a `PRODUCES` edge from the chain's
/// source node to it. The source is the **Lambda** when the
/// resolver→datasource→lambda chain resolves wholly via `Resource` refs (a fact),
/// else the resolver node itself (its `TypeName`/`FieldName` are template
/// literals, so the resolver→field link is itself a fact). A resolver whose key
/// names no field, or whose type is non-root, produces **no edge** (counted).
#[allow(clippy::too_many_arguments)]
fn link_produces(
    g: &mut Graph,
    resolver: &InfraResource,
    repo_name: &str,
    template_path: &str,
    by_id: &BTreeMap<&str, &InfraResource>,
    gql_fields: &BTreeMap<String, Uid>,
    cov: &mut InfraLinkCoverage,
) {
    // Only a root `TypeName` + a `FieldName` form a money-link candidate.
    let (Some(type_name), Some(field_name)) = (&resolver.type_name, &resolver.field_name) else {
        return;
    };
    if !is_root_type(type_name) {
        // A non-root resolver (a field resolver on `User`, say) is not a money
        // link: there is no root `GraphqlField` to point at. Not a candidate.
        return;
    }
    cov.resolvers_total += 1;

    let key = format!("{type_name}.{field_name}");
    let Some(field_uid) = gql_fields.get(&key) else {
        // The resolver implements a field no schema declares — no edge, surfaced.
        cov.resolvers_unlinked += 1;
        return;
    };

    // Source-node + provenance/confidence selection from the chain grading.
    let (src, prov, conf) = produces_source(g, resolver, repo_name, template_path, by_id);

    g.add_edge(Edge {
        src,
        dst: field_uid.clone(),
        kind: EdgeKind::Produces,
        provenance: prov,
        confidence: Confidence::new(conf),
    });
    cov.resolvers_linked += 1;
}

/// Select the `PRODUCES` source node, provenance, and confidence for a resolver's
/// chain.
///
/// Walk resolver —`data_source_ref`→ datasource —`lambda_ref`→ lambda. When BOTH
/// hops are `Resource`-graded AND the lambda exists as a node, the chain is a
/// fact: the source is the **Lambda** (so `impact(field)` surfaces the
/// implementing function) at `Extracted` 0.95 — or `Inferred` 0.70 if either hop
/// was `Inferred`-graded. When the chain breaks (an `Unresolved` hop, a
/// non-datasource/non-lambda target, a missing node), fall back to the **resolver
/// node** at `Extracted` 0.95 (its `TypeName`/`FieldName` are still template
/// literals — the field IS implemented here, we just cannot name the Lambda).
fn produces_source(
    g: &Graph,
    resolver: &InfraResource,
    repo_name: &str,
    template_path: &str,
    by_id: &BTreeMap<&str, &InfraResource>,
) -> (Uid, Provenance, f32) {
    let resolver_uid = infra_uid(repo_name, template_path, &resolver.logical_id);
    let resolver_fallback = (resolver_uid, Provenance::Extracted, CONF_PRODUCES_EXTRACTED);

    // resolver —Routes→ datasource (must be an AppSync data source resource).
    let Some((ds_id, ds_inferred)) = resolve_ref_id(resolver.data_source_ref.as_ref()) else {
        return resolver_fallback;
    };
    let Some(ds) = by_id.get(ds_id) else {
        return resolver_fallback;
    };
    if ds.kind != InfraKind::AppSyncDataSource {
        return resolver_fallback;
    }

    // datasource —Routes→ lambda (must be a Lambda function resource that exists
    // as a node).
    let Some((lambda_id, lambda_inferred)) = resolve_ref_id(ds.lambda_ref.as_ref()) else {
        return resolver_fallback;
    };
    let Some(lambda) = by_id.get(lambda_id) else {
        return resolver_fallback;
    };
    if lambda.kind != InfraKind::LambdaFunction {
        return resolver_fallback;
    }
    let lambda_uid = infra_uid(repo_name, template_path, lambda_id);
    if g.get_node(&lambda_uid).is_none() {
        return resolver_fallback;
    }

    // The chain resolves end-to-end. The Lambda sources the edge; an `Inferred`
    // hop anywhere in the chain downgrades it (the weakest hop dominates).
    if ds_inferred || lambda_inferred {
        (lambda_uid, Provenance::Inferred, CONF_PRODUCES_INFERRED)
    } else {
        (lambda_uid, Provenance::Extracted, CONF_PRODUCES_EXTRACTED)
    }
}

/// Resolve a graded reference to `(logical_id, is_inferred)` when it names a
/// SINGLE resource (a `Resource` or `Inferred` grade), or `None` otherwise —
/// `Unresolved`, `Literal`, **or `InferredMulti`** (a `Fn::If` over distinct
/// targets cannot uniquely resolve a chain). `is_inferred` is true for an
/// `Inferred` grade so the caller can downgrade an otherwise-fact chain.
///
/// Used by the money-link chain walk ([`produces_source`]), which needs ONE
/// concrete datasource/lambda — so an `InferredMulti` hop honestly breaks the
/// chain to the resolver fallback. Edge emission ([`add_ref_edge`]) uses
/// [`ref_edge_targets`] instead, which fans out over an `InferredMulti`.
fn resolve_ref_id(reference: Option<&RefValue>) -> Option<(&str, bool)> {
    match reference {
        Some(RefValue::Resource(id)) => Some((id.as_str(), false)),
        Some(RefValue::Inferred(id)) => Some((id.as_str(), true)),
        // Resource/Inferred only: a multi-target Fn::If is not a unique chain hop.
        _ => None,
    }
}

/// The edge target(s) a graded reference names, as `(logical_id, is_inferred)`
/// pairs, for edge emission. Unlike [`resolve_ref_id`] this fans out over an
/// [`RefValue::InferredMulti`] (a `Fn::If` over distinct branch targets), yielding
/// one Inferred target per id — so the builder emits an edge per possible
/// deployment (Slice 10, B2). `Unresolved`/`Literal`/absent → empty.
fn ref_edge_targets(reference: Option<&RefValue>) -> Vec<(&str, bool)> {
    match reference {
        Some(RefValue::Resource(id)) => vec![(id.as_str(), false)],
        Some(RefValue::Inferred(id)) => vec![(id.as_str(), true)],
        Some(RefValue::InferredMulti(ids)) => ids.iter().map(|id| (id.as_str(), true)).collect(),
        _ => Vec::new(),
    }
}

/// The `Runs` bridge: a Lambda → the code `Module` its handler resolves to.
///
/// Candidate paths = the template file's directory joined with the `CodeUri`
/// directory joined with the `Handler` file stem across [`HANDLER_EXTS`] (plus
/// `index.*` when the stem is `index`). **SAM semantics: `CodeUri` is relative
/// to the TEMPLATE FILE's directory, not the repo root** — a template at
/// `backend/template.yaml` with `CodeUri: functions/op/` resolves to
/// `backend/functions/op/…` (dogfood regression: every Lambda
/// in a non-root template counted handler-unresolved before this). Exactly one
/// indexed module matching any candidate → a `Runs` edge at `Extracted` 0.95.
/// Zero or several matches → no edge, counted in `lambdas_handler_unresolved`.
/// A Lambda with no `Handler` is not a `Runs` candidate at all (nothing to
/// resolve) and is not counted either way.
///
/// The matched module's `Module` node is created by whichever language plane
/// indexed the file (TS or Python), so the `Runs` edge target UID must carry the
/// *matched file's* language tag — a `.py`/`.pyi` handler links to the `py`-tagged
/// module node `assemble_python` created, a TS/JS handler to the `ts`-tagged one.
fn link_runs(
    g: &mut Graph,
    lambda_uid: &Uid,
    res: &InfraResource,
    repo_name: &str,
    template_path: &str,
    module_paths: &BTreeSet<&str>,
    cov: &mut InfraLinkCoverage,
) {
    let Some(handler) = res.handler.as_deref() else {
        return; // no handler to resolve — not a Runs candidate.
    };
    let candidates = handler_candidate_paths(template_path, res.code_uri.as_deref(), handler);
    let matched: Vec<&str> = module_paths
        .iter()
        .filter(|p| candidates.contains(**p))
        .copied()
        .collect();

    if matched.len() == 1 {
        let module_uid = module_uid_for(repo_name, matched[0]);
        g.add_edge(Edge {
            src: lambda_uid.clone(),
            dst: module_uid,
            kind: EdgeKind::Runs,
            provenance: Provenance::Extracted,
            confidence: Confidence::new(CONF_RUNS),
        });
        cov.lambdas_runs_linked += 1;
    } else {
        // Zero or ambiguous: never an invented edge (R1). A CodeUri that points
        // outside the indexed tree, a handler in a language with no plane yet, and
        // genuine misses all land here honestly.
        cov.lambdas_handler_unresolved += 1;
    }
}

/// The code `Module` UID for a matched handler path, tagged with the file's
/// language so the `Runs` edge lands on the node the right plane created. A
/// `.py`/`.pyi` file is `py`-tagged (matching `strata_lang_py::assemble_python`);
/// everything else (TS/JS) is `ts`-tagged (matching [`crate::build::uid_module`]).
fn module_uid_for(repo_name: &str, path: &str) -> Uid {
    let is_python = path
        .rsplit('.')
        .next()
        .map(|ext| ext == "py" || ext == "pyi")
        .unwrap_or(false);
    if is_python {
        Uid::new("py", repo_name, path, "<module>", "")
    } else {
        uid_module(repo_name, path)
    }
}

/// The normalized repo-relative candidate module paths for a Lambda handler.
///
/// `Handler: app.handler` → stem `app` → `{codeuri_dir}/app.{ext}` for every
/// [`HANDLER_EXTS`] extension; when the stem is `index`, that is already covered
/// (the stem IS `index`). `CodeUri: src/handlers/` joins as the directory; a
/// missing `CodeUri` joins at the repo root. The result is a `BTreeSet` so
/// membership tests are cheap and order-independent.
fn handler_candidate_paths(
    template_path: &str,
    code_uri: Option<&str>,
    handler: &str,
) -> BTreeSet<String> {
    // The handler stem is the part before the LAST `.` (`app.handler` → `app`,
    // `nested/app.lambda_handler` → `nested/app`). A handler with no `.` (rare)
    // is taken whole.
    let stem = match handler.rfind('.') {
        Some(idx) => &handler[..idx],
        None => handler,
    };

    // SAM resolves `CodeUri` relative to the template file's directory, so the
    // candidate base is `<template dir>/<code uri>` (each part may be empty —
    // a root-level template contributes no prefix).
    let template_dir = match template_path.rfind('/') {
        Some(idx) => &template_path[..idx],
        None => "",
    };
    let code_dir = code_uri.map(normalize_dir).unwrap_or_default();
    let base = if code_dir.is_empty() {
        template_dir.to_string()
    } else {
        join_repo_rel(template_dir, &code_dir)
    };
    let mut out = BTreeSet::new();
    for ext in HANDLER_EXTS {
        out.insert(join_repo_rel(&base, &format!("{stem}.{ext}")));
    }
    out
}

/// Normalize a `CodeUri` to a clean directory prefix: trim a leading `./`, trim a
/// trailing `/`, and drop a `.` (the repo root). `src/handlers/` → `src/handlers`;
/// `./functions/x/` → `functions/x`; `.` / `./` → `` (root).
fn normalize_dir(code_uri: &str) -> String {
    let trimmed = code_uri.trim();
    let no_dot_slash = trimmed.strip_prefix("./").unwrap_or(trimmed);
    let no_trailing = no_dot_slash.trim_end_matches('/');
    if no_trailing == "." {
        String::new()
    } else {
        no_trailing.to_string()
    }
}

/// Join a (possibly empty) directory prefix with a file name into a `/`-normalized
/// repo-relative path. An empty prefix yields the bare file name (repo root).
fn join_repo_rel(dir: &str, file: &str) -> String {
    if dir.is_empty() {
        file.to_string()
    } else {
        format!("{dir}/{file}")
    }
}

/// Whether a resolver `TypeName` is a GraphQL root operation type — the only
/// types that have a corresponding `GraphqlField` operation node to link to.
fn is_root_type(type_name: &str) -> bool {
    matches!(type_name, "Query" | "Mutation" | "Subscription")
}

/// Add graded reference edge(s) `src —kind→ <referenced node>` for the same-
/// template resource(s) a reference names that exist as nodes:
/// - [`RefValue::Resource`] → one `Extracted` 0.95 edge.
/// - [`RefValue::Inferred`] → one `Inferred` 0.70 edge (when the id is a node).
/// - [`RefValue::InferredMulti`] → one `Inferred` 0.70 edge PER distinct branch
///   target (Slice 10, B2: both possible `Fn::If` deployments surfaced).
/// - [`RefValue::Unresolved`] / [`RefValue::Literal`] / a missing node → no edge.
///
/// Never an edge to a phantom node, never an invented target (R1).
fn add_ref_edge(
    g: &mut Graph,
    src: &Uid,
    reference: Option<&RefValue>,
    repo_name: &str,
    template_path: &str,
    kind: EdgeKind,
) {
    for (id, is_inferred) in ref_edge_targets(reference) {
        let dst = infra_uid(repo_name, template_path, id);
        if g.get_node(&dst).is_none() {
            // The grader only emits a resource id for a same-template id, so the
            // node should exist; guard anyway so a stray reference cannot invent
            // one. A missing branch target is skipped, the others still emit.
            continue;
        }
        let (prov, conf) = if is_inferred {
            (Provenance::Inferred, CONF_REF_INFERRED)
        } else {
            (Provenance::Extracted, CONF_REF_RESOURCE)
        };
        g.add_edge(Edge {
            src: src.clone(),
            dst,
            kind,
            provenance: prov,
            confidence: Confidence::new(conf),
        });
    }
}

/// Add the containment edge `<api> —Contains→ member` for a resolver/datasource's
/// `ApiId` reference (Slice 10, B3). The reference is the resolver/datasource
/// pointing AT its API, so the edge is the REVERSE: the API contains the member.
/// Confidence follows the ref grade (Resource → Extracted 0.95, Inferred → 0.70;
/// an `InferredMulti` `Fn::If` over distinct APIs adds one edge per API). It is a
/// membership edge, not a dependency — `impact` never traverses it.
///
/// Never an edge from a phantom API node, never an invented target (R1).
fn add_contains_edge(
    g: &mut Graph,
    member: &Uid,
    api_ref: Option<&RefValue>,
    repo_name: &str,
    template_path: &str,
) {
    for (api_id, is_inferred) in ref_edge_targets(api_ref) {
        let api_uid = infra_uid(repo_name, template_path, api_id);
        if g.get_node(&api_uid).is_none() {
            continue; // the ApiId names no same-template node — no edge, surfaced.
        }
        let (prov, conf) = if is_inferred {
            (Provenance::Inferred, CONF_REF_INFERRED)
        } else {
            (Provenance::Extracted, CONF_REF_RESOURCE)
        };
        g.add_edge(Edge {
            src: api_uid,
            dst: member.clone(),
            kind: EdgeKind::Contains,
            provenance: prov,
            confidence: Confidence::new(conf),
        });
    }
}

/// The infra-plane node for one resource: `Extracted` 1.0, `name` = `fqn` =
/// logical id, `path` = template path, kind mapped from [`InfraKind`].
fn infra_node(repo_name: &str, template_path: &str, res: &InfraResource) -> Node {
    Node {
        uid: infra_uid(repo_name, template_path, &res.logical_id),
        kind: node_kind_for(res.kind),
        name: res.logical_id.clone(),
        fqn: res.logical_id.clone(),
        path: template_path.to_string(),
        span: Span::default(),
        provenance: Provenance::Extracted,
        confidence: Confidence::new(1.0),
    }
}

/// The UID of an infra resource node: `infra | repo | template_path | logical_id |`.
fn infra_uid(repo_name: &str, template_path: &str, logical_id: &str) -> Uid {
    Uid::new(INFRA_LANG, repo_name, template_path, logical_id, "")
}

/// Map an [`InfraKind`] to its infra-plane [`NodeKind`]. `Generic` resources are
/// inventory-only [`CloudResource`](NodeKind::CloudResource)s.
fn node_kind_for(kind: InfraKind) -> NodeKind {
    match kind {
        InfraKind::LambdaFunction => NodeKind::LambdaFn,
        InfraKind::IamRole => NodeKind::IamRole,
        InfraKind::AppSyncApi => NodeKind::AppSyncApi,
        InfraKind::AppSyncResolver => NodeKind::AppSyncResolver,
        InfraKind::AppSyncDataSource => NodeKind::AppSyncDataSource,
        // An IamPolicy never becomes a node (its grants attach to roles); mapped
        // here only for match exhaustiveness — pass 1 skips node creation for it.
        InfraKind::IamPolicy => NodeKind::CloudResource,
        InfraKind::Generic => NodeKind::CloudResource,
    }
}

/// The shared `CloudAction` node UID for an IAM action string within a repo
/// (`iam | repo | "" | action |`). Both `Grants` (role→action) and
/// `RequiresPermission` (code→action) point at this node, so reconciliation is a
/// graph lookup; wildcard expansion is the permission-gap traversal's job.
fn cloud_action_uid(repo_name: &str, action: &str) -> Uid {
    Uid::new("iam", repo_name, "", action, "")
}

/// Add a `Grants` edge `role_uid → CloudAction(action)`, creating the (idempotent,
/// UID-keyed) `CloudAction` node. `action` is a concrete action (`dynamodb:PutItem`),
/// a wildcard (`dynamodb:*` / `*`), or an `<opaque:reason>` indeterminacy marker.
fn add_grant(g: &mut Graph, repo_name: &str, role_uid: &Uid, action: &str) {
    let action_uid = cloud_action_uid(repo_name, action);
    // Idempotent: a role may be granted the same action by several policies,
    // overlapping SAM templates, or repeated inline blocks. The CloudAction node is
    // UID-keyed (so `add_node` is already idempotent), but `add_edge` does not dedup,
    // so guard against a duplicate Grants edge — a role's grant set is a set.
    let already_granted = g
        .neighbors(role_uid, Direction::Outgoing, &[EdgeKind::Grants])
        .iter()
        .any(|(_, n)| n.uid == action_uid);
    if already_granted {
        return;
    }
    g.add_node(Node {
        uid: action_uid.clone(),
        kind: NodeKind::CloudAction,
        name: action.to_string(),
        fqn: action.to_string(),
        path: String::new(),
        span: Span::default(),
        provenance: Provenance::Extracted,
        confidence: Confidence::new(1.0),
    });
    g.add_edge(Edge {
        src: role_uid.clone(),
        dst: action_uid,
        kind: EdgeKind::Grants,
        provenance: Provenance::Extracted,
        confidence: Confidence::new(1.0),
    });
}

// ── Terragrunt structural plane (Track D1, Slice 14, M2) ─────────────────────

/// A structural Terragrunt dependency edge is a config *fact* (the literal
/// `config_path` resolves to a directory that IS another unit in the repo) →
/// `Extracted` 0.95, the EXTRACTED band floor. The cross-unit *attribute* wiring
/// (`dependency.x.outputs.*`) is NOT evaluated, so it stays absent — surfaced by
/// the edge's coarseness, never invented.
const CONF_TG_DEP: f32 = 0.95;

/// Per-repo Terragrunt structural coverage: how many units were detected and how
/// many of their declared dependencies resolved to a known same-repo unit (vs.
/// pointed outside the indexed tree).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TerragruntCoverage {
    /// `terragrunt.hcl` units detected and extracted.
    pub units_detected: usize,
    /// Declared (literal-`config_path`) dependencies across all units.
    pub deps_total: usize,
    /// Dependencies whose resolved `config_path` named another detected unit → a
    /// structural `Routes` edge was added.
    pub deps_linked: usize,
    /// Dependencies whose `config_path` resolved to no detected unit (a unit
    /// outside the indexed tree, or a typo'd path) → **no** edge. Surfaced, never
    /// invented.
    pub deps_unresolved: usize,
}

/// Add the Terragrunt structural plane to `g`: one [`CloudResource`] node per
/// detected unit (keyed by the unit's directory), and a `Routes` edge per
/// dependency whose literal `config_path` resolves to ANOTHER detected unit
/// (Extracted 0.95). Pure and deterministic over `units` (path order is the
/// caller's responsibility); idempotent by UID.
///
/// **Honest bound:** only the structural unit→unit dependency is stated. Terragrunt
/// `dependency.<name>.outputs.*` attribute wiring is NOT evaluated (that is
/// reimplementing Terragrunt), so a unit's resources are not cross-wired to the
/// depended-on unit's resources — surfaced by the absence of those edges.
pub fn build_terragrunt_plane(
    g: &mut Graph,
    repo_name: &str,
    units: &[TerragruntUnit],
) -> TerragruntCoverage {
    let mut cov = TerragruntCoverage::default();

    // unit directory → its terragrunt.hcl path, the existence oracle for resolving
    // a dependency's `config_path` to a known unit (and its node UID).
    let unit_dirs: BTreeMap<String, &str> = units
        .iter()
        .map(|u| (unit_dir(&u.path), u.path.as_str()))
        .collect();

    // ── Pass 1: a node per unit (Extracted 1.0). Idempotent by UID. ──
    cov.units_detected = units.len();
    for unit in units {
        let dir = unit_dir(&unit.path);
        g.add_node(terragrunt_node(repo_name, unit, &dir));
    }

    // ── Pass 2: structural dependency edges between units. ──
    for unit in units {
        let from_dir = unit_dir(&unit.path);
        let src = terragrunt_uid(repo_name, &unit.path, &from_dir);
        for dep in &unit.dependencies {
            cov.deps_total += 1;
            // Resolve the relative `config_path` against the unit's own directory.
            let target_dir = resolve_rel_dir(&from_dir, &dep.config_path);
            match unit_dirs.get(&target_dir) {
                Some(target_path) => {
                    let dst = terragrunt_uid(repo_name, target_path, &target_dir);
                    g.add_edge(Edge {
                        src: src.clone(),
                        dst,
                        kind: EdgeKind::Routes,
                        provenance: Provenance::Extracted,
                        confidence: Confidence::new(CONF_TG_DEP),
                    });
                    cov.deps_linked += 1;
                }
                // The config_path points outside the indexed unit set — no edge,
                // surfaced honestly (never an edge to a phantom unit).
                None => cov.deps_unresolved += 1,
            }
        }
    }

    cov
}

/// The infra-plane node for one Terragrunt unit: a [`CloudResource`] whose name/fqn
/// is the unit directory, `path` the `terragrunt.hcl`, `Extracted` 1.0.
fn terragrunt_node(repo_name: &str, unit: &TerragruntUnit, dir: &str) -> Node {
    Node {
        uid: terragrunt_uid(repo_name, &unit.path, dir),
        kind: NodeKind::CloudResource,
        name: dir.to_string(),
        fqn: dir.to_string(),
        path: unit.path.clone(),
        span: Span::default(),
        provenance: Provenance::Extracted,
        confidence: Confidence::new(1.0),
    }
}

/// The UID of a Terragrunt unit node: `infra | repo | <terragrunt.hcl path> | <unit
/// dir> |`. The unit dir is the logical id so a dependency edge can be addressed by
/// the target's directory.
fn terragrunt_uid(repo_name: &str, unit_path: &str, dir: &str) -> Uid {
    Uid::new(INFRA_LANG, repo_name, unit_path, dir, "")
}

/// The directory of a `terragrunt.hcl` path (`infra/prod/app/terragrunt.hcl` →
/// `infra/prod/app`). A root-level `terragrunt.hcl` yields `"."` so a sibling
/// `config_path = "../x"` still resolves.
fn unit_dir(unit_path: &str) -> String {
    match unit_path.rfind('/') {
        Some(idx) => unit_path[..idx].to_string(),
        None => ".".to_string(),
    }
}

/// Resolve a relative `config_path` (`../vpc`, `../../shared/db`, `./x`) against a
/// base directory into a normalized repo-relative unit directory. Pure string path
/// arithmetic (no IO): splits on `/`, applies `.`/`..` against the base components.
/// A path that climbs above the repo root collapses to as far as it can (the
/// resulting dir simply won't match any unit — surfaced as unresolved).
fn resolve_rel_dir(base_dir: &str, config_path: &str) -> String {
    // Start from the base directory's components (dropping a bare "." base).
    let mut stack: Vec<&str> = if base_dir == "." || base_dir.is_empty() {
        Vec::new()
    } else {
        base_dir.split('/').collect()
    };
    for part in config_path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            other => stack.push(other),
        }
    }
    if stack.is_empty() {
        ".".to_string()
    } else {
        stack.join("/")
    }
}
