//! The steering content — one source of truth, rendered per agent.
//!
//! This module is pure string construction: it has no IO and no knowledge of
//! files. [`render_steering_block`] produces the managed block (spec §2) written
//! verbatim into `CLAUDE.md`, `AGENTS.md`, and `.kiro/steering/strata.md`. The
//! four [`skills`] are static, agent-independent SKILL.md bodies.
//!
//! Budgets (spec §5): the steering block and each skill stay ≤120 lines so the
//! agent's context stays cheap; depth lives in the skills, not the block.

/// The identity facts for the steering block's first line — what the agent is
/// operating on. Either a loaded graph's real counts/planes, or the honest
/// "not yet indexed" variant when no index is present.
#[derive(Debug, Clone)]
pub enum Identity {
    /// A graph loaded at init time: real node/edge counts and which planes are
    /// present (code is always present; contract/infra only if detected).
    Indexed {
        /// The repo or estate name shown in the identity line.
        name: String,
        nodes: usize,
        edges: usize,
        /// Whether contract-plane nodes (GraphQL fields / API operations) exist.
        has_contract: bool,
        /// Whether infra-plane nodes (Lambdas / cloud resources) exist.
        has_infra: bool,
        /// True if this is a linked workspace estate (multi-repo) rather than
        /// a single repo.
        is_estate: bool,
    },
    /// No index yet — the artifacts are written with an honest placeholder and a
    /// pointer to `strata index`.
    NotIndexed,
    /// User-scope (global) install: no single repo, so a generic line.
    Global,
}

impl Identity {
    /// The first line of the steering block — the agent's environment statement.
    fn line(&self) -> String {
        match self {
            Identity::Indexed {
                name,
                nodes,
                edges,
                has_contract,
                has_infra,
                is_estate,
            } => {
                let mut planes = vec!["code"];
                if *has_contract {
                    planes.push("contract");
                }
                if *has_infra {
                    planes.push("infra");
                }
                let kind = if *is_estate { "estate" } else { "repo" };
                format!(
                    "This {kind} is indexed by StrataGraph as **{name}** ({nodes} nodes, {edges} edges; planes present: {}). The MCP tools below let you understand the code, assess blast radius across planes, and navigate safely.",
                    planes.join("/")
                )
            }
            Identity::NotIndexed => {
                "This repository is **not yet indexed** by StrataGraph. Run `strata index .` so the MCP tools below have a graph to serve. Until then, blast-radius answers are unavailable and you MUST NOT claim certainty about what depends on a symbol.".to_string()
            }
            Identity::Global => {
                "This agent kit is installed globally by StrataGraph. It resolves the current repo at runtime; if a repo is not indexed yet, run `strata index .` first, and until then do not claim certainty about what depends on a symbol.".to_string()
            }
        }
    }
}

