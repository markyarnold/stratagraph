//! THE headline test (Slice 3, M3 — Definition of Done test 4): cross-repo
//! blast radius. `impact(<producer handler in repo A>)` over the estate graph
//! built by `link_estate` MUST include the consumer function in repo B that
//! calls the same operation — the whole point of the contract plane.
//!
//! Copies the committed `crossrepo` fixture estate to a tempdir, indexes each
//! repo (`ResolveMode::Off`), links the estate, and asserts the target node
//! identity of the cross-repo consumer in the impact result.

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
fn impact_of_producer_handler_reaches_consumer_in_other_repo() {
    // ── Arrange: index + link the 2-repo estate. ──
    let tmp = tempfile::tempdir().expect("tempdir");
    copy_dir_all(&fixture_dir("crossrepo"), tmp.path()).expect("copy fixture");
    let manifest_path = tmp.path().join("strata.workspace.toml");
    let manifest = WorkspaceManifest::parse_file(&manifest_path).expect("manifest parses");
    index_estate(&manifest, &manifest_path, ResolveMode::Off);
    let (estate, _coverage, results) = link_estate(&manifest, tmp.path());
    assert!(
        results.iter().all(|r| r.ok),
        "both repos link ok: {results:?}"
    );

    // The PRODUCER handler in repo A.
    let producer = fn_uid("repo-producer", "src/handlers.ts", "getUser");
    assert!(
        estate.get_node(&producer).is_some(),
        "producer handler node must exist in the estate graph"
    );

    // ── Act: blast radius over the estate graph (contract-aware by default). ──
    let result = impact(&estate, &producer, &ImpactOptions::default());
    let affected: Vec<&str> = result.affected.iter().map(|a| a.uid.as_str()).collect();

    // ── Assert: the CONSUMER functions in repo B are affected — by node identity. ──
    let literal_consumer = fn_uid("repo-consumer", "src/api.ts", "fetchUserProfile");
    let name_consumer = fn_uid("repo-consumer", "src/api.ts", "getUserViaClient");

    assert!(
        result.affected.iter().any(|a| a.uid == literal_consumer),
        "impact(producer in repo A) MUST include the literal-URL consumer in repo B \
         (cross-repo blast radius). Affected: {affected:?}"
    );
    assert!(
        result.affected.iter().any(|a| a.uid == name_consumer),
        "impact(producer in repo A) MUST include the operationId-name consumer in repo B. \
         Affected: {affected:?}"
    );

    // The cross-repo reach is honestly confidence-weighted (Inferred, < 1.0):
    // produce(0.80) × consume(0.70) = 0.56 for the literal consumer.
    let lit = result
        .affected
        .iter()
        .find(|a| a.uid == literal_consumer)
        .unwrap();
    assert!(
        (lit.confidence - 0.56).abs() < 1e-5,
        "literal consumer reach conf = 0.80 × 0.70 = 0.56, got {}",
        lit.confidence
    );
    assert!(
        !lit.ambiguous,
        "this cross-repo path is Inferred end-to-end, not ambiguous"
    );

    // The producer handler resides in a DIFFERENT repo than the consumers — make
    // the cross-repo nature explicit so the test cannot pass on a same-repo edge.
    assert!(
        producer.as_str().contains("repo-producer")
            && literal_consumer.as_str().contains("repo-consumer"),
        "producer and consumer must be in different repos"
    );

    // The widget consumer (undeclared endpoint) is NOT reached — no false edge.
    let widget = fn_uid("repo-consumer", "src/api.ts", "fetchWidget");
    assert!(
        !result.affected.iter().any(|a| a.uid == widget),
        "a consumer of an undeclared endpoint must NOT appear in the blast radius"
    );

    // Sanity: a contract-free reverse-CALLS-only impact would find NOTHING (the
    // handler has no in-repo callers), proving the reach is via the contract plane.
    let code_only = impact(
        &estate,
        &producer,
        &ImpactOptions {
            include_contracts: false,
            ..ImpactOptions::default()
        },
    );
    assert!(
        !code_only.affected.iter().any(|a| a.uid == literal_consumer),
        "without the contract hop the cross-repo consumer is unreachable (proves \
         the link is the contract plane, not a code edge)"
    );
}
