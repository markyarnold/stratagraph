# Roadmap

This page is an honest account of what StrataGraph does **not** do yet. Everything below is deliberately deferred: a planned next step with a clear shape, not an apology and not a bug. Where a capability is partly shipped, this page says exactly where the line is. There are no dates here; the order reflects the committed sequencing in [`docs/strata-design.md`](../../strata-design.md), not a calendar.

The principle behind every deferral is the same one that governs the shipped product: StrataGraph would rather ship a narrow capability that is honest than a broad one that guesses. So each item below is scoped to what it will *add*, and what it will keep refusing to invent.

## Data plane: deeper schema linking

The data plane today extracts explicit SQL DDL (tables, columns, foreign keys) and links code to tables via raw-SQL string literals (`Reads`/`Writes`) and ORM model classes that name their table explicitly (`MapsTo`). See [data-linking](../accuracy/methodology.md). The deferred extensions, all documented in `docs/accuracy/data-linking.md`:

- **ORM convention-derivation.** Guessing the table a model maps to from naming convention (a `class User` with no `__tablename__`, or Rails/Django implicit pluralisation) is a future `Inferred`-tier phase. Today a model with no explicit name yields no hint rather than a guessed link.
- **More ORM dialects.** TypeScript **Drizzle** (`pgTable("…")`), C# **Entity Framework** (`[Table("…")]`), and **Prisma** `.prisma` schema files (a new parser) are deferred. Drizzle in particular needs variable-symbol extraction the TS analyzer does not emit today, so a Drizzle table currently yields no hint: an honest deferral, not a silent half-link.
- **Column-level data mapping.** Both the raw-SQL and the ORM paths are table-level today. Mapping an individual model field or a `SELECT`ed column to a specific `Column` node is deferred.

Runtime query-log ingestion (resolving dynamically-built SQL by observing it at runtime) is a future enhancement, never a blocker for the static plane.

## Contract plane: gRPC code-linking and AsyncAPI

The contract plane extracts OpenAPI, GraphQL, and gRPC operations, and links producer and consumer **code** to OpenAPI and GraphQL operations across repositories. Two pieces are deferred:

- **gRPC producer/consumer code-linking.** gRPC `.proto` services are extracted as operations today, but the `PRODUCES`/`CONSUMES` edges from the service `impl` and the client stub to those operations are not yet built (`docs/accuracy/grpc-linking.md` is explicit: extraction now, code-linking next). When it lands it will be honestly banded like the OpenAPI/GraphQL links.
- **AsyncAPI.** Event-schema contracts (AsyncAPI for topics and message types) are a planned contract format (Track D4, the Phase 1 remainder). No adapter ships today.

## Infrastructure plane: IAM permission-gap detection

The infra plane builds the AWS resource graph (`LambdaFn`, `IamRole`, AppSync API/resolver/data-source, generic `CloudResource` inventory) and the wiring between them (`Assumes`, `Routes`, `Runs`, `Contains`, and the AppSync resolver → `GraphqlField` money link) from CloudFormation/SAM, Terraform HCL, Terraform plan JSON, and Terragrunt.

**IAM permission-gap detection** (Track D2, design §6.4) reconciles what a role is *allowed* to do against what its code actually *calls*. It is being built in halves, and this is exactly where the line is today.

- **Shipped: the `Grants` half (what a role is allowed to do).** A role's granted actions are extracted into `CloudAction` nodes and `Grants` edges from CloudFormation/SAM (inline `Policies` and expanded SAM policy templates such as `DynamoDBCrudPolicy`) and Terraform (`aws_iam_role_policy`, `aws_iam_policy`, inline policies). You can already see a role's grants in `context`. True to the never-confident-wrong rule, any grant that cannot be enumerated (a managed-policy ARN, a `Deny`, a `NotAction`, a `data`-source or non-literal policy document, a policy attachment) is recorded as an `<opaque:…>` marker that makes the role **INDETERMINATE**, never silently treated as "grants nothing".
- **Next: the `RequiresPermission` half (what code calls).** Statically detect AWS SDK calls in handler code (for example a boto3 `table.put_item` to `dynamodb:PutItem`, an AWS SDK v3 `PutObjectCommand` to `s3:PutObject`) via a curated call-to-action map, and emit `RequiresPermission` edges. The edge kind is reserved in the vocabulary; no analyzer emits it yet.
- **Then: the reconciliation.** A `permission_gap` traversal (an owned, in-memory Rust algorithm, like every reliability-critical traversal) plus a CLI/MCP surface that flags where a Lambda calls an action its role does not grant, and that **suppresses any role marked indeterminate by an opaque grant** so a gap is raised only when the grant set is fully known. This is the security capability that completes the infra moat.

