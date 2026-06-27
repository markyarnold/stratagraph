# Honest limitations

This page is the deliberate counterweight to [the results](./results.md): the
full, plain list of what StrataGraph does **not** do today, and where its bounds are.
It is here because the product's thesis depends on it. A tool that claims to be
"never confidently wrong" earns that claim only if it states its limits as
clearly as its capabilities; otherwise the next unstated gap is exactly the
confident-wrong answer the design is built to avoid.

The unifying principle across every item below: **a known limitation surfaces as
`AMBIGUOUS` or as an absent edge, never as a wrong one.** When StrataGraph cannot
resolve something precisely, it either fans out at low confidence (flagged,
below the will-break bar) or emits no edge and counts the miss. It does not invent
a confident link to cover a gap. Each limitation here is a documented, often
test-pinned bound, not a silent failure mode.

Capabilities that are *planned but not built* are also tracked on the
[roadmap](../project/roadmap.md); this page describes the bound as it stands
today.

## Call-resolution granularity and precision

### Compiler-grade precision exists only for TypeScript so far

StrataGraph's call resolution is **heuristic** in the shipping graph, with a
compiler-grade SCIP oracle used to *measure* it (see
[methodology](./methodology.md)). A full compiler-precision resolution path,
where the graph itself carries `RESOLVED`-grade edges from a language server, is
built for **TypeScript** and deferred for the other languages (tracked as A3).
The practical effect: outside TypeScript, the shipping call edges are `INFERRED`
or `AMBIGUOUS` heuristics, capped strictly below compiler confidence. They are
measured to be accurate within their bands (see [results](./results.md)), but
they are honestly *guesses*, banded as such, not type-system facts.

### Dynamic and instance-method dispatch is heuristic, not resolved

A method call on a value whose type StrataGraph cannot statically determine
(`obj.method()` with no resolvable receiver type) is the canonical `AMBIGUOUS`
case. StrataGraph fans out to **every** same-named method in the repository and bands
the whole set below 0.40. This preserves recall (the real target is in the set if
it exists first-party) at the cost of precision, and the over-inclusion is exactly
what the `AMBIGUOUS` band measures. It is never silently narrowed to a single
confident guess. (For TypeScript, the SCIP overlay *does* resolve many of these
to the one real target; for Python, Rust, and C# they remain banded fan-outs
until their compiler-precision tracks land.)

### Class instantiation is not modelled as a call edge

Constructing an object (`new Widget()`, `Widget(...)`, `Rectangle::new()` used as
a constructor) is **not** emitted as a `CALLS` edge to the constructor in the
heuristic graph. A compiler oracle (SCIP) resolves a constructor call to the
class, so in the measured corpora these appear as **recall misses**: surfaced and
counted honestly, never inflating precision. If you need "who constructs this
type?", that relationship is not yet a first-class edge.

### Inheritance is not yet an edge in the heuristic planes

The Python, Rust, and C# analyzers parse a type's bases for the header but do
**not** emit `Extends` / `Implements` edges (the shared `AnalyzedFile` model
carries no bases field). A `this.method()` / `self.method()` call that resolves
only via a base type is therefore an honest **miss**, not an invented edge:
the enclosing-class rule does not climb the inheritance chain. (TypeScript's SCIP
overlay does resolve inherited calls; the heuristic itself does not.)

## Data plane

### Table-level, not column-level, granularity

The data plane links code to **tables**, not to individual columns. An ORM model
or a SQL statement that touches a table produces a table-granular relationship;
StrataGraph does not currently track which *columns* a given query reads or writes. So
"what breaks if I drop this column?" is not answerable at column precision today,
only "what touches this table?". This is a deliberate granularity bound, stated so
you do not over-read a table-level link as a column-level guarantee.

### ORM linking is explicit-name only

The ORM-to-table link (`MapsTo`) is emitted only when a model **explicitly names**
its table (e.g. an explicit `__tablename__` / table-name attribute). Convention-
based table naming (a framework pluralizing a class name), and the schema-inference
ORMs (Drizzle, Prisma) and C#'s Entity Framework, are **deferred**: a model that
relies on convention rather than an explicit name produces no `MapsTo` edge rather
than a guessed one. The absence is the honest signal; see the canonical report
`docs/accuracy/data-linking.md` for exactly what is and is not linked.

## Contract plane

### gRPC / protobuf is extraction-only (no producer/consumer code-linking yet)

