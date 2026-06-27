//! Bundled-vendor pruning (Track D follow-up): files that are *committed* parts of
//! a third-party dependency bundle — the classic AWS Lambda anti-pattern of
//! `pip install -t .` vendoring `botocore`/`s3transfer`/`boto3` right next to
//! first-party handler code — are NEVER indexed. They otherwise inflate the graph
//! with third-party code and bloat the `rename` tool's "implicated files" set.
//!
//! The signal is a sibling `*.dist-info` metadata directory; the exact set of
//! vendored FILES is read from its `RECORD` (so `python_dateutil-*.dist-info`
//! correctly prunes the `dateutil/` files it installed, despite the
//! distribution-vs-import name mismatch). The prune is per-FILE, not by directory
//! name: a co-located first-party file the `RECORD` does not list always survives,
//! even one inside a same-named dir (the never-lose-first-party guarantee, Fix A).
//! **All** planes prune identically by that file set — code (TS/Py/C#), Terraform/
//! Terragrunt, AND the contract/infra spec collectors (a vendored CFN/SAM template
//! or OpenAPI/GraphQL/proto spec inside a bundle is pruned too, Fix B). A
//! `.strataignore` file (gitignore syntax) is also honored as a user-controlled
//! escape valve, and `--include-vendored` disables detection wholesale.
//!
//! Honest bound: a genuinely-vendored file ABSENT from its `RECORD` (rare) is not
//! pruned — conservative inflation, never first-party loss.
//!
//! Mirrors the runtime-decoy technique of `mixed_lang.rs`: the bundle is written
//! into a **tempdir at runtime** (a committed fixture could not hold a realistic
//! dist-info tree), then indexed — exercising the indexer's own pruning, not
//! `.gitignore`.

use std::fs;
use std::path::Path;

use strata_index::index_repo;
use strata_store::{DuckGraphStore, GraphStore};

/// Write a `.py` defining `def <sym>(): pass` at repo-relative `rel` under `root`.
fn write_py(root: &Path, rel: &str, sym: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, format!("def {sym}():\n    pass\n")).unwrap();
}

/// Write a dist-info `RECORD` (only the first CSV field — the installed path —
/// matters to the detector) at repo-relative `rel` under `root`.
fn write_record(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, body).unwrap();
}

/// Write arbitrary `body` at repo-relative `rel` under `root` (for spec/template
/// fixtures that are not `.py`).
fn write_file(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, body).unwrap();
}

/// A minimal CFN/SAM template declaring one Lambda whose logical id is `logical_id`.
/// `CfnSamAdapter::detects` keys on the top-level `Resources` map of `AWS::…` types,
/// and the infra plane names the resulting node by its logical id — so the logical
/// id appears as a graph node name iff the template was indexed.
fn cfn_template(logical_id: &str) -> String {
    format!(
        concat!(
            "Resources:\n",
            "  {0}:\n",
            "    Type: AWS::Serverless::Function\n",
            "    Properties:\n",
            "      Handler: index.handler\n",
            "      Runtime: nodejs18.x\n",
        ),
        logical_id
    )
}

/// A minimal OpenAPI 3 spec declaring one operation whose `operationId` is `op`.
/// The contract plane names the resulting `ApiOperation` node by its operationId —
/// so the operationId appears as a graph node name iff the spec was indexed.
fn openapi_spec(op: &str) -> String {
    format!(
        concat!(
            "openapi: 3.0.0\n",
            "info:\n",
            "  title: t\n",
            "  version: \"1.0\"\n",
            "paths:\n",
            "  /{0}:\n",
            "    get:\n",
            "      operationId: {0}\n",
            "      responses:\n",
            "        \"200\":\n",
            "          description: ok\n",
        ),
        op
    )
}

/// Collect every node name in the graph (a leaked vendored symbol is unmistakable).
fn node_names(g: &strata_core::Graph) -> Vec<String> {
    let mut v = Vec::new();
    for n in g.nodes() {
        v.push(n.name.clone());
    }
    v
}