/// Render the managed steering block body (the text *between* the markers; the
/// markers themselves are added by the managed-block writer).
///
/// Carries every MUST/NEVER from spec §2: impact-before-modify + report blast
/// radius; cross-plane checks; warn/pause on HIGH/CRITICAL or cross-repo; band
/// policy (≥0.9 act / 0.4–0.8 verify / <0.4 or ambiguous = unknown, say so);
/// dead-surface flag. Tool reference uses the REAL MCP names (`query`,
/// `context`, `impact`/`explain` + `include_contracts`/`include_infra`) and
/// states the snapshot bound honestly. Ends with the skill routing table (Claude)
/// or steering cross-refs (Kiro), supplied by the caller as `routing`.
pub fn render_steering_block(identity: &Identity, routing: &str) -> String {
    let mut s = String::new();
    s.push_str("# StrataGraph: Cross-Plane Code Intelligence\n\n");
    s.push_str(&identity.line());
    s.push_str("\n\n");

    // ── Always Do (MUST) ──
    s.push_str("## Always Do (MUST)\n\n");
    s.push_str("- **MUST act on the pre-edit blast radius the hook injects.** Before each file edit, a PreToolUse hook computes that file's blast radius and injects it as context (the same report as `strata blast <file>`). It is authoritative at edit time: read it, report the affected dependents and risk, and follow the rules below. Never edit past it without acting on what it shows.\n");
    s.push_str("- **MUST run `impact` on a symbol/field/operation BEFORE modifying it**, and report the blast radius to the user before proceeding: list the direct (d=1) and indirect (d=2) dependents with each one's `will_break` verdict (WILL BREAK only when `confidence ≥ 0.40` AND not `ambiguous`; depth does NOT decide it), its confidence, and a risk level (LOW / MEDIUM / HIGH / CRITICAL), then wait for direction.\n");
    s.push_str("- **MUST run `detect_changes` before committing.** It is the mechanical pre-commit check: it git-diffs your work, derives the changed symbols PER PLANE (code / contract / infra), aggregates the blast radius over the whole graph, and returns a risk level with reasons. Read its risk and affected set, report them, and pause for direction on HIGH/CRITICAL. Do NOT hand-run `impact` symbol-by-symbol when `detect_changes` does it across every plane in one call.\n");
    s.push_str("- **MUST check every plane the target touches.** A GraphQL field / API operation → `context` and read its `producers` (who implements it) and `consumers` (who queries it) buckets. A Lambda / handler / module → its `produces` / `consumes`. An ordinary exported symbol → `impact` for upstream dependents.\n");
    s.push_str("- **MUST warn and pause for direction** when the blast radius is HIGH or CRITICAL, when it crosses a repo boundary (estate), or when it touches contract surface consumed by another plane.\n");
    s.push_str("- **MUST treat confidence bands as trust policy:** ≥ 0.90 → act on it; 0.40–0.89 → verify in the source before relying on it; < 0.40 or `ambiguous: true` → treat as UNKNOWN and **say so explicitly; never present uncertain impact as certain.**\n");
    s.push_str("- **MUST flag likely-dead contract surface:** a field/operation with **0 producers AND 0 consumers** is probably dead, so call it out rather than treating it as live.\n");
    s.push_str(KNOWLEDGE_STEERING_ADDITION);
    s.push('\n');

    // ── Never Do ──
    s.push_str("## Never Do\n\n");
    s.push_str("- **NEVER edit a schema/contract file** (GraphQL SDL, API definition) without first running `impact` (or `context`) on the affected operations and reporting who produces and consumes them.\n");
    s.push_str("- **NEVER rename a symbol with find-and-replace.** Run `impact` first, then update exactly the d=1 set the graph reports; grep-and-replace silently corrupts cross-file and cross-plane references.\n");
    s.push_str("- **NEVER ignore a HIGH or CRITICAL risk result**, and never proceed past one without explicit user direction.\n");
    s.push_str("- **NEVER claim \"nothing depends on this\" from grep alone.** The graph carries contract and infra links that grep cannot see (a Lambda producing a field; a frontend consuming an operation). When the graph is your evidence, say so.\n\n");

    // ── Tool reference ──
    s.push_str("## Tools (MCP)\n\n");
    s.push_str("- **`impact`** `{ symbol, depth?, min_confidence?, include_contracts?, include_infra? }`: reverse blast radius (everything that depends on `symbol`). Contract- and infra-aware by default: it follows producer → operation → consumer across the contract plane (so cross-plane and cross-repo consumers appear) and Assumes/Routes/Runs across the infra plane (so an IamRole reaches the Lambdas that assume it). Pass `include_contracts: false` and/or `include_infra: false` for a narrower radius.\n");
    s.push_str("- **`explain`** `{ symbol, affected, depth?, min_confidence?, include_contracts?, include_infra? }`: WHY is `affected` in `symbol`'s blast radius? Returns the evidence chain: each edge's kind/provenance/confidence and the running (accumulated) confidence that produces impact's number, or an honest `reachable: false` when it is not in the radius. The same toggles as `impact`, so the explained confidence matches the impact row.\n");
    s.push_str("- **`context`** `{ symbol }`: the 360° view of one symbol: `callers`, `callees`, `imports_in`/`imports_out`, `members`, `container`, and the contract buckets `producers` / `consumers` / `produces` / `consumes`.\n");
    s.push_str("- **`query`** `{ text }`: case-insensitive lexical search over name / fully-qualified name / path. Use it to find the exact symbol before `impact`/`context`.\n");
    s.push_str("- **`detect_changes`** `{ staged? }`: the pre-commit check: git-diffs the working tree (or the staged index) vs HEAD, derives the changed symbols per plane (code / contract / infra), aggregates the blast radius over the graph, and returns `{ files, symbols, affected, risk }` with risk reasons. Use it before committing instead of running `impact` per changed symbol.\n");
    s.push_str(KNOWLEDGE_TOOL_LIST_ADDITION);
    s.push('\n');
    s.push_str("> **Auto-reload (read this):** the MCP server now hot-reloads. When the on-disk index changes (the PostToolUse `strata index` hook, or a manual reindex) it swaps in the fresh graph before the next request, no session/server restart needed. The reload is degrade-safe: a reindex caught mid-write keeps the previous graph and retries, so a tool call never blocks or serves a half-loaded graph. It keys off `.strata/index.stamp`, falling back to the `graph.duckdb` mtime for indexes written before this feature. (Estate `--workspace` reloads the same way on a manifest or per-repo change.)\n\n");

    s.push_str(routing.trim_end());
    s.push('\n');
    s
}

/// K7 (design §5): the knowledge-plane addition to the Always Do block —
/// three MUST bullets, verbatim from the design/plan §5 text. Kept as its own
/// constant (rather than inlined `push_str` calls like its siblings) so the
/// kit-token-audit guardrail (`steering_addition_stays_within_the_kit_token_audit_budget`)
/// can measure exactly what K7 added, independent of the pre-existing block.
///
/// - The first bullet is the **only** conditional MUST added by the knowledge
///   plane: it fires only off the free pre-edit hook's `docs:` line (never an
///   unconditional per-edit `guidance` call — the token-economy governing
///   principle, design §5).
/// - The second extends the honesty rule: doc guidance is repo knowledge, not
///   ground truth — the SAME confidence-band trust policy applies to a
///   `Mentions`/`Documents` edge as to any other.
/// - The third extends the commit rule: `detect_changes`' "docs to review"
///   line must be reported, and any fix offered, never auto-applied.
const KNOWLEDGE_STEERING_ADDITION: &str = "\
- **MUST act on the `docs:` line the pre-edit blast injects.** When it lists a section at ≥ 0.80 covering the file that you have not consulted this session, fetch `guidance` for the file BEFORE editing. Never call guidance unconditionally — the hook line is the trigger.
- **Doc guidance is repo knowledge, not ground truth.** Docs can be stale: the graph marks drift, and a mention below 0.40 or ambiguous is UNKNOWN — the same trust policy as every other band.
- **Report `detect_changes`' \"docs to review\" line in your pre-commit summary** and offer (never auto-apply) updates for stale sections.
";

