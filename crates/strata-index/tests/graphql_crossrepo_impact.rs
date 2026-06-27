//! THE GraphQL headline test (Slice 4, M2 — Definition of Done test 6):
//! cross-repo GraphQL blast radius. `impact(<resolver handler in repo-schema>)`
//! over the estate graph built by `link_estate` MUST include the gql consumer
//! function in repo-app that queries the same field — by node identity, at the
//! honest produce×consume confidence (0.80 × 0.95 = 0.76). Contract-free impact
//! does NOT reach it (proving the reach is the contract plane, not a code edge).

use std::path::{Path, PathBuf};

use strata_core::{impact, ImpactOptions, Uid};
use strata_index::{index_estate, link_estate, ResolveMode, WorkspaceManifest};

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

/// repo/path/fqn code-plane function UID.
fn fn_uid(repo: &str, path: &str, fqn: &str) -> Uid {
    Uid::new("ts", repo, path, fqn, "")
}

#[test]
fn impact_of_graphql_resolver_reaches_gql_consumer_in_other_repo() {
    // ── Arrange: index + link the 2-repo GraphQL estate. ──
    let tmp = tempfile::tempdir().expect("tempdir");
    copy_dir_all(&fixture_dir("crossrepo_graphql"), tmp.path()).expect("copy fixture");
    let manifest_path = tmp.path().join("strata.workspace.toml");
    let manifest = WorkspaceManifest::parse_file(&manifest_path).expect("manifest parses");
    index_estate(&manifest, &manifest_path, ResolveMode::Off);
    let (estate, _coverage, results) = link_estate(&manifest, tmp.path());
    assert!(
        results.iter().all(|r| r.ok),
        "both repos link ok: {results:?}"
    );

    // The PRODUCER: the `getUser` resolver handler in repo-schema.
    let producer = fn_uid("repo-schema", "src/resolvers.ts", "getUser");
    assert!(
        estate.get_node(&producer).is_some(),
        "the getUser resolver handler node must exist in the estate graph"
    );

    // ── Act: blast radius over the estate graph (contract-aware by default). ──
    let result = impact(&estate, &producer, &ImpactOptions::default());
    let affected: Vec<&str> = result.affected.iter().map(|a| a.uid.as_str()).collect();

    // ── Assert: the gql CONSUMER in repo-app is affected — by node identity. ──
    let consumer = fn_uid("repo-app", "src/queries.ts", "loadUserProfile");
    assert!(
        result.affected.iter().any(|a| a.uid == consumer),
        "impact(getUser resolver in repo-schema) MUST include the gql consumer in \
         repo-app (cross-repo GraphQL blast radius). Affected: {affected:?}"
    );

    // The cross-repo reach is honestly confidence-weighted: produce(0.80) ×
    // consume(0.95) = 0.76 (the GraphQL CONSUMES is Extracted, not Inferred).
    let c = result.affected.iter().find(|a| a.uid == consumer).unwrap();
    assert!(
        (c.confidence - 0.76).abs() < 1e-5,
        "consumer reach conf = 0.80 × 0.95 = 0.76, got {}",
        c.confidence
    );
    assert!(
        !c.ambiguous,
        "this cross-repo GraphQL path is Inferred×Extracted end-to-end, not ambiguous"
    );

    // Producer and consumer are in DIFFERENT repos — make the cross-repo nature
    // explicit so the test cannot pass on a same-repo edge.
    assert!(
        producer.as_str().contains("repo-schema") && consumer.as_str().contains("repo-app"),
        "producer and consumer must be in different repos"
    );

    // The `nonExistentField` consumer (undeclared field) is NOT reached — no
    // false edge.
    let unknown = fn_uid("repo-app", "src/unknown.ts", "loadUnknown");
    assert!(
        !result.affected.iter().any(|a| a.uid == unknown),
        "a consumer of an undeclared GraphQL field must NOT appear in the blast radius"
    );

    // ── Sanity: contract-free impact finds NOTHING (the resolver has no in-repo
    // callers), proving the reach is via the contract plane. ──
    let code_only = impact(
        &estate,
        &producer,
        &ImpactOptions {
            include_contracts: false,
            ..ImpactOptions::default()
        },
    );
    assert!(
        !code_only.affected.iter().any(|a| a.uid == consumer),
        "without the contract hop the cross-repo gql consumer is unreachable (proves \
         the link is the contract plane, not a code edge)"
    );
}
