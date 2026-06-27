//! THE infra estate headline test (Slice 5, M2 — Definition of Done test 7):
//! cross-repo infrastructure blast radius. The schema + AppSync template live in
//! repo-a; a gql consumer lives in repo-b. `impact(Query.getUser)` over the estate
//! graph built by `link_estate` MUST surface BOTH the implementing **Lambda**
//! (repo-a, via the infra `PRODUCES` re-pointed to the canonical field node) AND
//! the gql **consumer** (repo-b, via the cross-repo `CONSUMES`). Contract-free
//! impact reaches neither — proving the reach spans the infra → contract →
//! consumer planes, not a code edge.
//!
//! This is the estate proof that the per-repo money link (test 3) survives the
//! `link_estate` dedup/re-point: the infra-sourced `PRODUCES` edge's dst (the
//! repo-local `GraphqlField` UID) is re-pointed to the canonical node exactly like
//! a code-sourced producer, so the Lambda still reaches the canonical field.

use std::path::{Path, PathBuf};

use strata_core::{impact, ImpactOptions, NodeKind, Uid};
use strata_index::{index_estate, link_estate, ResolveMode, WorkspaceManifest};

const ESTATE: &str = "infra-estate";

fn fixture_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".strata" {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        if entry.file_type()?.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// The canonical (estate-wide) GraphQL field UID under the api-scoped identity
/// (B6 fix): `contract | estate | {api_id}/graphql | key |`. The schema lives in
/// repo-a, which declares no api id → api_id defaults to the repo name `repo-a`.
fn canonical_gql_uid(key: &str) -> Uid {
    Uid::new("contract", ESTATE, "repo-a/graphql", key, "")
}

/// repo-a's infra Lambda node UID.
fn lambda_uid() -> Uid {
    Uid::new("infra", "repo-a", "template.yaml", "UserFunction", "")
}

/// repo-b's gql consumer function UID.
fn consumer_uid() -> Uid {
    Uid::new("ts", "repo-b", "src/queries.ts", "loadUser", "")
}

#[test]
fn impact_on_field_reaches_lambda_in_repo_a_and_consumer_in_repo_b() {
    // ── Arrange: index + link the 2-repo infra estate. ──
    let tmp = tempfile::tempdir().expect("tempdir");
    copy_dir_all(&fixture_dir("crossrepo_infra"), tmp.path()).expect("copy fixture");
    let manifest_path = tmp.path().join("strata.workspace.toml");
    let manifest = WorkspaceManifest::parse_file(&manifest_path).expect("manifest parses");
    index_estate(&manifest, &manifest_path, ResolveMode::Off);
    let (estate, _coverage, results) = link_estate(&manifest, tmp.path());
    assert!(
        results.iter().all(|r| r.ok),
        "both repos link ok: {results:?}"
    );

    // The impact target is the CANONICAL Query.getUser GraphqlField node (the
    // estate-deduped node both repos' edges were re-pointed onto).
    let field = canonical_gql_uid("Query.getUser");
    assert_eq!(
        estate.get_node(&field).map(|n| n.kind),
        Some(NodeKind::GraphqlField),
        "the canonical Query.getUser GraphqlField node must exist in the estate"
    );

    // The Lambda node from repo-a's template must survive into the estate.
    let lambda = lambda_uid();
    assert!(
        estate.get_node(&lambda).is_some(),
        "repo-a's UserFunction Lambda node must exist in the estate graph"
    );

    // ── Act: blast radius over the estate graph (contract-aware by default). ──
    let r = impact(&estate, &field, &ImpactOptions::default());
    let affected: Vec<&str> = r.affected.iter().map(|a| a.uid.as_str()).collect();

    // ── Assert: the Lambda (repo-a) is reached via the re-pointed infra PRODUCES. ──
    let l = r.affected.iter().find(|a| a.uid == lambda).unwrap_or_else(|| {
        panic!("impact(Query.getUser) MUST include repo-a's Lambda (infra PRODUCES re-pointed to the canonical field): {affected:?}")
    });
    assert!(
        (l.confidence - 0.95).abs() < 1e-5,
        "Lambda reach conf = infra PRODUCES Extracted 0.95, got {}",
        l.confidence
    );

    // ── Assert: the gql consumer (repo-b) is reached via the cross-repo CONSUMES. ──
    let consumer = consumer_uid();
    let c = r.affected.iter().find(|a| a.uid == consumer).unwrap_or_else(|| {
        panic!("impact(Query.getUser) MUST include repo-b's gql consumer loadUser (cross-repo CONSUMES): {affected:?}")
    });
    assert!(
        (c.confidence - 0.95).abs() < 1e-5,
        "consumer reach conf = CONSUMES Extracted 0.95, got {}",
        c.confidence
    );

    // The Lambda is in repo-a and the consumer in repo-b — make the cross-repo
    // nature explicit so the test cannot pass on a same-repo edge.
    assert!(
        lambda.as_str().contains("repo-a") && consumer.as_str().contains("repo-b"),
        "the Lambda and the consumer must be in different repos"
    );

    // ── Sanity: contract-free impact reaches NEITHER (the field has no incoming
    // CALLS), proving the reach is via the infra/contract planes. ──
    let code_only = impact(
        &estate,
        &field,
        &ImpactOptions {
            include_contracts: false,
            ..ImpactOptions::default()
        },
    );
    assert!(
        !code_only.affected.iter().any(|a| a.uid == lambda)
            && !code_only.affected.iter().any(|a| a.uid == consumer),
        "without the contract hop the field target reaches neither the Lambda nor the \
         cross-repo consumer (proves the links are the infra/contract planes)"
    );
}
