# FAQ and troubleshooting

Practical answers, grounded in how StrataGraph actually behaves. For the deeper mechanics, the linked concepts and reference pages carry the detail.

## General

### Does StrataGraph run my code?

No. StrataGraph is **static analysis**. It parses your source with Tree-sitter (and, for TypeScript, optionally runs the `scip-typescript` compiler indexer out-of-process), reads your spec, IaC, and SQL files as text, and builds a graph from what it finds. It never executes your application, your handlers, your migrations, or your infrastructure.

### Does it need network access?

No. StrataGraph runs fully offline as a single binary. The one exception is opt-in: if you ask for precise TypeScript resolution and allow an install, it may fetch the pinned `scip-typescript`, and the contributor-only accuracy-corpus regenerators need a network. Normal indexing, querying, the MCP server, and the desktop app are all offline.

### Does it send my code anywhere?

No. There is no telemetry and no upload. The deterministic planes carry no language-model dependency at all. (The future, opt-in [knowledge plane](roadmap.md) is the only model-assisted part, and it does not exist yet.)

### What languages and planes does it cover?

Five languages (TypeScript, JavaScript, Python, C#, Rust) across four deterministic planes (code, contract, infrastructure, data), with a fifth (knowledge) designed but not yet built. All are first-class; TypeScript/JavaScript additionally has a compiler-grade `Resolved` tier (SCIP), while Python, C#, and Rust use band-disciplined heuristics (measured at ~1.0 precision for Python and Rust, extraction-validated for C#). See [Languages and coverage](../concepts/coverage.md) and the [Roadmap](roadmap.md).

### Can it handle monorepos and multi-repo estates?

Yes to both. A single index covers a whole monorepo. For code spread across **separate repositories**, StrataGraph links them into one cross-repo graph via a workspace manifest (an "estate"), so a frontend in one repo that consumes an API produced in another shows up in the blast radius. See [Multi-repo estates](../getting-started/estates.md) and [Cross-repository impact](../guides/cross-repo.md).

## Reading results

### Why is X marked `Ambiguous`?

`Ambiguous` means StrataGraph found a candidate but **could not resolve it confidently**: for example a method call whose receiver type it cannot pin down, a bare name that matches several definitions, or a contract operation key owned by more than one API. This is the honesty discipline working as designed: rather than silently pick one and risk being confident-wrong, StrataGraph surfaces the candidate and marks it uncertain. Treat anything `Ambiguous` (or below 0.40 confidence) as **unknown** and verify it in the source. See [Confidence and provenance](../concepts/confidence.md).

### Why does `impact` on a type show 0 dependents but report a hint?

`impact` on a container (a type, a class, a module) reports what depends on **that node itself**. If nothing imports or references the type directly but code does depend on its *members* (methods, fields), the direct count can be zero while a hint tells you the members have dependents. That is not a contradiction; it is pointing you at the right granularity. Run `impact` (or `context`) on the specific member to see its blast radius. See [What breaks if I change this?](../guides/impact.md).

### What does "nothing depends on this" actually mean?

It means **nothing the graph can see** depends on the symbol you asked about, within the planes and resolution StrataGraph has. It is not a proof of absolute deadness. Reflection, dynamic dispatch the heuristic could not resolve, a consumer in a repo not in your estate, or a string-built reference can all hide a real dependency. StrataGraph is recall-biased and labels its uncertainty, but a clean `impact` result is "no known dependents," not "provably none." Conversely, a contract field or operation with **zero producers and zero consumers** is flagged as *probably dead*: a strong signal, still worth a human confirm. See [Is this schema field dead?](../guides/dead-surface.md).

### Why did a result's confidence change between runs?

Confidence is a calibrated property of the evidence, capped to the edge's provenance band. As your code changes (an import added, a type annotation that lets resolution land precisely, a SCIP index now available), the evidence changes and so does the number. The bands themselves (and the will-break threshold) are derived from measurement and re-derived as the accuracy corpus grows. See [Confidence and provenance](../concepts/confidence.md) and [How accuracy is measured](../accuracy/methodology.md).

## The index

### Where does StrataGraph store its index, and how big is it?

In a `.strata/` directory at the repository root, as a DuckDB database (`.strata/graph.duckdb`) alongside a small `.strata/index.stamp` hot-reload marker. The database holds the graph, a file-hash map, and a parse cache, so re-indexing only re-parses files whose content changed. Size scales with your codebase (roughly with node and edge count); it is a single embedded file, not a server. Add `.strata/` to your `.gitignore`; it is a build artifact, not source.

### The index seems stale. How do hot-reload and reindexing work?

Two mechanisms:

- **Reindex** with `strata index <repo>`. It is incremental: unchanged files are reused from the parse cache, only changed or new files are re-parsed, and the result is identical to a full rebuild. When wired through the agent kit, a PostToolUse hook reindexes after edits automatically.
- **Hot-reload** keeps a running MCP server fresh. After each successful index, StrataGraph writes `.strata/index.stamp` **last**, once every persist has completed. Before each request, the server checks that cheap signal (the stamp bytes, falling back to the db file's mtime/length for indexes written before this feature) and swaps in the new graph if it changed: no server or session restart. The swap is degrade-safe: a reindex caught mid-write keeps the previous graph and retries on the next request, so a tool call never blocks or serves a half-loaded graph. Estates (`--workspace`) reload the same way on a manifest or per-repo change.

If results still look stale, confirm you are pointed at the right `.strata/graph.duckdb` (or the right `--workspace` manifest) and that the reindex actually ran without error.

### A file failed to parse: did it crash or get silently dropped?

Neither. A malformed input is handled gracefully and **visibly**. A broken contract spec, IaC template, or `.sql` schema is skipped (the rest of the repo still indexes) and the skip is recorded as a diagnostic (the CLI prints lines like `[infra] FAILED …` / `[data] FAILED …`, capped so a pathological repo can't flood output, with the exact failure count always preserved). The index never partially extracts a broken document and never panics on one. A malformed file produces a diagnostic, not a crash and not a silent gap.

## Setup and exclusions

### "command not found: strata"

The `strata` binary is not on your `PATH`. After `cargo build`, it is at `target/debug/strata` (or `target/release/strata` for a release build) inside the repo: run it by that path, or install it onto your `PATH` (for example `cargo install --path crates/strata-cli`, or copy/symlink the binary into a directory already on `PATH`). Check which one you're invoking with `strata --version`: it prints the package version plus the compiled engine id, so a stale binary on `PATH` is identifiable at a glance. See [Install](../getting-started/install.md).

### How do I exclude vendored or generated code?

Three layers, strongest first:

- **Automatic vendored-bundle pruning.** StrataGraph detects committed Python dependency bundles by their `*.dist-info/RECORD` and prunes exactly the files that wheel installed, per file, so a name-colliding first-party file the RECORD doesn't list survives. Pass `--include-vendored` to index them anyway.
- **Built-in skip directories.** Language-specific dependency and build-output roots are never walked: `node_modules`, `__pycache__`/`venv`/`.venv`/`site-packages`, `bin`/`obj`/`packages`/`.vs`, and Rust's `target/`.
- **`.gitignore` and `.strataignore`.** The walker honours `.gitignore`, and you can add a `.strataignore` (same gitignore syntax) as your own exclude list for anything else: the escape valve for, say, a legacy vendored tree that doesn't ship a `.dist-info`.

See [Configuration](../reference/configuration.md).

### Where do I go for the rest?

- Commands and flags: [CLI reference](../reference/cli.md).
- The MCP tools and the agent kit: [MCP tools](../reference/mcp.md) and [The agent kit](../reference/agent-kit.md).
- Exactly what is and isn't supported today: [Honest limitations](../accuracy/limitations.md) and the [Roadmap](roadmap.md).