/// K7: the knowledge-plane addition to the Tools (MCP) reference — `guidance`
/// and `search_docs`, the two new tools the pre-edit hook's `docs:` line and
/// the honesty rule above point at.
const KNOWLEDGE_TOOL_LIST_ADDITION: &str = "\
- **`guidance`** `{ symbol?, file?, budget?, section? }`: a token-budgeted digest of what the repo already knows about a symbol or file — its own doc comment, then the docs that document/mention it, ordered by descending confidence, sliced from disk at query time (bodies are never stored in the graph). Default budget ~1,200 tokens (4800 chars) across all sections; pass `section` (an anchor) to fetch one section's full body, uncapped.
- **`search_docs`** `{ query, limit? }`: lexical (tantivy, deterministic — no ML/embeddings) full-text search over the knowledge plane's indexed docs (markdown sections, doc comments, spec descriptions). Use it instead of grepping docs for \"how do we…?\"/\"is there guidance on X?\" questions — every hit names its matched terms, never a summary or an answer.
";

/// The Claude Code skill-routing table appended to the steering block.
pub const CLAUDE_ROUTING: &str = "\
## Skill routing

| When the task is… | Read this skill |
|---|---|
| First contact / which tool do I use? | `.claude/skills/strata/strata-guide/SKILL.md` |
| Understand architecture, \"how does X work?\" | `.claude/skills/strata/strata-exploring/SKILL.md` |
| Blast radius, \"what breaks if I change X?\" | `.claude/skills/strata/strata-impact-analysis/SKILL.md` |
| Schema/API/infra: producers, consumers, dead surface | `.claude/skills/strata/strata-contracts-and-infra/SKILL.md` |";

/// The Kiro steering cross-references appended to the steering block. Kiro reads
/// steering files, not skills, so this points at the lifecycle hooks and tools
/// rather than a skill table.
pub const KIRO_ROUTING: &str = "\
## Workflow hooks (Kiro)

Two lifecycle hooks run automatically, both scoped to the file-write tools:
- **strata-pre-edit**: before any file write, confirms you ran `impact`/`blast` on every symbol/field about to change.
- **strata-post-edit**: after a file edit, re-runs `strata index .` to keep the on-disk graph fresh (the MCP server hot-reloads it).

There is deliberately **no pre-commit hook**: Kiro can only trigger a hook by tool name, and there is no \"git commit\" tool (a commit runs through the same shell tool as every other command), so a pre-commit hook would fire on all shell use. The commit-time check is therefore a rule you run **yourself**: before you create a git commit, run `detect_changes`, report its per-plane affected set and risk, and pause on HIGH/CRITICAL — exactly as the Always Do rules above require.

When in doubt: `query` to find the symbol → `context` for its plane buckets → `impact` before you change it → `detect_changes` before you commit.";