/// A realistic vendored Lambda bundle next to first-party code:
///
/// ```text
/// app/handler.py                          first-party  — MUST index
/// app/helpers/util.py                     first-party  — MUST index (co-located subpackage)
/// app/s3transfer/__init__.py              vendored     — MUST prune (name == dist-info prefix)
/// app/s3transfer-0.10.0.dist-info/RECORD  marker
/// app/dateutil/__init__.py                vendored     — MUST prune (name != dist-info prefix)
/// app/python_dateutil-2.9.0.dist-info/RECORD marker
/// ```
fn write_dist_info_bundle(root: &Path) {
    // First-party.
    write_py(root, "app/handler.py", "fp_handler_entry");
    // A co-located first-party subpackage in the SAME directory as the vendored
    // deps: it is in NO `RECORD`, so it must NOT be over-pruned.
    write_py(root, "app/helpers/util.py", "fp_helper_util");

    // Vendored: import name matches its distribution / dist-info prefix.
    write_py(root, "app/s3transfer/__init__.py", "vendored_s3transfer_fn");
    write_record(
        root,
        "app/s3transfer-0.10.0.dist-info/RECORD",
        "s3transfer/__init__.py,sha256=aaa,10\ns3transfer-0.10.0.dist-info/RECORD,,\n",
    );

    // Vendored: import name (`dateutil`) differs from the distribution name
    // (`python-dateutil`) — only RECORD knows the link, not the dir name.
    write_py(root, "app/dateutil/__init__.py", "vendored_dateutil_fn");
    write_record(
        root,
        "app/python_dateutil-2.9.0.dist-info/RECORD",
        "dateutil/__init__.py,sha256=bbb,20\npython_dateutil-2.9.0.dist-info/RECORD,,\n",
    );
}

#[test]
fn vendored_dist_info_bundle_is_pruned_first_party_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_dist_info_bundle(root);

    let mut store = DuckGraphStore::open_in_memory().unwrap();
    let stats = index_repo(root, &mut store).unwrap();
    let g = store.load_graph().unwrap();
    let nodes = node_names(&g);

    // First-party is indexed — including the co-located `helpers/` subpackage.
    // That guard is what distinguishes precise RECORD-driven pruning from a coarse
    // "prune every sibling of a vendor root" rule, which would wrongly drop it.
    assert!(
        nodes.iter().any(|n| n == "fp_handler_entry"),
        "first-party handler must be indexed: {nodes:?}"
    );
    assert!(
        nodes.iter().any(|n| n == "fp_helper_util"),
        "co-located first-party helpers/util.py must NOT be over-pruned: {nodes:?}"
    );

    // Vendored is pruned — including the name-mismatched `dateutil` (proves the
    // detector reads RECORD rather than matching the directory name).
    for leaked in ["vendored_s3transfer_fn", "vendored_dateutil_fn"] {
        assert!(
            !nodes.iter().any(|n| n == leaked),
            "vendored symbol leaked into the graph: {leaked} in {nodes:?}"
        );
    }
    // No node may originate from a pruned directory.
    for n in g.nodes() {
        assert!(
            !n.path.contains("s3transfer") && !n.path.contains("dateutil"),
            "a node originated from a vendored dir: {}",
            n.path
        );
    }

    // Exactly the two first-party Python files were indexed.
    assert_eq!(
        stats.files_indexed, 2,
        "only app/handler.py + app/helpers/util.py are first-party sources"
    );
}

/// Finding 4 (first-party loss) repro: a vendored dist-info whose RECORD names a
/// *generic* top-level (`utils/`) that **name-collides** with a co-located
/// FIRST-PARTY `utils/` dir in the SAME parent. The first-party `utils/foo.py` is
/// in NO `RECORD`, so it MUST survive — pruning the whole `utils/` dir by name (the
/// pre-fix logic) would silently delete real code, the one failure this product
/// must not have.
///
/// ```text
/// app/utils/__init__.py                       vendored     — in s3transfer RECORD, MUST prune
/// app/utils/foo.py                            first-party  — NOT in any RECORD, MUST survive
/// app/s3transfer/__init__.py                  vendored     — in s3transfer RECORD, MUST prune
/// app/s3transfer-1.0.0.dist-info/RECORD       marker (lists utils/__init__.py + s3transfer/…)
/// app/handler.py                              first-party  — MUST index
/// ```
///
/// Pre-fix (prune `parent/<top>` by NAME): `utils/foo.py` is wrongly pruned (the
/// dir is gone) → `fp_utils_foo` absent → RED. Post-fix (prune the RECORD's exact
/// file set): only `utils/__init__.py` + `s3transfer/__init__.py` are pruned, so
/// `utils/foo.py` is reached and indexed.
fn write_name_colliding_bundle(root: &Path) {
    // First-party entrypoint.
    write_py(root, "app/handler.py", "fp_handler_entry");

    // A vendored package whose import name == its distribution prefix.
    write_py(root, "app/s3transfer/__init__.py", "vendored_s3transfer_fn");
    // A vendored single-module top-level `utils/__init__.py` that the SAME wheel
    // installed — its dir name collides with the first-party `utils/` below.
    write_py(root, "app/utils/__init__.py", "vendored_utils_init");
    // The co-located FIRST-PARTY file inside the same-named `utils/` dir. It is in
    // NO RECORD — the file-set prune must reach and index it.
    write_py(root, "app/utils/foo.py", "fp_utils_foo");

    // The dist-info RECORD lists the two vendored files (and itself), NOT foo.py.
    write_record(
        root,
        "app/s3transfer-1.0.0.dist-info/RECORD",
        concat!(
            "s3transfer/__init__.py,sha256=aaa,10\n",
            "utils/__init__.py,sha256=bbb,20\n",
            "s3transfer-1.0.0.dist-info/RECORD,,\n",
        ),
    );
}

