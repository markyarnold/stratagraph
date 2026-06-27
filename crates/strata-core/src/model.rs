use serde::{Deserialize, Serialize};

use crate::ids::Uid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NodeKind {
    Repo,
    Package,
    File,
    Module,
    Class,
    Interface,
    Function,
    Method,
    /// An interface-contract operation (e.g. an OpenAPI path+method). Lives in
    /// the contract plane, not the code plane; produced by `strata-index` from a
    /// `strata_contract::OperationDef`. Handled generically by the graph, store,
    /// and traversal (no per-kind match arms exist), so this addition is additive.
    ApiOperation,
    /// A GraphQL root-operation field (`Query.getUser`/`Mutation.createUser`/
    /// `Subscription.onPing`). The GraphQL analogue of [`ApiOperation`]: same
    /// contract plane, produced by `strata-index` from a GraphQL-format
    /// `strata_contract::OperationDef`. Also handled generically (no per-kind
    /// match arms), so it is equally additive (Slice 4, M2).
    GraphqlField,
    /// An infrastructure-plane Lambda function resource (`AWS::Lambda::Function`
    /// / `AWS::Serverless::Function`), produced by `strata-index` from a
    /// `strata_infra::InfraResource` with kind `LambdaFunction`. Lives in the
    /// infra plane; handled generically by the graph/traversal (no per-kind match
    /// arms), so this addition is additive (Slice 5, M2).
    LambdaFn,
    /// An infrastructure-plane IAM role resource (`AWS::IAM::Role`). Infra plane;
    /// handled generically. Additive (Slice 5, M2).
    IamRole,
    /// An infrastructure-plane AppSync GraphQL API resource
    /// (`AWS::AppSync::GraphQLApi`). Carried as inventory; the `ApiId`-wiring
    /// edges are deferred. Infra plane; handled generically. Additive (Slice 5, M2).
    AppSyncApi,
    /// An infrastructure-plane AppSync resolver resource
    /// (`AWS::AppSync::Resolver`). The node a `PRODUCES` edge originates from when
    /// the resolver→datasource→lambda chain cannot be fully resolved to a Lambda.
    /// Infra plane; handled generically. Additive (Slice 5, M2).
    AppSyncResolver,
    /// An infrastructure-plane AppSync data source resource
    /// (`AWS::AppSync::DataSource`). The `Routes` waypoint between a resolver and
    /// its backing Lambda. Infra plane; handled generically. Additive (Slice 5, M2).
    AppSyncDataSource,
    /// Any other infrastructure resource (a queue, table, bucket, …): inventory
    /// only (logical id + CFN type), from a `strata_infra::InfraKind::Generic`.
    /// Infra plane; handled generically. Additive (Slice 5, M2).
    CloudResource,
    /// A data-plane database table, produced by `strata-index` from a
    /// `strata_data::TableDef` (a committed `CREATE TABLE` / cumulative `ALTER`).
    /// Lives in the data plane; handled generically by the graph/traversal (no
    /// per-kind match arms), so this addition is additive (Slice 16, D3).
    Table,
    /// A data-plane table column, produced by `strata-index` from a
    /// `strata_data::ColumnDef`. The target of a `Table —HasColumn→` edge and the
    /// endpoint of a `Column —ForeignKey→ Column` edge. Data plane; handled
    /// generically. Additive (Slice 16, D3).
    Column,
    /// A security-plane **cloud IAM action** — a concrete action `dynamodb:PutItem`,
    /// a wildcard grant pattern `dynamodb:*` / `*`, or an `<opaque:reason>` sentinel
    /// marking a role whose grants could not be enumerated (a managed-policy ARN, an
    /// unbundled SAM policy template, or a `Deny` statement). The shared node at
    /// which `RequiresPermission` (code → action, from detected AWS SDK calls) and
    /// `Grants` (role → action, from IAM policy statements) meet, so IAM
    /// permission-gap reconciliation is a graph traversal (Track D2, design §6.4).
    /// Produced by `strata-index`; handled generically by the graph/traversal (no
    /// per-kind match arms), so this addition is additive.
    CloudAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EdgeKind {
    Defines,
    MemberOf,
    Imports,
    Calls,
    /// Reserved; **not yet emitted by any analyzer** — class-inheritance edges are a
    /// future enhancement. The variant exists so the enum and its store/serde
    /// round-trip are stable, but no extractor produces it today and `impact` never
    /// traverses one (none exist to traverse).
    Extends,
    /// Reserved; **not yet emitted by any analyzer** — interface-implementation
    /// edges are a future enhancement. Defined for enum/serde stability only; no
    /// extractor produces it today.
    Implements,
    /// Producer link: a handler/function node → the `ApiOperation` it implements.
    Produces,
    /// Consumer link: a calling function node → the `ApiOperation` it calls.
    /// Shipped (Slice 8): consumer linking emits these for GraphQL/OpenAPI/gRPC, and
    /// `impact` traverses them by default (the contract hop, `include_contracts`).
    Consumes,
    /// Infra-plane wiring: a `LambdaFn` node → the `IamRole` it assumes (from a
    /// Lambda's `Role` reference). Handled generically by the graph/traversal (no
    /// per-kind match arms); `impact` traverses it as a dependency edge by default
    /// (Slice 10, B1b, `include_infra`).
    Assumes,
    /// Infra-plane bridge into the code plane: a `LambdaFn` node → the code
    /// `Module` node whose file is its resolved handler (`CodeUri` + `Handler`).
    /// Handled generically; `impact` traverses it as a dependency edge by default
    /// (Slice 10, B1b, `include_infra`).
    Runs,
    /// Infra-plane wiring: an `AppSyncResolver` → its `AppSyncDataSource`, and an
    /// `AppSyncDataSource` → the `LambdaFn` it backs. Handled generically; impact
    /// traverses it as a dependency edge (Slice 10, B1b).
    Routes,
    /// Infra-plane containment: an `AppSyncApi` → the resolvers/datasources it
    /// owns (from their `ApiId` reference). A membership/inventory edge, NOT a
    /// dependency: it lights up the `context` `members` bucket so `context(api)`
    /// lists its resolvers, but `impact` deliberately does NOT traverse it
    /// (changing the API container is not changing a resolver). Handled
    /// generically by the graph/traversal (Slice 10, B3).
    Contains,
    /// Data-plane containment: a `Table` node → each of its `Column` nodes (from a
    /// committed `CREATE TABLE` / `ALTER … ADD COLUMN`). It lights up the `context`
    /// `members` bucket so `context(table)` lists its columns. `impact` reverse-
    /// walks INCOMING dependency edges, so a changed `Column` reaches its owning
    /// `Table` ("you changed a column; the table is affected") — but `impact(table)`
    /// does NOT re-list its columns (a container has no incoming `HasColumn`); use
    /// `context(table).members` for that. Handled generically by the
    /// graph/traversal (Slice 16, D3).
    HasColumn,
    /// Data-plane reference: a `Column` node → the `Column` it references via an
    /// explicit `REFERENCES` / table-level `FOREIGN KEY`. A dependency edge:
    /// `impact` traverses it (changing the referenced column reaches the
    /// referencing column), exactly as a code `Calls` or an infra `Routes`.
    /// Handled generically by the graph/traversal (Slice 16, D3).
    ForeignKey,
    /// Data-plane code→table read: a code symbol (the enclosing `Function`/`Method`,
    /// or the file `Module` when none) → a `Table` it reads, parsed from a raw-SQL
    /// `SELECT … FROM t` / `JOIN t` string literal in code (Slice 16, D3, M2). A
    /// dependency edge in the SAME sense as `ForeignKey`/`Calls`: `impact` reverse-
    /// walks it INCOMING from the target, so `impact(table)` reaches the code that
    /// reads it ("change this table → who reads it breaks") — the §6.2 payoff. The
    /// edge is emitted only when the literal's table name matches a declared `Table`
    /// node (Extracted 0.95); an undeclared table yields NO edge (counted
    /// unresolved, never invented). Handled generically by the graph/traversal.
    Reads,
    /// Data-plane code→table write: a code symbol → a `Table` it writes, parsed from
    /// a raw-SQL `INSERT INTO t` / `UPDATE t` / `DELETE FROM t` string literal in
    /// code (Slice 16, D3, M2). The write analogue of [`Reads`](EdgeKind::Reads),
    /// with identical impact semantics (`impact(table)` reaches the writers) and the
    /// same band discipline (declared table → Extracted 0.95; undeclared → no edge,
    /// counted). Handled generically by the graph/traversal.
    Writes,
    /// Data-plane ORM mapping: an ORM **model class** node to the `Table` its rows
    /// live in, from an explicit table-name declaration on the model (a Python
    /// SQLAlchemy `__tablename__ = "…"`, a Django `class Meta: db_table = "…"`, a TS
    /// TypeORM `@Entity("…")`) (Slice 25, D3, M2b). Direction is **model to table**:
    /// the model DEPENDS on the table (a column/table change breaks the model), so it
    /// is a dependency edge in the SAME sense as `Reads`/`Writes`/`ForeignKey` —
    /// `impact` reverse-walks it INCOMING from the target, so `impact(table)` reaches
    /// the mapping model (and transitively the code that uses the model). It is NOT a
    /// `Reads`/`Writes`: an ORM model is a *structural mapping*, not a query, so
    /// keeping it distinct keeps `context(table)`'s read/write buckets honest and
    /// gives the table a dedicated `mapped_by` view. The edge is emitted only when the
    /// model's explicit table name matches a declared `Table` node (Extracted 0.95);
    /// an undeclared table yields NO edge (counted unresolved, never invented).
    /// Handled generically by the graph/traversal.
    MapsTo,
    /// Security-plane **grant**: an `IamRole` (or a SAM `LambdaFunction`, via its
    /// implicit execution role) node → a `CloudAction` it grants, from an IAM policy
    /// statement's `Allow`ed `Action` (a concrete action, a wildcard pattern like
    /// `dynamodb:*`/`*`, or an `<opaque:reason>` sentinel when the grants cannot be
    /// enumerated). The supply side of IAM permission-gap detection
    /// (Track D2, design §6.4), reconciled against `RequiresPermission` by the
    /// `permission_gap` traversal. NOT a code dependency: `impact` does not traverse
    /// it (a grant is reconciliation input, not a blast-radius edge). Handled
    /// generically by the graph/traversal; additive.
    Grants,
    /// Security-plane **requirement**: a code `Function`/`Method` node → the
    /// `CloudAction` it needs, derived from a statically-detected AWS SDK call
    /// (boto3 `client.put_item()` → `dynamodb:PutItem`; AWS SDK v3
    /// `new PutItemCommand()` → `dynamodb:PutItem`). The demand side of IAM
    /// permission-gap detection (Track D2, §6.4). Emitted only for a curated,
    /// verified `(service, operation)` → action mapping; an unmapped call yields NO
    /// edge (an honest unknown, never a guessed action). Handled generically;
    /// `impact` does not traverse it (it is reconciliation input). Additive.
    RequiresPermission,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Provenance {
    Extracted,
    Resolved,
    Observed,
    Inferred,
    Ambiguous,
    Model,
}

/// A confidence value clamped to the inclusive range 0.0..=1.0.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Confidence(f32);

impl Confidence {
    pub fn new(value: f32) -> Confidence {
        Confidence(value.clamp(0.0, 1.0))
    }

    pub fn value(self) -> f32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Span {
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub uid: Uid,
    pub kind: NodeKind,
    pub name: String,
    pub fqn: String,
    pub path: String,
    pub span: Span,
    pub provenance: Provenance,
    pub confidence: Confidence,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub src: Uid,
    pub dst: Uid,
    pub kind: EdgeKind,
    pub provenance: Provenance,
    pub confidence: Confidence,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_clamps_out_of_range() {
        assert_eq!(Confidence::new(1.5).value(), 1.0);
        assert_eq!(Confidence::new(-0.2).value(), 0.0);
        assert_eq!(Confidence::new(0.9).value(), 0.9);
    }

    #[test]
    fn node_round_trips_through_json() {
        let node = Node {
            uid: Uid::new("ts", "app", "src/a.ts", "foo", "()"),
            kind: NodeKind::Function,
            name: "foo".into(),
            fqn: "foo".into(),
            path: "src/a.ts".into(),
            span: Span {
                start_line: 1,
                start_col: 0,
                end_line: 3,
                end_col: 1,
            },
            provenance: Provenance::Extracted,
            confidence: Confidence::new(1.0),
        };
        let json = serde_json::to_string(&node).unwrap();
        let back: Node = serde_json::from_str(&json).unwrap();
        assert_eq!(node, back);
    }
}