StrataGraph extracts gRPC services, methods, and messages from `.proto` files into the
contract plane, but it does **not** yet link those operations to the code that
**implements** (produces) or **calls** (consumes) them: the producer/consumer
code-linking that the GraphQL and OpenAPI surfaces have. So a gRPC method appears
as contract surface, but its `producers` / `consumers` buckets are not yet
populated from code. Treat gRPC as *visible but not yet cross-linked*; see the
canonical report `docs/accuracy/grpc-linking.md`.

### AsyncAPI is not yet supported

Event-driven contract surfaces described with AsyncAPI are **not** extracted
today. There is no AsyncAPI plane; an AsyncAPI document is not indexed as contract
surface. This is a stated gap, not a partial implementation.

### C# contributes no contract surface

A C# file is a full code-plane citizen, but the C# plane emits **no**
routes / HTTP-call / GraphQL contract producers or consumers. ASP.NET routing
attributes (`[HttpGet]`, `[Route]`) and `HttpClient` consumer calls are a later
enhancement. So C# code does not yet appear as a producer or consumer of contract
operations.

## Infrastructure plane

### IAM permission-gap detection is half-built (grants yes, reconciliation no)

The `Grants` half ships: a role's allowed actions are extracted into `CloudAction`
nodes and `Grants` edges from CloudFormation/SAM and Terraform policies, with any
un-enumerable grant (a managed-policy ARN, a `Deny`, a `data`-source policy
document, a policy attachment) marked `<opaque:…>` so the role is treated as
INDETERMINATE. You can see a role's grants today in `context`.

What is **not built yet** is the other half and the reconciliation: detecting the
AWS actions code actually calls (`RequiresPermission`, from boto3 / AWS SDK v3)
and the `permission_gap` traversal that flags where a Lambda calls an action its
role does not grant. So StrataGraph does not yet tell you about a gap, and by the
opaque rule it will only ever flag one when the role's grant set is fully known.
See the [roadmap](../project/roadmap.md) for the order of what is coming.

### C# Lambda handlers are deliberately not resolved (counted, not guessed)

A .NET Lambda's handler is a CLR reference
(`Assembly::Namespace.Type::Method`), **not a file path**, so it cannot be mapped
to the indexed `.cs` module without a `.csproj`/assembly-name resolution step that
is out of scope today. Rather than guess, the C# plane is deliberately excluded
from the handler-resolution set, and a C# Lambda handler is counted as
**unresolved**: surfaced honestly and pinned by a test, not linked to a
plausible-looking wrong module. (Contrast Python, whose file-path handlers do
earn their `Runs` edge.)

## Language coverage

### C# has no measured resolution-precision number

As stated on [the results page](./results.md), C# is **extraction-coverage only**.
There is no per-band resolution-precision report for C# because that requires a
`scip-dotnet` oracle and the .NET SDK, unavailable in the current build
environment. The C# linker is band-disciplined and honest by construction (it
caps every edge below compiler grade and counts what it leaves unlinked), but it
is **not yet graded against an oracle**. Do not read the C# extraction coverage as
a precision claim.

## Corpus and freshness caveats

### The measured numbers are starting calibrations on small corpora

Every resolution-precision number in this section is measured on a **small,
committed, hermetic corpus**: tens of call sites per language, hand-built to
exercise the hard cases. They are honest and test-pinned, but they are **not**
statistically authoritative samples of real-world code at these sizes. They are
*starting calibrations* that sharpen (and whose CI floors are re-derived) as the
corpora grow. Read a band precision as "measured to be at least this good on the
committed corpus," not as a population-level guarantee.

### The index is a snapshot (mitigated by hot-reload)

StrataGraph answers from an **index** built at a point in time, not from the live
working tree. If code changes after indexing and before a query, the graph can be
stale until the next index. This is mitigated, not eliminated: the index is
refreshed by a post-edit hook, and the MCP server **hot-reloads** the fresh graph
before the next request when the on-disk index changes (degrade-safe: a reindex
caught mid-write keeps the previous graph and retries). But between an edit and
its reindex, an impact answer reflects the last indexed state. Treat a result as
"as of the last index," and re-run after large changes.

## How to use these limitations

None of the above is a reason to distrust a StrataGraph answer; it is the opposite.
The value of an honest limitations list is that it tells you *which questions
StrataGraph answers with confidence and which it does not*. When StrataGraph gives you a
high-band link, these bounds are why you can trust it; when it gives you an
`AMBIGUOUS` fan-out or no edge at all, these bounds are what it is honestly
telling you it cannot yet resolve. The discipline is the same everywhere: surface
the uncertainty, never launder it into a confident answer.

For the broader picture of what is built versus planned, see
[Languages and coverage](../concepts/coverage.md) and the
[roadmap](../project/roadmap.md).