#[test]
fn name_colliding_first_party_dir_survives_record_file_set_prune() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_name_colliding_bundle(root);

    let mut store = DuckGraphStore::open_in_memory().unwrap();
    let stats = index_repo(root, &mut store).unwrap();
    let g = store.load_graph().unwrap();
    let nodes = node_names(&g);

    // ── The guarantee: a name-colliding FIRST-PARTY file is never lost. ──
    assert!(
        nodes.iter().any(|n| n == "fp_utils_foo"),
        "first-party app/utils/foo.py (absent from any RECORD) MUST survive a \
         name-colliding vendored `utils/` top-level: {nodes:?}"
    );
    assert!(
        nodes.iter().any(|n| n == "fp_handler_entry"),
        "first-party handler must be indexed: {nodes:?}"
    );

    // ── The genuinely-vendored files (named in the RECORD) are still pruned. ──
    for leaked in ["vendored_s3transfer_fn", "vendored_utils_init"] {
        assert!(
            !nodes.iter().any(|n| n == leaked),
            "a RECORD-listed vendored symbol leaked into the graph: {leaked} in {nodes:?}"
        );
    }
    // The vendored `utils/__init__.py` is gone, but its sibling `utils/foo.py`
    // remains — so a node from `utils/` IS allowed (it is the first-party one).
    assert!(
        g.nodes()
            .any(|n| n.path == "app/utils/foo.py" && n.name == "fp_utils_foo"),
        "the surviving node must originate from the first-party app/utils/foo.py"
    );
    assert!(
        !g.nodes().any(|n| n.path == "app/utils/__init__.py"),
        "no node may originate from the vendored app/utils/__init__.py"
    );

    // Exactly the two first-party Python files (handler.py + utils/foo.py).
    assert_eq!(
        stats.files_indexed, 2,
        "only app/handler.py + app/utils/foo.py are first-party sources"
    );
}

#[test]
fn strataignore_excludes_listed_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    write_py(root, "app/handler.py", "fp_handler_entry");
    // A committed bundle the user chooses to exclude by hand (gitignore syntax).
    write_py(root, "app/blob/leaked.py", "strataignore_leaked_fn");
    fs::write(root.join(".strataignore"), "blob/\n").unwrap();

    let mut store = DuckGraphStore::open_in_memory().unwrap();
    index_repo(root, &mut store).unwrap();
    let g = store.load_graph().unwrap();
    let nodes = node_names(&g);

    assert!(
        nodes.iter().any(|n| n == "fp_handler_entry"),
        "first-party handler must be indexed: {nodes:?}"
    );
    assert!(
        !nodes.iter().any(|n| n == "strataignore_leaked_fn"),
        "a `.strataignore`-excluded symbol leaked into the graph: {nodes:?}"
    );
    for n in g.nodes() {
        assert!(
            !n.path.contains("blob"),
            "a node originated from a `.strataignore`-excluded dir: {}",
            n.path
        );
    }
}

#[test]
fn include_vendored_flag_reincludes_bundle_but_strataignore_still_applies() {
    use strata_index::{index_repo_with_options, IndexOptions};

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_dist_info_bundle(root);
    // A `.strataignore` target alongside the bundle: it must stay excluded even with
    // the escape hatch on, proving the flag is orthogonal to `.strataignore`.
    write_py(root, "app/blob/leaked.py", "strataignore_leaked_fn");
    fs::write(root.join(".strataignore"), "blob/\n").unwrap();

    let opts = IndexOptions {
        include_vendored: true,
        ..IndexOptions::default()
    };
    let mut store = DuckGraphStore::open_in_memory().unwrap();
    index_repo_with_options(root, &mut store, &opts).unwrap();
    let g = store.load_graph().unwrap();
    let nodes = node_names(&g);

    // With `--include-vendored`, detection is off: the vendored bundle IS indexed
    // (alongside the first-party handler).
    for present in [
        "vendored_s3transfer_fn",
        "vendored_dateutil_fn",
        "fp_handler_entry",
    ] {
        assert!(
            nodes.iter().any(|n| n == present),
            "--include-vendored must index {present}: {nodes:?}"
        );
    }
    // …but `.strataignore` is honored regardless of the flag.
    assert!(
        !nodes.iter().any(|n| n == "strataignore_leaked_fn"),
        "`.strataignore` must still apply under --include-vendored: {nodes:?}"
    );
}

