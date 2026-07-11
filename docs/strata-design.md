# StrataGraph: Cross Boundary Code Intelligence

**Name:** StrataGraph (evokes the layered planes of the graph)
**Status:** Draft v0.1, foundational design document for iteration
**Applies to:** any codebase, from a single repository monolith to a distributed estate. Contract, data, infrastructure and knowledge awareness are optional planes that activate themselves when the relevant artefacts are present. Cloud and infrastructure support is provider agnostic, richest on AWS as the first adapter.

---

## Contents

1. [Vision and thesis](#1-vision-and-thesis)
2. [The core idea: a universal core, progressive planes](#2-the-core-idea-a-universal-core-progressive-planes)
3. [Design principles (what makes it bomb proof)](#3-design-principles-what-makes-it-bomb-proof)
4. [The unified data model](#4-the-unified-data-model)
5. [The extraction pipeline](#5-the-extraction-pipeline)
6. [Cross boundary impact: the flagship capability](#6-cross-boundary-impact-the-flagship-capability)
7. [Graph storage architecture](#7-graph-storage-architecture)
8. [Engine architecture](#8-engine-architecture)
9. [Frontend and desktop app](#9-frontend-and-desktop-app)
10. [Collaboration layer](#10-collaboration-layer)
11. [MCP tool and resource surface](#11-mcp-tool-and-resource-surface)
12. [Security, privacy and governance](#12-security-privacy-and-governance)
13. [Source model and licensing](#13-source-model-and-licensing)
14. [Phased roadmap](#14-phased-roadmap)
15. [Decisions and remaining open questions](#15-decisions-and-remaining-open-questions)
- [Appendix A: feature parity and incorporation](#appendix-a-feature-parity-and-incorporation)

---

## 1. Vision and thesis

Modern AI coding agents and human engineers fail at the same point: they understand individual files but not how the system fits together across boundaries. The boundaries are where the damage happens. A change to an API producer breaks a consumer in another repository. A change to a database column breaks three services that query it. A change to an IAM role silently removes a permission a Lambda needs at runtime.

Existing knowledge graph tools (GitNexus, Graphify, CodeGraphContext, Sourcegraph Cody) stop at the repository edge and treat the database and the infrastructure as out of scope. StrataGraph's thesis is to own exactly those boundaries.

> **One line:** Turn any codebase into a queryable knowledge graph, then follow the dependencies as far as your code, contracts, data and infrastructure allow. Know what breaks before you ship.

The universal product, the reason anyone reaches for this the way they reach for GitNexus or Graphify, is a precise structural graph of a codebase that agents and humans can query. That works for a single monolith with no database and no cloud. Every plane beyond the code plane is a progressive enhancement that activates itself when its inputs are present, and adds nothing the user has to configure to get started.

The defensible position is not "a better code graph". The code intelligence layer is commoditising and the IDEs (Cursor, Claude Code, Codex) are likely to absorb it. The differentiation is owning the boundaries the incumbents treat as edges: the data the code reads and writes, the contracts between components, and, where they exist, the infrastructure that runs and permits it. These are not commoditised, and the infrastructure plane in particular is a standout capability no comparable tool offers. It is the thing that makes StrataGraph unforgettable in environments that have it, while never getting in the way of environments that do not.

## 2. The core idea: a universal core, progressive planes

StrataGraph builds a single queryable graph. The code plane is the universal core and is always built. The other four planes are progressive enhancements: each activates itself when StrataGraph detects its inputs in the codebase, and contributes nothing until then. A single Python repository with no specs, no migrations and no IaC gets a clean code graph and never encounters a cloud or contract concept. The same engine run against a repository that does have those artefacts lights up the corresponding planes automatically. The product value is that wherever multiple planes are present, they form one graph you can traverse in a single query.

1. **Code plane (always on).** Functions, classes, methods, modules, calls, imports, inheritance. The universal core, built for any codebase in any supported language. Cross component impact within this plane alone (which functions break if I change this one, across modules or across repositories) is already the GitNexus and Graphify use case.
2. **Contract plane (activates on specs).** The interface artefacts between components: REST and GraphQL APIs, gRPC and protobuf, event schemas. Activates when such specs are found. Useful within a monolith that exposes an API, not only across services.
3. **Data plane (activates on schema artefacts).** Databases, tables, columns, constraints, views. Activates when migrations, DDL or ORM models are found. The majority of applications have a database, so this is broadly relevant, not a microservice concern.
4. **Infrastructure plane (activates on IaC, provider agnostic).** Cloud and deployment resources read from infrastructure as code. Activates only when IaC is found, and the specific cloud is handled by an adapter (AWS first, then others). On AWS this means IAM roles and policies, Lambda functions, ECS services, EventBridge rules, queues, gateways, RDS instances, buckets, Cognito pools. The same model accommodates Kubernetes manifests, Docker Compose, GCP and Azure through their own adapters.
5. **Knowledge plane (optional, opt in).** The "why": design documents, ADRs, diagrams, PDFs, notes. Model assisted, always tagged as inferred and segregated from the deterministic planes.

Where planes coexist, **cross plane impact** is the flagship: a single traversal can run from an IAM role to a downstream consumer, through a Lambda, its code, an event contract and a table. But the same impact machinery, restricted to the planes that happen to be present, is what makes a humble monolith with a database genuinely useful too.

## 3. Design principles (what makes it bomb proof)

These are non negotiable and shape every later decision.

- **Deterministic first, model optional.** The structural graph is built without any language model and is fully reproducible. Models only ever add the knowledge plane and optional enrichment, never hard dependency edges.
- **Provenance on every edge.** Every relationship records how it was derived and how much to trust it. An inference can never masquerade as a fact. This is the single most important property for a CTO to gate a deployment on the output.
- **Resolved over raw.** Where a source has both a raw form and a resolved form, prefer the resolved form. Parse the Terraform plan rather than guessing at HCL interpolation. Use language server symbol resolution rather than heuristic name matching.
- **Incremental from day one.** Content hash every input, re-parse only what changed, update only the affected subgraph. Not a future roadmap item.
- **Swappable storage, owned correctness.** The reliability critical traversals are owned and tested in the core over an in memory graph. The database is a swappable persistence, search and indexing substrate, so correctness never depends on a single graph engine. This is the lesson of the Kùzu shutdown: teams that put correctness inside a third party engine were stranded when it was abandoned.
- **Local first, hosted optional.** Everything works fully offline on a developer machine. The hosted product adds scale, freshness, collaboration and governance, never core capability.
- **Progressive enhancement, zero required configuration.** The code plane works on any codebase with no setup. Every other plane activates itself on detecting its inputs and is invisible until then. A user never has to declare "this is a microservice" or "this is AWS" to get value.
- **Provider and stack agnostic via adapters.** Languages, contract formats, data stores and cloud providers are all adapters behind stable interfaces. AWS is the first infrastructure adapter because it is the richest, not because the core assumes it. The community can add adapters without touching the core.
- **MCP native.** The primary consumer is an AI agent over the Model Context Protocol. Surviving the IDEs absorbing code intelligence depends on being agent portable.

### 3.1 Capability detection and product switches

StrataGraph decides what to build by detecting what is present, then lets the user override.

- **Detection.** On indexing, a probe step inspects the codebase: languages, and the presence of OpenAPI or GraphQL or protobuf specs, SQL migrations or ORM models, and IaC (and which provider). Each detected capability switches its plane on. Detection results are reported plainly so the user can see what was found and what was activated.
- **Switches.** Every plane and adapter has an explicit switch: auto (the default, follow detection), on (force build even on weak signals) and off (never build, for example to exclude the model assisted knowledge plane in a regulated environment, or to skip IaC parsing in CI). Switches live in a simple project configuration file and as CLI flags.
- **Packaging.** The deterministic planes ship in the core binary. Heavier or specialised adapters (additional cloud providers, language servers, the knowledge plane's model pass) can ship as optional extensions so the base install stays small. A switch being off means its extension need not even be loaded.

The principle: powerful when the inputs are there, silent when they are not, and always under the user's control.

## 4. The unified data model

### 4.1 Provenance and confidence

Every edge (and derived node) carries a provenance tag and a numeric confidence in the range 0 to 1.

| Provenance | Meaning | Typical confidence |
| --- | --- | --- |
| `EXTRACTED` | Read directly from a deterministic source (AST node, spec file, DDL, resolved plan) | 0.95 to 1.0 |
| `RESOLVED` | Derived by a compiler or language server with full symbol resolution | 0.9 to 1.0 |
| `OBSERVED` | Seen in runtime data (query logs, traces, CloudTrail) | 0.9 to 1.0 |
| `INFERRED` | Derived heuristically (string built SQL, naming convention, framework pattern) | 0.4 to 0.8 |
| `AMBIGUOUS` | Multiple candidate targets, none confidently selected | below 0.4 |
| `MODEL` | Produced by an LLM or vision pass (knowledge plane only) | tagged separately, never gates impact |

Impact queries take a minimum confidence threshold. The default for "will break" is high, with lower confidence edges surfaced as "may be affected, review". **Resolved by measurement (2026-06-12, §15.6):** the default will-break cutoff is **0.40**, the lowest band whose measured precision crosses the will-break bar (`INFERRED` measured 1.00, `AMBIGUOUS` 0.53; see `docs/accuracy/ts-resolution.md` and `strata_core::traverse::DEFAULT_WILL_BREAK_CONFIDENCE`). It governs the *label* only; `impact` stays recall-biased and surfaces everything, AMBIGUOUS marked. The label is emitted as a derived `will_break` field on every `AffectedNode`, surfaced through the MCP impact tool JSON, the CLI `impact`/`detect-changes` printers, and the desktop impact table.

### 4.2 Node types by plane

| Plane | Node types |
| --- | --- |
| Code | `Function`, `Method`, `Class`, `Interface`, `Module`, `File`, `Package`, `Repo` |
| Contract | `ApiOperation`, `GraphqlField`, `GrpcMethod`, `EventSchema`, `Topic`, `MessageType` |
| Data | `Database`, `Table`, `Column`, `Constraint`, `View`, `Migration` |
| Infrastructure | Generic: `CloudResource`, `Role`, `Policy`, `Compute`, `EventSource`, `DataStore`, `Network`. AWS adapter specialises these into `IamRole`, `IamPolicy`, `LambdaFn`, `EcsService`, `EventBridgeRule`, `Queue`, `Bucket`, `RdsInstance`, `ApiGateway`, `CognitoPool`, `SecurityGroup`. Other adapters (Kubernetes, GCP, Azure) specialise the same generics. |
| Knowledge | `Document`, `Decision`, `Concept`, `Diagram` |
| Cross cutting | `Process` (a traced execution flow), `Community` (a detected cluster), `Service` (a logical service spanning repos and resources) |

### 4.3 Edge types

Structural code edges: `CALLS`, `IMPORTS`, `EXTENDS`, `IMPLEMENTS`, `DEFINES`, `MEMBER_OF`.

Contract edges: `PRODUCES` (code to contract), `CONSUMES` (code to contract), `IMPLEMENTS_OPERATION`, `PUBLISHES` (code to topic), `SUBSCRIBES` (code to topic).

Data edges: `READS` (code to column or table), `WRITES` (code to column or table), `MAPS_TO` (ORM model to table), `MIGRATES` (migration to table or column), `REFERENCES` (foreign key).

Infrastructure edges: `ASSUMES` (compute to role), `GRANTS` (policy to action on resource), `HAS_POLICY` (role to policy), `RUNS` (compute resource to handler code), `TRIGGERS` (event source to compute), `ROUTES` (rule to target), `ACCESSES` (compute to data store), `REQUIRES_PERMISSION` (code to cloud action, derived from SDK calls).

Knowledge edges: `DESCRIBES`, `DECIDES`, `MENTIONS` (always `MODEL` provenance).

The power is in chaining across edge families in one traversal. `ASSUMES` plus `RUNS` plus `READS` connects a role to a Lambda to its code to a table.

### 4.4 Stable identity (the cross repo unlock)

Cross plane and cross repo linking only works if the same thing has the same identity everywhere. Identity strategy by plane:

- **Code symbols:** use SCIP monikers (Sourcegraph's Code Intelligence Protocol) where an indexer exists, falling back to a deterministic hash of language, package, fully qualified name and signature. SCIP gives globally unique, stable symbol identities, which is exactly what makes producer to consumer linking exact rather than fuzzy.
- **Contracts:** spec native keys, **scoped to an api**. The estate-wide canonical identity of a contract operation is `(api_id, format, key)`, not the bare `(format, key)` it began as, where `key` is the spec-native key (OpenAPI `operationId`, GraphQL `type.field`, protobuf `package.Service/Method`, event schema subject plus version), `format` is the contract format, and `api_id` is the api the operation belongs to. `api_id` resolution is deliberately boring: a manifest-declared `[[repos.apis]]` id when a declared spec owns the operation, else the repo name. This is the **B6 fix**: bare `(format, key)` falsely merged two *unrelated* APIs that happen to share a key (two services both exposing `GET /health`, or two bounded contexts both declaring `Query.getUser`) into one node, confidently dragging an unrelated repo into a blast radius, the one failure the product must never produce. Scoping to `api_id` makes the safe behaviour the default (two repos never merge a shared key; unrelated same-key APIs get distinct nodes), with an explicit opt-in to merge (declare the same `id` in both repos when they genuinely host one API) and an honest fan-out (a consumer whose key is owned by several apis links `AMBIGUOUS` to each, never a silent confident pick). There is intentionally no spec-title tier: generic titles ("API", "Backend") would recreate the very collision being fixed.
- **Data:** fully qualified `database.schema.table.column`.
- **Infrastructure:** the resolved ARN where available, otherwise the IaC address (for example `module.api.aws_lambda_function.handler`).

A **workspace manifest** declares which repositories, services, databases and IaC stacks belong to one system, so the graph knows the boundaries of "your estate". Optional `[[repos.apis]]` entries declare the api identities a repo's specs belong to (the contract-identity opt-in above).

## 5. The extraction pipeline

Tiered, each tier independent and confidence tagged. Tiers 0 to 4 are deterministic. Tier 5 is optional.

**Tier 0, structure.** Walk the file tree, build `File`, `Module`, `Package`, `Repo` nodes. Content hash everything for incremental updates.

**Tier 1, code parsing.** Tree-sitter ASTs for breadth and speed across all supported languages: functions, classes, imports, intra file calls. Provenance `EXTRACTED`.

**Tier 2, code resolution.** Precise cross file and cross repo resolution. Where a SCIP indexer or language server exists for a language, use it for ground truth symbol resolution and call graphs (provenance `RESOLVED`). Where it does not, fall back to heuristic resolution clearly tagged `INFERRED`. This hybrid is how StrataGraph beats the incumbents on accuracy: they rely on heuristics throughout, which is why agent blast radius is sometimes wrong.

Initial languages and their precision strategy (four grammars, since TypeScript and JavaScript share one; **Rust joined the initial set** in development — Tree-sitter extraction with banded confidence, rust-analyzer precision deferred to A3 like the others):

- **C#:** Roslyn, full compilation model, exact symbol, type and call graph. The most precise of the set, almost entirely `RESOLVED`.
- **TypeScript and JavaScript:** Tree-sitter for breadth, the TypeScript compiler API for type and symbol resolution, plus Node module resolution (ESM, CommonJS, workspaces). Strong precision.
- **Python:** Tree-sitter plus pyright, leaning on type hints. The hardest because of dynamism, so unresolved dynamic dispatch is tagged `AMBIGUOUS` rather than guessed.

**Tier 3, contracts.** Parse the actual interface artefacts. OpenAPI and Swagger, GraphQL SDL (including AppSync), protobuf and gRPC, AsyncAPI for events. Build `ApiOperation`, `GraphqlField`, `EventSchema`, `Topic` nodes. Link producer and consumer code to them. Because these come from the spec, the edges are `EXTRACTED`. This is the deterministic cross repo glue.

**Tier 4a, data.** Parse SQL migrations (Flyway, Liquibase, Alembic, Prisma, Drizzle, ActiveRecord), DDL and ORM model definitions into `Database`, `Table`, `Column`, `Constraint` nodes. Link code to schema by ORM mapping (`MAPS_TO`, high confidence), DAO and repository query parsing (`READS` and `WRITES`, medium), and optionally by ingesting real query logs or `pg_stat_statements` in the hosted product (`OBSERVED`, high). Covers RDS and ClickHouse.

**Tier 4b, infrastructure (optional, auto detected, adapter based).** Built only when IaC is detected. The infrastructure plane is provider agnostic: a generic resource model with per provider adapters that recognise the IaC formats and resource semantics of that provider. The AWS adapter is the first and richest. Parsing prefers resolved over raw:

- SAM `template.yaml`: expand through the SAM transform first so shorthand (`AWS::Serverless::Function`) becomes its real resources (function, role, permissions, event sources), then parse. `CodeUri` and `Handler` give the infra to code link directly.
- CloudFormation JSON (and YAML): parse `Resources`, resolving intrinsics (`Ref`, `Fn::GetAtt`, `Fn::Sub`) where possible.
- Terraform and Terragrunt: run to a `terraform show -json` plan as the primary, resolved source (provenance `EXTRACTED`), with static HCL parsing as a lower confidence fallback (`INFERRED`). Terragrunt dependency blocks between modules are themselves infrastructure edges, used to stitch a multi stack estate together.
- OpenTofu: deferred, but close to free later since it shares the plan JSON format.

Derived links that complete the cross plane reach: IAM policy statements reconciled against AWS SDK calls detected in code (`REQUIRES_PERMISSION` versus `GRANTS`, for gap detection), and Lambda environment variables and resource references connecting compute to an `RdsInstance` and onward to its tables.

Build infrastructure nodes (`IamRole`, `LambdaFn`, `EventBridgeRule`, `RdsInstance` and so on) and the edges between them (`ASSUMES`, `HAS_POLICY`, `GRANTS`, `TRIGGERS`, `ROUTES`, `ACCESSES`). Then the two links that make it powerful:

- **Infra to code:** map each compute resource to its handler code via the SAM or CDK handler path or deployment artefact (`RUNS`).
- **Code to required permission:** statically detect AWS SDK calls in the handler code (for example `dynamodb:PutItem`, `s3:GetObject`) and emit `REQUIRES_PERMISSION` edges, so they can be reconciled against what the role actually grants. This yields IAM gap detection, which is a security feature, not just an impact feature.

**Tier 5, knowledge (optional).** An opt in LLM and vision pass over ADRs, design docs, PDFs and diagrams, attaching `Document`, `Decision` and `Concept` nodes with `MODEL` provenance, visually and structurally segregated from the deterministic graph.

After extraction: cluster (Leiden community detection over the code and contract planes), trace processes (execution flows from entry points), and build hybrid search indexes (lexical plus semantic with reciprocal rank fusion).

## 6. Cross boundary impact: the flagship capability

Four worked scenarios. The openCypher below is illustrative of the traversals, not final syntax. Crucially, impact queries degrade gracefully: each traversal returns whatever planes are present. Scenario 6.1 and 6.2 need only code, contracts or a database and apply to most codebases including a monolith. Scenario 6.3 and 6.4 are the infrastructure plane at work and appear only when IaC is detected, shown here with the AWS adapter.

### 6.1 Change an API producer, find downstream consumer repos

```cypher
MATCH (changed:Function {uid: $symbol})-[:PRODUCES]->(op:ApiOperation)
MATCH (op)<-[c:CONSUMES]-(consumer:Function)
MATCH (consumer)-[:MEMBER_OF*]->(r:Repo)
WHERE c.confidence >= $minConfidence
RETURN r.name AS repo, op.operationId AS contract,
       collect(DISTINCT consumer.name) AS affected, c.confidence
ORDER BY c.confidence DESC
```

Result: the exact consumer repositories and functions that depend on the contract, with confidence. The same shape works for GraphQL fields and event schemas, which covers AppSync and EventBridge or Kafka.

### 6.2 Change an RDS column, find dependent services

```cypher
MATCH (col:Column {uid: $column})
MATCH (col)<-[rw:READS|WRITES]-(code:Function)
MATCH (code)-[:MEMBER_OF*]->(svc:Service)
OPTIONAL MATCH (code)-[:PRODUCES]->(op:ApiOperation)<-[:CONSUMES]-(downstream:Function)
RETURN svc.name AS service,
       collect(DISTINCT code.name) AS direct_touchpoints,
       collect(DISTINCT op.operationId) AS exposed_contracts,
       collect(DISTINCT downstream.name) AS downstream_consumers,
       min(rw.confidence) AS confidence
```

Result: not just the services that touch the column directly, but the contracts they expose and the downstream consumers of those contracts. The blast radius crosses both the data boundary and the service boundary in one query.

### 6.3 Change an IAM role, find dependent compute and its reach

```cypher
MATCH (role:IamRole {uid: $role})
MATCH (role)<-[:ASSUMES]-(compute)
MATCH (compute)-[:RUNS]->(handler:Function)
OPTIONAL MATCH (handler)-[:READS|WRITES]->(data)
OPTIONAL MATCH (handler)-[:PRODUCES|PUBLISHES]->(contract)
RETURN labels(compute) AS kind, compute.name AS resource,
       handler.name AS entrypoint,
       collect(DISTINCT data.uid) AS data_reached,
       collect(DISTINCT contract.uid) AS contracts_served
```

Result: every Lambda or ECS task that assumes the role, the code each runs, and what that code reaches in the data and contract planes. Altering or removing the role shows its full operational footprint before you apply.

### 6.4 IAM permission gap detection (security)

```cypher
MATCH (handler:Function)-[req:REQUIRES_PERMISSION]->(action:CloudAction)
MATCH (compute)-[:RUNS]->(handler)
MATCH (compute)-[:ASSUMES]->(role:IamRole)-[:HAS_POLICY]->(p:IamPolicy)
WHERE NOT (p)-[:GRANTS]->(action)
RETURN compute.name AS resource, handler.name AS code,
       action.name AS missing_permission
```

Result: code that calls an AWS action its role does not grant. The reverse query finds over provisioned roles, granting actions no code uses, which is least privilege tightening. For a security focused engineering organisation this alone is a compelling reason to adopt.

## 7. Graph storage architecture

The decision is shaped by the Kùzu shutdown of October 2025: the embedded engine GitNexus was built on was archived and abandoned, with very few people able to maintain the fork. The robust answer is to not let any single graph engine own our correctness.

**The traversal engine is ours, in memory, in Rust.** The reliability critical traversals (impact, context, schema impact, permission gap, process tracing) run as owned, tested algorithms over an in memory typed graph held as a compressed sparse adjacency structure. Blast radius is bounded depth over a graph that fits in memory, so this is microsecond fast and, crucially, exhaustively testable with golden fixtures. Correctness lives here, not in a database query planner.

**The database is a substrate, chosen for reliability not graphiness: DuckDB.** It is among the most reliable, fastest and best maintained embedded databases available, MIT licensed, in process, single file, columnar (ideal for the scan and aggregate parts of impact and search), with first class Rust bindings, full text search, and an HNSW vector extension so hybrid search needs no second store. The graph persists as vertex and edge tables and is loaded into the Rust engine for traversal. DuckPGQ, the SQL/PGQ extension from the DuckDB research group, benchmarks competitively against Neo4j and is offered as a standards based query escape hatch, but as a maturing community extension it stays off the reliability critical path. SQL/PGQ being part of the SQL:2023 standard makes it a durable bet.

**Hosted service: FalkorDB** behind the same storage trait for multi tenant scale, with openCypher. Run as a managed service, so its SSPL licence is fine there, but it is never bundled into the source-available local build. Neo4j Enterprise is the alternative; avoid Neo4j Community on the managed-service side for licensing reasons.

One `GraphStore` trait, multiple backends, correctness owned in the core. Bomb proof here means no single database can strand us.

## 8. Engine architecture

A **Rust core** does the heavy lifting: file walking, Tree-sitter orchestration, incremental diffing, graph building and query coordination. Rust buys three things: speed and safe concurrency (rayon) on a long running indexer, memory safety, and a single signed static binary with no runtime to install. That last point is a headline UX advantage, because both incumbents inflict toolchain pain (Python for one, Node and sometimes a C++ toolchain for the other).

**Language analysers as adapters.** Tree-sitter in process for breadth. Out of process SCIP indexer and language server runners for precision on tier 1 languages. An adapter interface so the community can add languages without touching the core.

**Spec and IaC parsers** as a parallel adapter family: OpenAPI, GraphQL, protobuf, AsyncAPI, Terraform plan JSON, CloudFormation, CDK synth.

**MCP server**, first class, over stdio and HTTP, serving all indexed repositories and the unified estate graph. Plus agent hooks (pre and post tool use) and generated agent skills, matching the deepest integration the incumbents offer so agents consult the graph before searching and reindex after commits.

Scope languages tightly at first to the agreed set (TypeScript, JavaScript with Node resolution, Python and C#) and do them excellently. Depth on the boundaries beats breadth on the basics.

### 8.1 Reliability engineering: how we guarantee blast radius

The product promise is that we will not let a blind breaking change ship. That promise is only as good as the reliability discipline behind it, so it is engineered explicitly.

- **Owned, tested traversals.** Blast radius and the other critical traversals are Rust algorithms over the in memory graph, covered by a golden fixture corpus of repositories with hand verified blast radii. CI fails on any regression.
- **Recall biased for safety.** For blast radius we prefer false positives to false negatives. Missing a real dependency is the unforgivable failure; over flagging a safe one is a triage cost. We target near total recall on true dependencies and use calibrated confidence to triage the noise.
- **Differential testing against ground truth.** Build the graph the fast way (Tree-sitter) and the precise way (Roslyn, the TypeScript compiler, pyright), treat the compiler as truth, and measure and bound the fast path's precision and recall per language as a gated metric.
- **Calibrated confidence.** A 0.9 edge must be correct about ninety percent of the time, validated against ground truth, so "will break" versus "may be affected" is meaningful rather than decorative.
- **Explicit unknowns, never silent gaps.** When resolution fails (reflection, dynamic dispatch, string built SQL, runtime constructed ARNs) we emit an `AMBIGUOUS` marker and surface the unanalysed path. A blast radius that silently omits a path it could not analyse is worse than one that admits it.
- **Incremental equals full.** Incremental updates are periodically reconciled against a full rebuild and must produce an identical graph. Output is deterministic and content addressed.

## 9. Frontend and desktop app

Build the local app with **Tauri**: a Rust backend (the engine itself) with a web based UI shell, cross platform, small and signed. This is the "Rust frontend" done pragmatically.

For graph visualisation, render with a WebGL library (Sigma.js with Graphology, or Cosmograph) rather than a native Rust GUI, because the large graph drawing ecosystem is far more mature there. The same web UI is reused as the hosted product front end.

UI priorities: a plane filter (show or hide code, contract, data, infrastructure, knowledge), confidence as a visual weight on edges, and an impact view that animates a blast radius outward from a selected node across planes.

## 10. Collaboration layer

Treat the graph as shared infrastructure, not a personal cache.

- **Local OSS:** commit the graph artefact to the repository with a merge driver so parallel commits union cleanly. This pattern is already proven by Graphify.
- **Hosted:** a continuously fresh org graph built in CI, presence, comments and annotations on nodes so the "why this exists" lives next to the code, saved and shared impact analyses, a PR bot that posts blast radius and flags changes touching a high risk shared contract, a hot table or a shared role, and Slack or Teams alerts on those events.

## 11. MCP tool and resource surface

Tools (per repo and estate wide). **Shipped today** (the exact seven the server
advertises):

- `context` (360 degree view of a symbol, table, contract or resource across planes)
- `impact` (cross plane blast radius with confidence and depth)
- `explain` (the evidence chain behind an impact result: each hop's kind, provenance and running confidence)
- `query` (lexical search over name / fqn / path; hybrid search is roadmap, below)
- `blast` (a file's pre-edit blast radius: its symbols, their dependents, and a risk level)
- `detect_changes` (git diff mapped to affected nodes across planes, with operation-level breaking/additive labels on contract changes)
- `rename` (graph assisted coordinated rename)

**Roadmap** (designed, not yet shipped):

- `permission_gap` (IAM reconciliation — the Grants supply side is built; the demand side and the reconciliation traversal are Track D2)
- `schema_impact` (column or table change reach — today subsumed by `impact` on a data-plane node at table granularity)
- `cypher` (raw openCypher escape hatch, kept off the reliability-critical path per §7)

Resources: estate overview, per repo context and staleness, clusters, processes, contracts, schema, infrastructure topology, and the graph schema for query construction.

## 12. Security, privacy and governance

- Local mode makes no network calls, the graph stays on disk.
- The optional model pass transmits only what the user opts into, under their own provider key.
- Hosted governance is what a CTO would buy a managed option for: SSO, SCIM, RBAC, audit log, and a historical graph for time travel ("what did the impact look like before this refactor"). The capabilities are source available; the managed option would sell operating them.
- Signed release artefacts and supply chain attestation from day one.

## 13. Source model and licensing

The whole project is **source available** under the Functional Source License (FSL-1.1-ALv2): readable, free for any non-competing use, and each release becomes Apache 2.0 two years after it ships. The licence is chosen to build trust and maximise adoption while keeping a future hosted option viable. There is no crippled core and no paid tier: the entire suite is in this repository and self-hostable.

**The full engine, source available:** single repo and multi-repo estate graphs, all deterministic extraction tiers including data and infrastructure, the MCP server, hooks and skills, the CLI, the desktop app, local visualisation, and the commit the graph team workflow. Generous on purpose, because adoption is the moat. The org-wide capabilities below ship here too as they are built, under the same terms.

**Optional future managed hosting:** a managed always fresh org wide graph at scale, continuous CI indexing, the PR review bot, real time collaboration and comments, query log and CloudTrail driven linking, the historical graph, SSO, SCIM, RBAC, audit, SLA and support. If offered, this is sold as operation and convenience for teams that would rather not run it themselves, never as a crippled core: the source stays available. The reason to pay would be operational, not access to the features.

## 14. Phased roadmap

**Phase 0, parity proof.** Rust core, Tree-sitter, owned in memory traversal engine over an embedded DuckDB store, single repo code graph, MCP server with `context`, `impact`, `query`, single binary install, working in Claude Code and Kiro.

**Phase 1, the contract wedge.** Contract extraction (OpenAPI, GraphQL, protobuf, AsyncAPI), SCIP based stable identity, workspace manifest, cross repo impact demo.

**Phase 2, the data and infrastructure moat.** Schema graph from migrations and ORM models, code to table links, IaC ingestion from Terraform plan JSON and CDK synth, the IAM role impact demo and permission gap detection. This is the headline that no competitor matches.

**Phase 3, precision and freshness.** SCIP and LSP adapters for the tier 1 languages, incremental indexing throughout, confidence and provenance surfaced everywhere in the UI.

**Phase 4, knowledge.** The optional multimodal pass for design context, ADR and diagram linking.

**Phase 5, collaboration and hosted.** Desktop polish, web app, multi tenant hosting, PR bot, governance, billing.

### 14.1 Roadmap status and committed deliverables (updated 2026-06-11)

**Done (merged to develop):** the TS/JS code-plane spine (single `strata` binary, MCP + CLI); SCIP compiler-grade resolution with calibrated, band-capped confidence and published accuracy reports; the contract plane with workspace-manifest estates and cross-repo impact for **OpenAPI** and **GraphQL** (incl. AppSync SDL and untagged document constants); the **infrastructure plane AWS vertical** (event-level SAM/CFN parsing, AppSync money link, infra→contract→frontend traces live on the dogfood repo); the **desktop GUI** (Tauri + Sigma WebGL graph view, contract-aware context buckets, in-app reindex/Index Now, open-project-folder flow); **agent integration** (`strata init claude|kiro`, estates over MCP, `include_contracts` everywhere); plus dogfood-driven fixes throughout (contract-target impact, parse-gated untagged docs, dead-field surfacing).

**Committed next deliverables, in order:**
1. **Infrastructure plane, first vertical (in progress).** AWS adapter starting where the dogfood repo points: static SAM/CloudFormation parsing → resource nodes (`LambdaFn`, `IamRole`, AppSync API/resolver/data-source, generic `CloudResource` inventory) and the wiring edges, then the money link (AppSync resolver → `GraphqlField` (`PRODUCES`)), completing infra → contract → frontend traces. Terraform plan JSON, Terragrunt, and IAM gap detection follow as the plane matures. Static-parse provenance is honest (`EXTRACTED` for literals, `INFERRED` for interpolations, surfaced unknowns); resolved-plan ingestion remains the preferred source when available (§5 Tier 4b).
2. **Desktop GUI (Rust).** Per §9: a Tauri app (the Rust engine in-process) with search and WebGL graph visualisation (plane filters, confidence-weighted edges, animated blast-radius view) so the product's visibility matches its engine. Same UI shell reused for the future hosted front end.
3. **Agent integration: steering files + hooks.** First-class, GitNexus-depth integration for **Claude Code and Kiro first**: generated steering/skills files, pre-tool-use enrichment hooks (consult the graph before searching), post-tool-use staleness/reindex hooks, and a one-command install per tool. The bar is that engaging with StrataGraph from a coding agent must be effortless. Includes serving estates over MCP (`strata mcp --workspace`).

4. **Python code capability (committed 2026-06-11).** A `strata-lang-py` analyzer behind the existing `LanguageAnalyzer` trait: Tree-sitter-python extraction (functions, classes, methods, imports, calls) with the same honest provenance discipline (`AMBIGUOUS` over guessed dynamic dispatch), wired into the indexer alongside TS/JS. Unlocks the dogfood repo's 39 Python Lambda handlers: `Runs` edges land on real code and Python repos get the full code plane. pyright-backed precision follows later per §5 Tier 2.
5. **C# code capability (committed 2026-06-11, per §15 decision 2).** `strata-lang-cs` behind the same trait: Tree-sitter-c-sharp extraction first (classes, methods, interfaces, usings, calls; heuristic with banded confidence, same as the TS slice-1 pattern), Roslyn-backed compiler precision as the follow-up (the cleanest precision win per §15.5).

### 14.2 Completion & Robustness Program (committed 2026-06-11)

Mark's directive: *"totally overcome the documented limitations, extremely thorough and robust."* This section is the audit of every recorded doubt, deferral, and caveat, organized into committed tracks. Each track lands via the established gated workflow (spec → plan → TDD build → independent review → live dogfood).

**Track A: Languages (first, the committed wedge).**
A1 Python (item 4 above). A2 C# (item 5). A3 pyright/Roslyn/TS-compiler precision rollout per §15.5, order resolved: TS (done, SCIP) → C#/Roslyn → Python/pyright.

**Track B: Complete the impact story (the §6 flagship scenarios still open).**
B1 **Role-impact traversal** *(ELEVATED 2026-06-12, user-found bug: an `IamRole` in the GUI shows nothing depending on it, "totally pointless"; the graph has the `Assumes` edges, the views don't surface them; pulled ahead of Track A2/C#)*: `impact` follows `Assumes`/`Routes`/`Runs` as dependency edges (role → its assuming Lambdas → their produced operations → consumers; a handler module → its Lambda → reach); `context` gains infra buckets (`assumes`/`assumed_by`, `routes_to`/`routed_from`, `runs`/`run_by`), completing §6.3 ("change an IAM role, find dependent compute and its reach"). B2 **`Fn::If` grading policy**: surface both branches as `Inferred` (the event parser preserves the structure; today's `Unresolved` under-claims). B3 **AppSync `ApiId` edges** (deferred in infra-linking.md). B4 **Non-root resolver coverage** (visible, not silently excluded). B5 **`detect_changes`** (git-diff → affected symbols/flows, the missing pre-commit tool the agent kit's protocol currently does manually) and **`rename`** (graph-aware multi-file rename with confidence-tagged edits), the two GitNexus-parity tools agents need most (Appendix A). B6 **Estate identity**, audit run 2026-06-11, hypothesis **CONFIRMED, Critical**: `(format, key)` canonical dedup falsely merges unrelated APIs sharing an operation key (empirically: a user-service schema change confidently reported a billing-only frontend as affected at 0.76, unflagged; `GET /health`-class collisions are near-universal in microservice estates; the intended `Ambiguous`-on-collision safeguard only fires within one repo, never across repos). **FIXED 2026-06-12 (slice 8, reviewed):** canonical key is now `(api_id, format, key)` with deliberately boring precedence: manifest-declared `[[repos.apis]]` id → safe per-repo namespace default. (The audit's suggested spec-intrinsic `info.title` tier was consciously DROPPED at implementation: generic titles like "API" would recreate the very collision; §4.4 is the authoritative identity story.) Consumers matching a key in N distinct APIs emit N `Ambiguous` 0.35 edges, never a silent confident pick. Note: declared ids share one slug namespace with repo names, so a declaration equal to another repo's name merges with that repo (merging always requires a positive declaration, never happens between two defaults). Reviewer's verdict: an estate can never again produce a confident (≥0.4) cross-API edge between undeclared APIs. Manifest v2 data-store and IaC-stack declarations remain assigned to Track D (resolves §15.8 when they land).

**Track C: Accuracy at scale (the caveat every report carries).**
C1 **Corpus expansion**: IN PROGRESS. The TS resolution corpus grew to 56 call sites / 50 adjudicable across six fixtures (re-exports, inheritance/override, async/HOFs, overloads + dynamic access) with per-class + per-band precision/recall measured and the report reissued (`docs/accuracy/ts-resolution.md`, 2026-06-12); real-repo corpora (a private real-world codebase + selected OSS monorepos) and the other-language corpora remain to be grown. C2 **Calibration measurement**: DONE for TS. Empirical per-band confidence calibration (does 0.9 mean 90%? INFERRED measured 1.00, AMBIGUOUS 0.53), the §4.1 monotonicity invariant, and CI floors all shipped; this also *resolved §15.6 (recall-vs-noise default) with data instead of judgment*: the default will-break threshold is the measured band boundary (the INFERRED floor, 0.40). C3 **Differential testing** against compiler ground truth (tsc/SCIP as oracle) per §3's bomb-proof principles: the differential harness drives the C1/C2 measurement.

**Track D: Infrastructure & data moat completion (Phase 2 of §14).**
D1 **Terraform plan JSON + Terragrunt** ingestion (the dogfood repo's `infrastructure/` tree is Terragrunt, currently invisible). D2 **IAM permission gap detection** (§6.4). D3 **Data plane** (§5 Tier 4a, §6.2): migrations/ORM models → table/column nodes, code→table links, the "change an RDS column" demo, static-first with honest bounds (resolves §15.7: static parsing now; runtime query-log ingestion stays a future runtime-observation step). D4 **protobuf + AsyncAPI** contract formats (Phase 1 remainder).

B7 **Ambiguous-symbol ergonomics on `impact`/`explain`** *(user-found: `impact <ambiguous-name>` returned an "ambiguous symbol: N candidates" hard error, a dead-end)*: `context` already returns candidates on ambiguity; `impact`/`explain` must do the same: list the candidates (uid/name/kind/path) and accept a `--uid` pin, never hard-error and never silently pick one.

**Track E: Product surface excellence.**
E1 **UI: path explanation**: click any impact result and see *why* (the evidence chain: each edge, its kind, provenance, confidence), accuracy made legible; the single highest-trust UI feature. E2 **UI: estate/graph overview** (clusters/communities once Leiden lands; until then plane-level overview), **index-diff view** ("what changed since last index", pairs with B5), node/subgraph **export** (PNG/JSON). E3 **MCP hot-reload** on db change (DONE): the server auto-reloads the served graph when the on-disk index changes (degrade-safe; keys off `.strata/index.stamp` with a `graph.duckdb`-mtime fallback), single-db and estate. E4 **Windows/Linux**: agent-kit hooks portable (today `sh -c`), desktop packaging beyond macOS. E5 Desktop hardening: CSP (today null), bundle signing. E6 `strata init --remove`; Cursor/Windsurf/Copilot adapters. E7 GitNexus-parity analytics per Appendix A: hybrid search (BM25+semantic+RRF), Leiden communities, process/flow detection, wiki generation.

**Sequencing (committed):** A1 → A2 → B1+B2+B3 (one infra-traversal slice) → B5 (detect_changes/rename) → C1+C2 (accuracy program) → D1 → D3 → E-track items interleaved where they unblock trust (E1 path-explanation early, since it rides on existing traversals). B6 (estate audit) runs as an immediate standalone review task: correctness questions don't queue.

## 15. Decisions and remaining open questions

**Decided.**

1. **Storage.** Owned in memory Rust traversal engine for correctness, DuckDB as the embedded substrate (storage, full text and vector search via HNSW), DuckPGQ as an optional standards based query escape hatch, FalkorDB for the hosted multi tenant service, all behind one `GraphStore` trait. Driven by the Kùzu shutdown: no single graph engine owns our correctness.
2. **Initial languages.** TypeScript, JavaScript with Node resolution, Python, C# — and Rust, added during development (`strata-lang-rust`, Tree-sitter, its own measured accuracy report). Precision via the TypeScript compiler/SCIP (shipped), then Roslyn (C#), pyright (Python) and rust-analyzer (Rust) per §15.5.
3. **IaC ingestion.** SAM `template.yaml` (transform expanded), CloudFormation JSON, and `terraform show -json` plans, supporting Terraform and Terragrunt. OpenTofu later. Resolved plan preferred over raw HCL throughout.
4. **Reliability stance.** Recall biased blast radius, owned and golden tested traversals, differential testing against compiler ground truth, calibrated confidence, explicit unknowns. Full GitNexus parity and Graphify incorporation are in scope (Appendix A).

**Previously open, now resolved or assigned (2026-06-11, see §14.2):**

5. **Resolution rollout order.** RESOLVED: TS first (shipped, SCIP), then C#/Roslyn (Track A2/A3), then Python/pyright. Tree-sitter heuristic extraction with banded confidence ships first for each language; compiler precision follows, the proven TS pattern.
6. **Recall versus noise default.** RESOLVED by measurement (2026-06-12, Track C2): the per-band calibration (`docs/accuracy/ts-resolution.md`) measured `INFERRED` precision **1.00** and `AMBIGUOUS` **0.50** against SCIP, so the band boundary where precision crosses the will-break bar is the **INFERRED floor, 0.40**, now the default will-break threshold (`strata_core::traverse::DEFAULT_WILL_BREAK_CONFIDENCE`). It governs the **label** only; `impact` stays recall-biased (`min_confidence = 0.0`, surfaces everything), and `AMBIGUOUS` stays surfaced-but-marked everywhere (the GUI already renders it as a reserved channel). Re-derived automatically as the corpus grows.
7. **Data layer linking depth.** RESOLVED: static-first with honest bounds (Track D3). Migrations/ORM models are parsed statically with banded confidence and explicit unknowns; runtime query-log ingestion remains a future runtime-observation enhancement, not a blocker.
8. **Estate manifest format.** ASSIGNED to Track B6: manifest v2 adds explicit api-identity scoping (so two unrelated APIs sharing operation names can never falsely merge) plus declarations for data stores and IaC stacks; the dedup-collision audit runs immediately.

---

## Appendix A: feature parity and incorporation

**GitNexus functionality to match in full**, extended across all planes rather than code only:

- MCP tools: `context`, `impact`, `query`, `detect_changes`, `rename` (all shipped, plus the shipped `explain` and `blast`); `cypher`, `list_repos`, and the group tools (`group_sync`, `group_contracts`, `group_query`, `group_status`, `group_list`) remain parity roadmap.
- MCP resources: estate and per repo context, clusters, processes, contracts, schema, infrastructure topology, graph schema.
- MCP prompts: pre commit impact analysis and architecture map generation.
- Agent skills: Exploring, Debugging, Impact Analysis, Refactoring, plus per area skills generated from detected communities.
- Hooks: pre tool use enrichment so the agent consults the graph before searching, post tool use stale detection and reindex prompts after commits.
- Multi repo registry serving many indexed repositories from one MCP server.
- Hybrid search: lexical (BM25) plus semantic plus reciprocal rank fusion.
- Leiden community detection, process (execution flow) detection, confidence scoring.
- Wiki and documentation generation from the graph.
- Graph visualisation in the desktop and web UI.

**Graphify capabilities to incorporate:**

- Multimodal extraction: documents, PDFs, images and diagrams via a vision pass, feeding the knowledge plane.
- A provenance and confidence model (richer than Graphify's three tags).
- The committed graph team workflow with a merge driver for parallel commits.
- Clustering that does not require an embedding store.
- Broad assistant support and a slash command install across many AI coding tools.