Until both halves and the reconciliation land, the `Grants` edges are visible but gap detection itself is not. Terraform plan-JSON IAM grants and OpenTofu are close-to-free follow-ons, since both share the Terraform plan-JSON format.

## Compiler-grade precision for Python, C#, and Rust

TypeScript/JavaScript already has compiler-grade resolution via the `strata-scip` SCIP adapter, with measured, band-capped confidence ([ts-resolution](../accuracy/results.md)). The other three languages ship **Tree-sitter extraction with banded heuristics today**: honest by construction (every link capped below a RESOLVED fact, every unmade link counted), but not compiler-precise. The deferred precision backends (Track A3, in the resolved order C# → Python after TS):

- **C# / Roslyn**: full overload resolution, generic instantiation, `partial`-type merging, cross-assembly resolution. Today's C# plane is Tree-sitter, not Roslyn (`docs/accuracy/cs-extraction.md`).
- **Python / pyright**: leaning on type hints to resolve dynamic dispatch that the heuristic marks `Ambiguous` rather than guessing (`docs/accuracy/py-extraction.md`).
- **Rust / rust-analyzer**: trait-method resolution, monomorphisation, macro expansion, cross-crate resolution (`docs/accuracy/rust-extraction.md`).

Each will arrive the same way TS did: a SCIP/LSP backend behind the existing analyzer, with a measured per-band precision report and CI floors. The infra `Runs` bridge for C# (assembly-string handlers) and Rust (Cargo bin-name handlers) is deferred alongside, since resolving those handlers needs the same compiler-grade mapping. (Python contract-plane producers/consumers are **now linked**: Flask/FastAPI/Django routes, `requests`/`httpx` calls, `gql` documents, and Graphene/Strawberry/Ariadne resolvers. Only Rust and C# code remains unscanned for contract signals.)

## The knowledge plane

The fifth plane, the "why" (design documents, ADRs, diagrams, PDFs, notes), is opt-in and not yet built (design §5.5). It is the only plane that will be model-assisted, and by deliberate design its output is always tagged `MODEL`/inferred and **segregated from the deterministic planes**: it never contributes a hard dependency edge and never gates impact. It can be switched off entirely for regulated environments. The deterministic planes that ship today carry no model dependency at all; the knowledge plane is the one place that will, kept strictly walled off.

## Desktop: signing and Windows/Linux builds

The desktop app ships for macOS today. Deferred (Track E4/E5):

- **Bundle signing** and CSP hardening (the desktop's content-security-policy is currently null).
- **Windows and Linux** desktop packaging, and portable agent-kit hooks (the installed hooks use `sh -c` today, which is not portable to Windows shells).

## More editor adapters

The agent kit installs first-class integrations for **Claude Code and Kiro** today (`strata init claude|kiro`: MCP wiring, steering/skills files, and scoped pre/post-tool-use hooks). Deferred (Track E6): adapters for **Cursor, Windsurf, and Copilot**, plus `strata init --remove` to cleanly uninstall a kit. The engine surface they all build on (the MCP server) is already there; each adapter is the tool-specific wiring on top.

## Further out

Two larger directions are scoped in the design but well beyond the near-term sequence:

- **GitNexus-parity analytics** (Appendix A): hybrid search (BM25 + semantic + reciprocal rank fusion), Leiden community detection, process/flow detection, and wiki generation from the graph.
- **Org-scale collaboration**: a continuously-fresh org graph built in CI, a PR bot that posts blast radius and flags high-risk shared-contract changes, and shared/saved impact analyses. These ship here as open source as they are built; a managed, hosted option may follow later for teams that would rather not run it themselves.

For where each of these sits in the committed order, and the open questions still being resolved, see §14 and §15 of [`docs/strata-design.md`](../../strata-design.md). For exactly what the shipped capabilities can and cannot do today, see [Honest limitations](../accuracy/limitations.md) and [Languages and coverage](../concepts/coverage.md).