/// The four Claude Code skills, each as a `(slug, SKILL.md body)` pair. The slug
/// is the directory name under `.claude/skills/strata/`.
pub fn skills() -> [(&'static str, String); 4] {
    [
        ("strata-guide", skill_guide()),
        ("strata-exploring", skill_exploring()),
        ("strata-impact-analysis", skill_impact_analysis()),
        ("strata-contracts-and-infra", skill_contracts_and_infra()),
    ]
}

/// Markdown frontmatter for a skill (name + trigger-example description).
fn frontmatter(name: &str, description: &str) -> String {
    format!("---\nname: {name}\ndescription: \"{description}\"\n---\n\n")
}

/// The shared band-policy table, reproduced in each skill so a reader landing on
/// any one skill sees the trust rule. This is the never-confident-wrong rule.
const BAND_POLICY_TABLE: &str = "\
| Confidence | Policy |
|---|---|
| ≥ 0.90 | Act on it. |
| 0.40 – 0.89 | Verify in the source before relying on it. |
| < 0.40 or `ambiguous: true` | UNKNOWN: say so explicitly; never present it as certain. |";

/// The shared blast-radius / risk vocabulary table.
///
/// Depth is **distance**, not a verdict: d=1 is a direct dependent, d=2 indirect,
/// d=3 transitive. The **WILL BREAK / may affect** call is per row and lives in the
/// `will_break` field the engine stamps — `confidence ≥ 0.40 AND not ambiguous`,
/// *independent of depth*. So a d=1 row can still be "may affect" (if it is
/// ambiguous or < 0.40), and the verdict is never read off the depth column.
const BLAST_RADIUS_TABLE: &str = "\
| Depth (distance) | Meaning | Risk rubric |
|---|---|---|
| d=1 | direct dependent | LOW < 5 affected |
| d=2 | indirect dependent | MEDIUM 5–15 affected |
| d=3 | transitive dependent | HIGH > 15, or many flows |
| – | contract/infra path | CRITICAL: auth, payments, or contract surface |

**Verdict is per row, not per depth.** Each affected node carries a `will_break` flag (`WILL BREAK` when `confidence ≥ 0.40` AND not `ambiguous`, else `may affect`), computed independent of depth. A d=1 dependent that is ambiguous or below 0.40 is `may affect`, never a certain break; report it as UNKNOWN per the band policy.";

fn skill_guide() -> String {
    let mut s = frontmatter(
        "strata-guide",
        "Use first when you are unsure which StrataGraph tool to reach for, or need the tool/plane/band reference. Examples: \\\"which tool do I use?\\\", \\\"what does StrataGraph index?\\\", \\\"how do I read confidence?\\\"",
    );
    s.push_str("# StrataGraph: Guide\n\n");
    s.push_str("StrataGraph is a cross-plane code graph: **code** (functions, classes, imports, calls), **contract** (GraphQL fields, API operations), and **infra** (Lambdas, cloud resources), linked by producer/consumer and runs/routes edges. It answers \"what breaks if I change X?\" across all three, including links grep cannot see.\n\n");

    s.push_str("## When to Use\n\n");
    s.push_str("- You don't yet know which tool answers the question.\n");
    s.push_str("- You need the tool surface, the plane model, or the confidence-band policy.\n");
    s.push_str("- You're about to edit and want the safe-change protocol in one place.\n");
    s.push_str("- Repo conventions / \"is there guidance on X?\" → reach for `guidance`/`search_docs` directly; no need to grep the docs tree by hand.\n\n");

    s.push_str("## The tools\n\n");
    s.push_str("- **`query({ text })`**: find the symbol by name / fqn / path (case-insensitive substring). Start here when you only have a name.\n");
    s.push_str("- **`context({ symbol })`**: the 360° view: `callers`, `callees`, `imports_in`/`imports_out`, `members`, `container`, the contract buckets `producers` / `consumers` / `produces` / `consumes`, and the knowledge-plane `docs` bucket (refs only — the sections that document/mention this symbol).\n");
    s.push_str("- **`impact({ symbol, depth?, min_confidence?, include_contracts?, include_infra? })`**: reverse blast radius. Contract- and infra-aware by default (follows producer → operation → consumer, and Assumes/Routes/Runs); pass `include_contracts: false` and/or `include_infra: false` to narrow it.\n");
    s.push_str("- **`explain({ symbol, affected, … })`**: the evidence chain proving WHY `affected` is in `symbol`'s blast radius (per-edge provenance/confidence + the running confidence), or an honest `reachable: false`.\n");
    s.push_str("- **`guidance({ symbol? | file?, budget?, section? })`**: a token-budgeted digest of what the repo already knows about a symbol/file — doc comment first, then the docs that document/mention it, by descending confidence. Bodies are read from disk at query time, so guidance is never staler than the file. Doc guidance is repo knowledge, not ground truth: a section below 0.40 or ambiguous is UNKNOWN, same trust policy as every other band.\n");
    s.push_str("- **`search_docs({ query, limit? })`**: lexical (tantivy, deterministic) full-text search over indexed docs — markdown sections, doc comments, spec descriptions. Every hit names its matched terms; use it instead of grepping the docs tree.\n\n");

    s.push_str("## Reading confidence (trust policy)\n\n");
    s.push_str(BAND_POLICY_TABLE);
    s.push_str("\n\n");

    s.push_str("## Blast radius & risk\n\n");
    s.push_str(BLAST_RADIUS_TABLE);
    s.push_str("\n\n");

    s.push_str("## Safe-change protocol\n\n");
    s.push_str("```\n");
    s.push_str("1. query(name)              → resolve the exact symbol\n");
    s.push_str("2. context(symbol)          → which planes does it touch?\n");
    s.push_str("3. impact(symbol)           → who breaks (d=1 / d=2, confidence, risk)\n");
    s.push_str("4. report to the user; PAUSE if HIGH/CRITICAL or cross-repo\n");
    s.push_str("5. change only the d=1 set the graph reports\n");
    s.push_str("```\n\n");

    s.push_str("> **Auto-reload:** the server hot-reloads the graph when the on-disk index changes (the edit hook's reindex, or a manual one), no session restart needed. The swap is degrade-safe: a reindex caught mid-write keeps the current graph and retries on the next call.\n");
    s
}

fn skill_exploring() -> String {
    let mut s = frontmatter(
        "strata-exploring",
        "Use when the user asks how code works, wants the architecture, or to trace a flow across code/contract/infra. Examples: \\\"how does the policy flow work?\\\", \\\"what calls this?\\\", \\\"trace this operation end to end\\\"",
    );
    s.push_str("# StrataGraph: Exploring\n\n");

    s.push_str("## When to Use\n\n");
    s.push_str("- \"How does X work?\" / \"Walk me through the Y flow.\"\n");
    s.push_str("- \"What calls this function?\" / \"What does this module import?\"\n");
    s.push_str("- Tracing an operation across planes: frontend consumer → GraphQL field → producer Lambda.\n");
    s.push_str("- Prefer this over grep: the graph sees cross-file, cross-plane, and cross-repo edges grep misses.\n\n");

    s.push_str("## Guidance-first: check what the repo already knows\n\n");
    s.push_str("Before reading source in an unfamiliar area, ask the knowledge plane first — the repo may already explain it, cheaper than reconstructing it from code: run `guidance({ file: \"<path>\" })` for a token-budgeted digest of the doc comments and docs that cover that file, and `search_docs({ query: \"<question>\" })` for a lexical search across the whole indexed docs tree (markdown, doc comments, spec descriptions). Both are refs-first and explainable — never a summary or an answer — so treat what they return as a lead into the source, not a replacement for reading it. Doc guidance is repo knowledge, not ground truth: a stale or ambiguous mention is UNKNOWN, same trust policy as every other band.\n\n");

    s.push_str("## Workflow\n\n");
    s.push_str("```\n");
    s.push_str(
        "0. guidance({ file })            → what does the repo already say about this file?\n",
    );
    s.push_str("   search_docs({ query })         → or search the whole docs tree by question\n");
    s.push_str("1. query({ text: \"concept\" })   → find candidate symbols/fields\n");
    s.push_str("2. context({ symbol })           → callers/callees + producers/consumers + docs\n");
    s.push_str("3. follow the buckets outward     → walk the flow across planes\n");
    s.push_str("```\n\n");

    s.push_str("## Checklist\n\n");
    s.push_str("```\n");
    s.push_str(
        "- [ ] for an unfamiliar file/area, try guidance()/search_docs() before reading source\n",
    );
    s.push_str("- [ ] query() to locate the entry symbol (don't guess the fqn)\n");
    s.push_str("- [ ] context() to see callers (who reaches it) and callees (what it reaches)\n");
    s.push_str(
        "- [ ] for a field/operation, read producers (implementers) + consumers (callers)\n",
    );
    s.push_str("- [ ] follow produces/consumes to cross from code into the contract plane\n");
    s.push_str("- [ ] check the docs bucket for sections that document/mention the symbol\n");
    s.push_str("- [ ] state confidence honestly per the band policy\n");
    s.push_str("```\n\n");

    s.push_str("## Understanding Output\n\n");
    s.push_str("`context` buckets, by plane:\n");
    s.push_str("- **code:** `callers` (who calls it), `callees` (what it calls), `imports_in`/`imports_out`, `members`, `container`.\n");
    s.push_str("- **contract:** `producers` (who implements this field/op), `consumers` (who queries it); `produces`/`consumes` are the outgoing views from a Lambda/module.\n");
    s.push_str("- **knowledge:** `docs` — every doc section that documents (a doc comment) or mentions this symbol, refs only (`uid`, `name`, `anchor`, `provenance`, `confidence`); fetch a section's body with `guidance({ symbol, section: <anchor> })`.\n\n");
    s.push_str(BAND_POLICY_TABLE);
    s.push_str("\n\n");

    s.push_str("## Worked Example: \"How is `getPolicyStats` served?\"\n\n");
    s.push_str("```\n");
    s.push_str("1. query({ text: \"getPolicyStats\" })\n");
    s.push_str("   → GraphqlField  Query.getPolicyStats\n\n");
    s.push_str("2. context({ symbol: \"getPolicyStats\" })\n");
    s.push_str("   → producers: PolicyOperationsFunction (Lambda)      [implements it]\n");
    s.push_str("   → consumers: frontend/policies.ts                   [queries it]\n\n");
    s.push_str("3. Flow: policies.ts ──query──> Query.getPolicyStats ──served by──> PolicyOperationsFunction\n");
    s.push_str("```\n\n");
    s.push_str("That single `context` crossed two planes (the frontend consumer and the producer Lambda) without reading a line of source.\n");
    s
}

fn skill_impact_analysis() -> String {
    let mut s = frontmatter(
        "strata-impact-analysis",
        "Use when the user wants to know what will break if they change something, or needs safety analysis before editing. Examples: \\\"is it safe to change X?\\\", \\\"what depends on this?\\\", \\\"what will break?\\\"",
    );
    s.push_str("# StrataGraph: Impact Analysis\n\n");

    s.push_str("## When to Use\n\n");
    s.push_str("- \"Is it safe to change this function / field / operation?\"\n");
    s.push_str("- \"What will break if I modify X?\" / \"Show me the blast radius.\"\n");
    s.push_str(
        "- **Before any non-trivial edit, and before a rename**: always, per the steering rules.\n",
    );
    s.push_str("- Before a commit, to confirm the change touches only what you intend.\n\n");

    s.push_str("## Workflow\n\n");
    s.push_str("```\n");
    s.push_str("1. query({ text })                    → resolve the exact symbol\n");
    s.push_str("2. impact({ symbol })                 → contract- & infra-aware blast radius\n");
    s.push_str(
        "3. (optional) impact({ symbol, include_contracts: false, include_infra: false })\n",
    );
    s.push_str("                                      → code-only radius, to separate planes\n");
    s.push_str("4. classify each dependent by its verdict + confidence; assign risk\n");
    s.push_str("5. (optional) explain({ symbol, affected }) → WHY a dependent is in the radius\n");
    s.push_str("6. report to the user; PAUSE if HIGH/CRITICAL or cross-repo\n");
    s.push_str("```\n\n");

    s.push_str("## Checklist\n\n");
    s.push_str("```\n");
    s.push_str("- [ ] impact() run BEFORE editing (not after)\n");
    s.push_str("- [ ] direct (d=1) dependents reviewed first, but read each row's verdict\n");
    s.push_str("- [ ] WILL BREAK only where will_break=true (conf >= 0.40 AND not ambiguous)\n");
    s.push_str(
        "- [ ] confidence band applied per item (≥0.9 act / 0.4–0.8 verify / <0.4 unknown)\n",
    );
    s.push_str("- [ ] ambiguous:true or <0.40 items called out as UNKNOWN, never as certain\n");
    s.push_str("- [ ] cross-plane / cross-repo consumers noted (contract-aware default)\n");
    s.push_str("- [ ] risk level assigned and reported; HIGH/CRITICAL → pause for direction\n");
    s.push_str("```\n\n");

    s.push_str("## Understanding Output\n\n");
    s.push_str(
        "`impact` returns `affected[]`, each with `depth`, `confidence`, `ambiguous`, and the derived `will_break` verdict (the WILL BREAK / may-affect call: `confidence ≥ 0.40` AND not `ambiguous`, independent of depth):\n\n",
    );
    s.push_str(BLAST_RADIUS_TABLE);
    s.push_str("\n\n");
    s.push_str(BAND_POLICY_TABLE);
    s.push_str("\n\n");
    s.push_str("**Zero direct dependents is NOT \"dead\" on a member-bearing node.** When `impact` on a class/struct/enum/interface/Table returns 0 DIRECT dependents but the result carries a non-empty `members_with_dependents`, the type is NOT dead: its methods/columns have dependents. The tool surfaces them (the CLI prints `0 dependents on X itself; N of its members have dependents: …`, MCP returns a `members_with_dependents` field, the desktop shows the same hint); run `impact` on a named member to see those dependents. This is the never-say-\"nothing depends on this\" guarantee in action, so report the members, never a bare \"nothing affected.\"\n\n");
    s.push_str("To justify any single row, run `explain({ symbol, affected })`: it returns the evidence chain (each hop's edge kind, provenance, and confidence, plus the running confidence that yields impact's number), or `reachable: false` when the node is not actually in the radius. Use the SAME `include_contracts`/`include_infra` toggles you ran `impact` with, so the explained confidence matches the row.\n\n");

    s.push_str("## Worked Example: \"What breaks if I change `Query.getPolicyStats`?\"\n\n");
    s.push_str("```\n");
    s.push_str("impact({ symbol: \"getPolicyStats\" })\n");
    s.push_str("→ affected:\n");
    s.push_str("  d=1  conf 0.95  amb no   PolicyOperationsFunction (Lambda)     WILL BREAK\n");
    s.push_str("  d=1  conf 0.95  amb no   frontend/policies.ts (gql consumer)   WILL BREAK\n");
    s.push_str("  d=1  conf 0.30  amb yes  legacy/probe.ts (heuristic call)      may affect\n\n");
    s.push_str("Three DIRECT (d=1) dependents, but the verdict is per row, not from the\n");
    s.push_str("depth. The two 0.95 non-ambiguous rows are WILL BREAK → act. The third is\n");
    s.push_str("d=1 too, yet ambiguous at 0.30 → `may affect`: report it as UNKNOWN, never\n");
    s.push_str("as a certain break. Two of these cross into the contract plane (producer +\n");
    s.push_str("consumer) → report it and PAUSE for direction before editing the schema.\n");
    s.push_str("```\n\n");
    s.push_str("Compare `impact({ symbol: \"getPolicyStats\", include_contracts: false })`: the frontend consumer drops out, leaving the code-only radius, useful to see which dependents are cross-plane.\n");
    s
}

fn skill_contracts_and_infra() -> String {
    let mut s = frontmatter(
        "strata-contracts-and-infra",
        "Use when changing a GraphQL field / API operation / schema, or working with Lambdas and cloud resources, to find producers, consumers, and dead contract surface. Examples: \\\"who implements this field?\\\", \\\"who consumes this operation?\\\", \\\"is this schema field dead?\\\"",
    );
    s.push_str("# StrataGraph: Contracts & Infra\n\n");
    s.push_str("The contract and infra planes are why StrataGraph sees what grep can't: a Lambda *produces* a GraphQL field; a frontend module *consumes* an operation. Changing a field without checking both sides is how cross-plane breakage ships.\n\n");

    s.push_str("## When to Use\n\n");
    s.push_str("- Editing a GraphQL field / API operation / schema file.\n");
    s.push_str("- \"Who implements (produces) this field?\" / \"Who queries (consumes) it?\"\n");
    s.push_str("- \"Is this field dead?\": zero producers and zero consumers.\n");
    s.push_str(
        "- Touching a Lambda or cloud resource and needing its produced/consumed surface.\n\n",
    );

    s.push_str("## Workflow\n\n");
    s.push_str("```\n");
    s.push_str("1. query({ text })          → resolve the field/operation/Lambda\n");
    s.push_str("2. context({ symbol })      → producers + consumers (and produces/consumes)\n");
    s.push_str("3. impact({ symbol })       → full cross-plane blast radius before editing\n");
    s.push_str("4. dead-surface check: producers (0) AND consumers (0) → flag as likely dead\n");
    s.push_str("```\n\n");

    s.push_str("## Checklist\n\n");
    s.push_str("```\n");
    s.push_str("- [ ] context() read BEFORE editing any schema/contract file\n");
    s.push_str("- [ ] producers bucket inspected: who implements it (which Lambda/resolver)\n");
    s.push_str("- [ ] consumers bucket inspected: who queries it (which frontend/module)\n");
    s.push_str("- [ ] 0 producers AND 0 consumers → reported as likely-dead surface\n");
    s.push_str("- [ ] cross-repo consumers noted; contract surface → warn and pause\n");
    s.push_str("```\n\n");

    s.push_str("## Understanding Output\n\n");
    s.push_str("On a **field/operation**: `producers` = implementers (Lambda/resolver), `consumers` = callers (frontend/module). On a **Lambda/module**: `produces` = fields it implements, `consumes` = operations it calls. All four buckets are always present, so `producers (0) / consumers (0)` is a real, readable signal, not a missing answer.\n\n");

    s.push_str("## Worked Example A: live field\n\n");
    s.push_str("```\n");
    s.push_str("context({ symbol: \"getPolicyStats\" })\n");
    s.push_str("→ producers (1): PolicyOperationsFunction\n");
    s.push_str("→ consumers (1): frontend/policies.ts\n");
    s.push_str("Live, cross-plane. Editing it affects both sides → report + pause.\n");
    s.push_str("```\n\n");

    s.push_str("## Worked Example B: dead-surface discovery\n\n");
    s.push_str("```\n");
    s.push_str("context({ symbol: \"getActiveGeneralPolicies\" })\n");
    s.push_str("→ producers (0)\n");
    s.push_str("→ consumers (0)\n");
    s.push_str("Zero producers AND zero consumers → likely DEAD schema surface.\n");
    s.push_str("Flag it to the user (candidate for removal); do NOT assume it is wired up.\n");
    s.push_str("```\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The steering block names `detect_changes` as the pre-commit check — in the
    /// Always Do MUST and the tool reference — so the kit teaches the tool, not the
    /// old manual per-symbol `impact` protocol (Slice 12).
    #[test]
    fn steering_block_names_detect_changes_for_pre_commit() {
        let block = render_steering_block(&Identity::NotIndexed, CLAUDE_ROUTING);
        assert!(
            block.contains("MUST run `detect_changes` before committing"),
            "the Always Do section must carry the detect_changes pre-commit MUST:\n{block}"
        );
        assert!(
            block.contains("**`detect_changes`** `{ staged? }`"),
            "the Tools (MCP) reference must document detect_changes:\n{block}"
        );
    }

    /// The Kiro routing lists exactly the two write-scoped hooks, does NOT claim a
    /// pre-commit hook (Kiro has no commit trigger), and keeps the commit-time
    /// `detect_changes` step as a rule the agent runs itself.
    #[test]
    fn kiro_routing_has_no_pre_commit_hook_but_keeps_the_commit_rule() {
        assert!(
            KIRO_ROUTING.contains("strata-pre-edit") && KIRO_ROUTING.contains("strata-post-edit"),
            "the two write-scoped hooks must be listed:\n{KIRO_ROUTING}"
        );
        assert!(
            !KIRO_ROUTING.contains("strata-pre-commit"),
            "the routing must NOT claim a pre-commit hook (Kiro has no commit trigger):\n{KIRO_ROUTING}"
        );
        assert!(
            KIRO_ROUTING.contains("no pre-commit hook"),
            "the routing must explain why there is no pre-commit hook:\n{KIRO_ROUTING}"
        );
        assert!(
            KIRO_ROUTING.contains("before you create a git commit, run `detect_changes`"),
            "the commit-time detect_changes step must survive as a self-run rule:\n{KIRO_ROUTING}"
        );
    }

    /// Slice 20: the steering's Always Do section tells the agent that the pre-edit
    /// hook injects the file's blast radius and it MUST act on that injected context
    /// — the prose half of the robust enforcement.
    #[test]
    fn steering_block_names_the_pre_edit_blast_injection() {
        let block = render_steering_block(&Identity::NotIndexed, CLAUDE_ROUTING);
        assert!(
            block.contains("MUST act on the pre-edit blast radius the hook injects"),
            "the Always Do section must tell the agent to act on the injected blast:\n{block}"
        );
        assert!(
            block.contains("authoritative at edit time"),
            "the injected blast must be stated as authoritative-at-edit-time:\n{block}"
        );
    }

    /// Slice 27 (H1): honesty guard — the kit must NOT equate depth with the
    /// will-break verdict. The engine's `will_break_label` is `conf ≥ 0.40 &&
    /// !ambiguous`, INDEPENDENT of depth (`traverse.rs`); a d=1 dependent that is
    /// ambiguous or sub-0.40 is "may affect", not a certain break. So neither the
    /// steering block nor any skill may carry the old `d=1 ... WILL BREAK` equation
    /// or instruct that d=1 items "WILL BREAK" as a class.
    #[test]
    fn kit_never_equates_depth_with_will_break() {
        // The shared blast-radius table (used by the block-bearing skills) must
        // describe d=1 as a *direct dependent*, never stamp the d=1 ROW "WILL BREAK".
        // (The table's prose may explain the will-break verdict — what it must NOT do
        // is put the verdict in a depth row, which is the depth↔verdict conflation.)
        let d1_row = BLAST_RADIUS_TABLE
            .lines()
            .find(|l| l.contains("d=1"))
            .expect("blast-radius table has a d=1 row");
        assert!(
            d1_row.contains("direct dependent"),
            "the d=1 row must call it a direct dependent:\n{d1_row}"
        );
        assert!(
            !d1_row.contains("WILL BREAK"),
            "the d=1 row must not be stamped WILL BREAK (verdict is conf-gated, not depth):\n{d1_row}"
        );
        // The verdict must be tied to confidence + non-ambiguity SOMEWHERE in the
        // table block, so a reader learns it is not a depth call.
        assert!(
            BLAST_RADIUS_TABLE.contains("confidence ≥ 0.40")
                && BLAST_RADIUS_TABLE.contains("ambiguous"),
            "the table must explain WILL BREAK as conf>=0.40 and not ambiguous:\n{BLAST_RADIUS_TABLE}"
        );

        // The steering block's impact MUST line must not present d=1 as WILL BREAK.
        let block = render_steering_block(&Identity::NotIndexed, CLAUDE_ROUTING);
        assert!(
            !block.contains("d=1 (**WILL BREAK**)"),
            "the Always Do impact line must not equate d=1 with WILL BREAK:\n{block}"
        );

        // No skill body may equate a depth with the will-break verdict either, and
        // each must tie the verdict to confidence + non-ambiguity.
        for (slug, body) in skills() {
            assert!(
                !body.contains("d=1") || !body.contains("these WILL BREAK"),
                "skill {slug} equates d=1 with a certain break:\n{body}"
            );
        }
    }

    /// The impact-analysis skill must document the members-with-dependents
    /// behavior: a member-bearing node (class/struct/enum/interface/Table) with 0
    /// DIRECT dependents is NOT dead when `members_with_dependents` is populated —
    /// its members have dependents, the tool surfaces them, and the agent runs
    /// `impact` on a named member to see them. This is the never-say-"nothing
    /// depends on this" guarantee the engine/CLI/MCP/GUI all surface, so the kit
    /// must teach it rather than letting an agent read 0-direct as dead.
    #[test]
    fn impact_skill_documents_members_with_dependents() {
        let body = skill_impact_analysis();
        assert!(
            body.contains("members_with_dependents"),
            "the impact skill must name the members_with_dependents result:\n{body}"
        );
        assert!(
            body.contains("is NOT dead") || body.contains("NOT \"dead\""),
            "the impact skill must state 0 direct deps on a member-bearing node is NOT dead:\n{body}"
        );
        assert!(
            body.contains("never-say-\"nothing depends on this\"")
                || body.contains("nothing depends on this"),
            "the impact skill must tie this to the never-say-nothing-depends guarantee:\n{body}"
        );
    }

    #[test]
    fn global_identity_line_is_generic_with_no_counts() {
        let line = Identity::Global.line();
        assert!(line.contains("resolves the current repo"), "got: {line}");
        // Never embeds per-repo counts in a global block.
        assert!(!line.contains("nodes"));
        assert!(!line.contains("indexed by StrataGraph as"));
    }

    // ── K7: knowledge-plane steering additions (design §5) ──────────────────

    /// The three exact knowledge-plane steering lines (spec/plan §5) must be
    /// present in the Always Do block, verbatim — this is pinned contract
    /// text, not a paraphrase the implementation is free to reword.
    #[test]
    fn steering_block_carries_the_knowledge_plane_always_do_lines() {
        let block = render_steering_block(&Identity::NotIndexed, CLAUDE_ROUTING);
        assert!(
            block.contains("MUST act on the `docs:` line the pre-edit blast injects."),
            "missing the conditional guidance-fetch MUST:\n{block}"
        );
        assert!(
            block.contains(
                "fetch `guidance` for the file BEFORE editing. Never call guidance \
                 unconditionally — the hook line is the trigger."
            ),
            "the conditional MUST must state the hook line (never unconditional) is the trigger:\n{block}"
        );
        assert!(
            block.contains("Doc guidance is repo knowledge, not ground truth."),
            "missing the honesty rule for doc guidance:\n{block}"
        );
        assert!(
            block.contains(
                "a mention below 0.40 or ambiguous is UNKNOWN — the same trust policy as every other band"
            ),
            "the honesty rule must tie doc mentions to the SAME band policy as every other edge:\n{block}"
        );
        assert!(
            block.contains(
                "Report `detect_changes`' \"docs to review\" line in your pre-commit summary"
            ),
            "missing the commit-rule extension for docs to review:\n{block}"
        );
        assert!(
            block.contains("offer (never auto-apply) updates for stale sections"),
            "the commit rule must say offer, never auto-apply:\n{block}"
        );
    }

    /// The Tools (MCP) reference must list `guidance` and `search_docs`
    /// alongside the original five — Kiro has no skills to fall back on, so
    /// the shared steering block is the only place it learns these exist.
    #[test]
    fn steering_block_tool_reference_lists_guidance_and_search_docs() {
        let block = render_steering_block(&Identity::NotIndexed, CLAUDE_ROUTING);
        assert!(
            block.contains("**`guidance`**"),
            "tool reference must document guidance:\n{block}"
        );
        assert!(
            block.contains("**`search_docs`**"),
            "tool reference must document search_docs:\n{block}"
        );
        // Both kits share this body — the SAME two entries must reach Kiro's
        // steering file too (Kiro reads steering only, never skills).
        let kiro_block = render_steering_block(&Identity::NotIndexed, KIRO_ROUTING);
        assert!(kiro_block.contains("**`guidance`**") && kiro_block.contains("**`search_docs`**"));
    }

    /// Kit token audit (design §5): the K7 steering ADDITION — the three new
    /// Always Do bullets plus the two new tool-list entries — must stay small
    /// (design target ~8 lines, hard-capped here at 10) so the per-session
    /// context cost stays bounded.
    #[test]
    fn steering_addition_stays_within_the_kit_token_audit_budget() {
        let total_lines = KNOWLEDGE_STEERING_ADDITION.lines().count()
            + KNOWLEDGE_TOOL_LIST_ADDITION.lines().count();
        assert!(
            total_lines <= 10,
            "K7 steering addition must stay ≤10 lines (design target ~8), got {total_lines}"
        );
    }

    /// K7 (design §5, "Claude Code specifics"): `strata-guide` gains a routing
    /// pointer for repo-conventions questions straight at the two new tools
    /// (no fifth skill — the content extends the existing two), plus tool-list
    /// rows for `guidance`/`search_docs` alongside query/context/impact/explain.
    #[test]
    fn guide_skill_routes_repo_conventions_to_guidance_and_search_docs() {
        let body = skill_guide();
        assert!(
            body.contains("Repo conventions") && body.contains("is there guidance on X?"),
            "strata-guide must route repo-conventions questions to the new tools:\n{body}"
        );
        assert!(
            body.contains("`guidance(") && body.contains("`search_docs("),
            "strata-guide's tool list must cover guidance/search_docs:\n{body}"
        );
    }

    /// K7: `strata-exploring` teaches guidance-first exploration of an
    /// unfamiliar file/area — try `guidance`/`search_docs` before reading
    /// source, the same "cheap references first" discipline the design's
    /// two-stage retrieval principle states for the engine itself.
    #[test]
    fn exploring_skill_recommends_guidance_first_for_unfamiliar_areas() {
        let body = skill_exploring();
        assert!(
            body.contains("`guidance(") && body.contains("`search_docs("),
            "strata-exploring must mention guidance/search_docs:\n{body}"
        );
        assert!(
            body.to_lowercase().contains("before reading"),
            "strata-exploring must recommend guidance/search_docs BEFORE reading source:\n{body}"
        );
    }

    /// No fifth skill (design §5): the knowledge-plane content extends the
    /// existing two skills; the skill set itself must stay at exactly four.
    #[test]
    fn still_exactly_four_skills_no_fifth_added() {
        assert_eq!(skills().len(), 4, "no fifth skill for the knowledge plane");
    }
}