/// Finding 6 (graph inflation) repro: a CFN/SAM template and an OpenAPI spec
/// committed INSIDE a vendored bundle must be pruned too — not just the `.py`/`.tf`
/// code. The bundle's dist-info `RECORD` lists both spec files as installed, so the
/// file-set prune (Fix A) covers them once `collect_spec_candidates` is threaded
/// with the vendored set (Fix B). A sibling FIRST-PARTY template/spec is indexed.
///
/// ```text
/// app/template.yaml                                first-party CFN   — MUST index  (FirstPartyFn)
/// app/openapi.yaml                                 first-party spec  — MUST index  (firstPartyOp)
/// app/vendir/template.yaml                         vendored CFN      — MUST prune  (VendoredFn)
/// app/vendir/api.yaml                              vendored spec     — MUST prune  (vendoredOp)
/// app/vendir-1.0.0.dist-info/RECORD                marker (lists both vendir/ files)
/// ```
///
/// Pre-fix (`collect_spec_candidates` ignores the vendored set): the vendored
/// `template.yaml` yields a `VendoredFn` node and `api.yaml` a `vendoredOp` node →
/// RED. Post-fix: neither vendored spec produces a node; the first-party two do.
fn write_vendored_specs_bundle(root: &Path) {
    // First-party spec + template (siblings of the vendored bundle).
    write_file(root, "app/template.yaml", &cfn_template("FirstPartyFn"));
    write_file(root, "app/openapi.yaml", &openapi_spec("firstPartyOp"));

    // The vendored bundle: a CFN template + an OpenAPI spec shipped inside it.
    write_file(
        root,
        "app/vendir/template.yaml",
        &cfn_template("VendoredFn"),
    );
    write_file(root, "app/vendir/api.yaml", &openapi_spec("vendoredOp"));
    // The dist-info RECORD lists both vendored spec files as installed (Fix A turns
    // these into the exact prune set; Fix B applies it to the spec collectors).
    write_record(
        root,
        "app/vendir-1.0.0.dist-info/RECORD",
        concat!(
            "vendir/template.yaml,sha256=aaa,100\n",
            "vendir/api.yaml,sha256=bbb,200\n",
            "vendir-1.0.0.dist-info/RECORD,,\n",
        ),
    );
}

#[test]
fn vendored_cfn_and_openapi_specs_are_pruned_first_party_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_vendored_specs_bundle(root);

    let mut store = DuckGraphStore::open_in_memory().unwrap();
    index_repo(root, &mut store).unwrap();
    let g = store.load_graph().unwrap();
    let nodes = node_names(&g);

    // First-party CFN Lambda + OpenAPI operation ARE indexed.
    assert!(
        nodes.iter().any(|n| n == "FirstPartyFn"),
        "first-party CFN Lambda must be indexed: {nodes:?}"
    );
    assert!(
        nodes.iter().any(|n| n == "firstPartyOp"),
        "first-party OpenAPI operation must be indexed: {nodes:?}"
    );

    // The vendored CFN template + OpenAPI spec produce NO node (the inflation the
    // finding flagged — a `template.yaml`/`openapi.yaml` inside a vendored bundle).
    for leaked in ["VendoredFn", "vendoredOp"] {
        assert!(
            !nodes.iter().any(|n| n == leaked),
            "a vendored spec leaked a node into the graph: {leaked} in {nodes:?}"
        );
    }
    // No node may originate from the vendored bundle dir.
    for n in g.nodes() {
        assert!(
            !n.path.contains("vendir/"),
            "a node originated from a vendored spec path: {}",
            n.path
        );
    }
}

#[test]
fn include_vendored_reincludes_vendored_specs() {
    use strata_index::{index_repo_with_options, IndexOptions};

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_vendored_specs_bundle(root);

    let opts = IndexOptions {
        include_vendored: true,
        ..IndexOptions::default()
    };
    let mut store = DuckGraphStore::open_in_memory().unwrap();
    index_repo_with_options(root, &mut store, &opts).unwrap();
    let g = store.load_graph().unwrap();
    let nodes = node_names(&g);

    // With detection off, the vendored specs are indexed too (transparently) —
    // alongside the first-party ones.
    for present in ["FirstPartyFn", "firstPartyOp", "VendoredFn", "vendoredOp"] {
        assert!(
            nodes.iter().any(|n| n == present),
            "--include-vendored must index {present}: {nodes:?}"
        );
    }
}
