# gRPC / protobuf Contract Coverage

Measured coverage of Strata's **contract-plane gRPC support**: how much of a
`.proto` estate the Slice-17 (Track D4a) protobuf adapter turns into contract
operations, and how those operations are identified estate-wide. This is the gRPC
companion to `docs/accuracy/openapi-linking.md` and `docs/accuracy/graphql-linking.md`.

## Scope: M1 is operation EXTRACTION, not code linking

**This milestone (M1) extracts `.proto` `service` definitions into `ApiOperation`
nodes and unifies them across an estate by the `(api_id, format, key)` identity.
It does NOT link producer or consumer CODE to those operations.** That is M2,
where it needs honest-banded code signals (a gRPC server `impl` → the rpc it
serves, a generated stub call → the rpc it invokes), exactly as the GraphQL plane
started with schema/operation substrate before its resolver/document linking.

So the numbers below are an **operation-node tally**, not a `PRODUCES`/`CONSUMES`
link-coverage table. There are deliberately **zero** gRPC contract edges at M1;
the operation nodes carry no confidence edge, so the §4.1 band guardrail is
unaffected (there is nothing yet to band).

The report is kept honest by a consistency test the same way the other reports are:

- **`tests/grpc_estate.rs::grpc_report_matches_committed_node_tally`** asserts the
  live estate's contract-node tally equals the numbers tabulated here (so this
  report cannot silently drift from the code).

Regenerate the raw figures with:

```
cargo test -p strata-index --test grpc_estate print_grpc_estate_tally -- --ignored --nocapture
```

## The parser (de-risk verdict)

`.proto` source is parsed by **`protox-parse` 0.9.0**, a pure, IO-free,
single-file parser: `parse(name, source) -> Result<FileDescriptorProto, ParseError>`
(text in, descriptor out, no filesystem), with `line:col` syntax diagnostics. It
parses **proto2 and proto3**, reads `package`, every `service` and its `rpc`s
(name, input/output message, and the `client_streaming`/`server_streaming` flags),
and tolerates `import` statements (it does not *resolve* them; the parse is
syntactic on the single file, which is all the contract plane needs).

The rejected alternative was `protobuf-parse` (rust-protobuf): its only public API
is filesystem-based (include paths + input files), which would force tempfile IO
inside `extract` and break the `ContractAdapter` pure-no-IO contract every other
adapter honours.

**Bounds (honest):** cross-file `import`s are not resolved (we only read this
file's package/service/rpc, which are present without resolution); request/response
message names are read as the source token, not a fully-qualified resolved type
(no symbol resolution, and none is needed for the operation key).

## What an operation is (honesty / provenance, R1)

An `rpc` declared in a parsed `.proto` is a **fact**, so its node is `Extracted`
(confidence 1.0): nothing is inferred at M1. A `.proto` that will not parse is a
**surfaced diagnostic** (`ContractError::Parse` with the parser's `line:col`
message), never a silent drop and never a partial-garbage operation. A `.proto`
that declares only `message`s/`enum`s (no `service`) is detected as proto but
yields **zero** operations, honest, because it declares no rpcs.

`message`s and `enum`s are **types, not operations**, and contribute no nodes.

## Canonical identity (the api-scoped key, B6 fix)

A gRPC operation is identified estate-wide by **`(api_id, format, key)`**, encoded
in its UID as `contract | <estate> | <api_id>/grpc | <key> |`.

- **`key`** is the rpc's fully-qualified gRPC identity:
  `"<package>.<Service>.<Method>"` when the `.proto` declares a `package` (e.g.
  `acme.users.v1.UserService.GetUser`), else the bare `"<Service>.<Method>"`. The
  package is part of the **key** (the chosen one of the two documented options,
  "package as part of key" vs "part of api_id"): it is the wire-correct identity
  (a gRPC call addresses `/<package>.<Service>/<Method>`), so two repos that host
  the *same* proto produce the *same* key and dedup correctly, while two
  different-package services that share a `Service.Method` name stay distinct.
- **`format`** is `grpc`. The format part keeps a gRPC `Foo.Get`, an OpenAPI op,
  and a GraphQL `Foo.Get` that share a key STRING on **distinct** canonical nodes.
- **`api_id`** is the manifest-declared `[[repos.apis]]` id when a declared `spec`
  owns the operation, **else the repo name** (the safe default). Two repos never
  merge a shared key unless they positively declare the same api id.

The node kind stays **`ApiOperation`**: a gRPC rpc *is* an api operation, so no
new `NodeKind` is warranted; the `grpc` vs `openapi` distinction is carried by the
UID's format discriminator, not the node kind.

The streaming shape is recorded cheaply on the operation's `method` (no schema
change): `GRPC` (unary), `GRPC_SERVER_STREAM`, `GRPC_CLIENT_STREAM`,
`GRPC_BIDI_STREAM`.

## Corpus

One committed fixture estate under
`crates/strata-index/tests/fixtures/crossrepo_grpc/`:

- **`repo-a`** and **`repo-b`**: each carries a byte-identical `service.proto`
  (`package shop.orders.v1; service OrderService { rpc GetOrder … }`) and both
  positively declare the **same** api id `orders` in `strata.workspace.toml`. The
  opt-in merge: their shared `shop.orders.v1.OrderService.GetOrder` rpc collapses
  to **one** canonical `ApiOperation` node.
- **`repo-collide`**: carries BOTH a package-less gRPC `collide.proto`
  (`service Query { rpc getOrder … }` → key `Query.getOrder`) AND a GraphQL
  `schema.graphql` (`type Query { getOrder … }` → key `Query.getOrder`). They
  share the key STRING but differ in `format`, so they stay **two** distinct
  canonical nodes (one `ApiOperation`, one `GraphqlField`): the format-
  discriminator proof.

`node_modules`/`.strata` are not committed; the estate is linked hermetically with
`ResolveMode::Off` (no Node/SCIP).

## Results

Measured 2026-06-13 over the committed `crossrepo_grpc` estate.

| metric | value |
|---|---:|
| gRPC `ApiOperation` nodes | **2** |
| GraphQL `GraphqlField` nodes (the format twin) | **1** |
| gRPC contract edges (`PRODUCES`/`CONSUMES`) | **0** (code linking is M2) |

Reading the numbers:

- **2 gRPC operation nodes:** `shop.orders.v1.OrderService.GetOrder` (repo-a and
  repo-b deduped to ONE canonical node under the declared api id `orders`, the B6
  opt-in merge) and `Query.getOrder` (repo-collide's package-less gRPC service).
- **1 GraphQL field node:** repo-collide's `Query.getOrder`. It shares the key
  string with the gRPC `Query.getOrder` but the `grpc` vs `graphql` discriminator
  keeps them on **distinct** nodes, never merged. This is the cross-format safety
  the B6 identity guarantees.
- **0 gRPC contract edges:** M1 lands substrate (operation nodes + identity) only;
  producer/consumer code linking is M2.

## What's next (M2)

Producer/consumer **code** linking, honestly banded: a gRPC service `impl` → the
rpc it serves (`PRODUCES`, Inferred, a convention match, like a REST route), and
a generated-stub call → the rpc it invokes (`CONSUMES`). At that point this report
gains a link-coverage table and band-invariant tests over gRPC contract edges, the
same shape as `graphql-linking.md` today.
