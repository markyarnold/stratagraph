use serde::{Deserialize, Serialize};

use crate::model::{NodeKind, Span};

/// serde default for [`GqlDocument::tagged`]: an entry written before the field
/// existed was always a `gql`/`graphql`-tagged template, so a missing key means
/// `tagged: true`.
fn default_true() -> bool {
    true
}

/// Bump this whenever AnalyzedFile/RawSymbol/ImportRef/CallRef/RouteDecl change
/// shape. Used to invalidate stale parse caches across upgrades.
///
/// v3: added `AnalyzedFile::routes` (`RouteDecl`) for contract-plane producer
/// linking (Slice 3, M2).
/// v4: added `AnalyzedFile::http_calls` (`HttpCall`) for contract-plane consumer
/// linking (Slice 3, M3).
/// v5: added `AnalyzedFile::gql_documents` (`GqlDocument`) and
/// `AnalyzedFile::resolver_entries` (`ResolverEntry`) for the GraphQL contract
/// plane (Slice 4, M2).
/// v6: added `GqlDocument::tagged` (untagged template-constant documents — the
/// dominant AppSync/Amplify style). Old cache entries default `tagged: true`
/// (they were all `gql`/`graphql`-tagged), so the default keeps them correct;
/// the bump enforces the "schema change ⇒ bump" invariant regardless.
///
/// **No bump for the Python plane (Slice 9).** Adding `strata-lang-py` does NOT
/// change `AnalyzedFile`/`RawSymbol`/`ImportRef`/`CallRef`/`RouteDecl` (or any
/// shape this version guards) — the Python adapter merely *fills* the existing
/// code-plane fields for a new file kind (`.py`). A `.py` file is a brand-new
/// cache entry under its own path key; it never reinterprets an existing TS/JS
/// entry, and an existing entry's bytes are unchanged. The "schema change ⇒ bump"
/// invariant is about the serialized *shape*, which is identical, so this stays
/// at 6.
///
/// v7: added `AnalyzedFile::sql_candidates` (`SqlCandidate`) for the data-plane
/// code→table linking (Slice 16, D3, M2). Each language analyzer's tree-sitter
/// walk now collects the **SQL-looking** string literals it sees (a cheap
/// case-insensitive `SELECT|INSERT INTO|UPDATE|DELETE FROM|… FROM` keyword
/// prefilter — NOT every string, which would bloat the cache for no benefit), so
/// the data plane can parse them into `Reads`/`Writes` edges. This IS a serialized
/// *shape* change (a new field), so it bumps the version: every parse cache is
/// re-derived once on the next index. The re-parse is the honest cost of caching
/// SQL strings precisely rather than re-scanning the source on every build (which
/// would break the `incremental == full` invariant). `#[serde(default)]` on the
/// new field means an old cache entry deserializes to an empty vec, but the bump
/// enforces the "schema change ⇒ bump" invariant regardless: the graph RESULTS are
/// identical after the re-parse (the field only *adds* data-plane edges, never
/// changes a code/contract/infra node or edge), so `incremental == full` still
/// holds — re-indexing the same tree yields a byte-identical graph.
///
/// v8: added `CallRef::receiver_is_path` (slice 23) — a discriminator separating a
/// `::`-scoped path qualifier (`Type::method`, a type/module) from a `.` field
/// receiver (`obj.method`, a value). The Rust linker uses it to resolve
/// type-qualified calls to the exact type's method instead of fanning out
/// ambiguously. This IS a serialized *shape* change (a new field), so it bumps the
/// version: every parse cache re-derives once on the next index and the flag is
/// populated correctly. `#[serde(default)]` (= `false`) keeps a pre-v8 cache entry
/// deserializable in transition — a defaulted `false` reads a scoped call as a
/// field receiver, which is merely the old, less-precise behavior, never wrong —
/// but the bump means no stale entry is actually served: the re-parse fills the
/// flag, and `incremental == full` still holds (re-indexing the same tree yields a
/// byte-identical graph; the field only *sharpens* existing Rust call edges, never
/// adds/removes a node).
///
/// v9: added `AnalyzedFile::orm_models` (`OrmModelHint`) for the data-plane ORM
/// model→table linking (Slice 25, D3, M2b). Each language analyzer's walk now
/// captures an ORM model class that declares an **explicit** table name — a Python
/// SQLAlchemy `__tablename__ = "…"`, a Django `class Meta: db_table = "…"`, a TS
/// TypeORM `@Entity("…")` — so the data plane can add a `MapsTo` edge from the
/// model class node to its `Table` node (mirroring the M2 raw-SQL `Reads`/`Writes`
/// path). Only an explicit literal table name yields a hint; a convention-derived
/// name (no literal) or a dynamic/interpolated argument yields none (never invented,
/// R1/R5). This IS a serialized *shape* change (a new field), so it bumps the
/// version: every parse cache re-derives once on the next index and the field is
/// populated. `#[serde(default)]` (= empty vec) keeps a pre-v9 cache entry
/// deserializable in transition, but the bump means no stale entry is served; the
/// field only *adds* data-plane `MapsTo` edges, never changes a code/contract/infra
/// node or edge, so `incremental == full` still holds (re-indexing the same tree
/// yields a byte-identical graph).
///
/// **v10** extends contract-plane extraction (`routes`/`http_calls`/
/// `gql_documents`/`resolver_entries`) to **Python** (previously TypeScript-only):
/// Flask/FastAPI/Django routes, `requests`/`httpx` calls, `gql` documents, and
/// Strawberry/Graphene/Ariadne resolvers. The serialized *shape* is unchanged (the
/// fields already exist), but Python now fills fields it previously left empty, so a
/// stale pre-v10 Python parse-cache entry must re-derive — hence the bump. It only
/// *adds* contract producer/consumer edges, never changing a code/infra node, so
/// `incremental == full` still holds.
pub const ANALYZER_SCHEMA_VERSION: u32 = 10;

/// A symbol a language adapter found in one file, before cross-file resolution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawSymbol {
    pub kind: NodeKind,
    pub name: String,
    pub fqn: String,
    pub container_fqn: Option<String>,
    pub span: Span,
}

/// An unresolved import statement (resolved to a target later by the indexer).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportRef {
    pub specifier: String,
    pub imported_names: Vec<String>,
    pub span: Span,
    /// Span of each locally-bound name's identifier, parallel to
    /// `imported_names`. Used by precise (SCIP) resolution to target the exact
    /// occurrence of an imported name. Defaults to empty for backward
    /// compatibility with the heuristic path, which never reads it.
    #[serde(default)]
    pub name_spans: Vec<Span>,
}

/// An unresolved call site (resolved to a target later by the indexer).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallRef {
    pub callee_name: String,
    pub receiver: Option<String>,
    pub enclosing_fqn: String,
    pub span: Span,
    /// Span of the callee *identifier* (the `property` of `obj.method()`, or the
    /// function identifier of a bare `foo()` call) — distinct from `span`, which
    /// covers the whole call expression. Precise (SCIP) resolution targets this
    /// identifier position; the heuristic path ignores it. Defaults to `span`'s
    /// default for backward compatibility.
    #[serde(default)]
    pub callee_span: Span,
    /// Whether [`receiver`](CallRef::receiver) is a **`::`-scoped path qualifier**
    /// (`true`) — a type/module, as in `Type::method()`, `mod::func()`, `Self::m()` —
    /// versus a **`.` field receiver** (`false`) — a value, as in `obj.method()` /
    /// `self.m()`. Only meaningful when `receiver` is `Some`; a bare call
    /// (`receiver: None`) leaves it `false`.
    ///
    /// The discriminator a single-segment `receiver` string alone cannot carry: both
    /// `IndexStamp::read()` and `obj.read()` yield `receiver = Some("IndexStamp")` /
    /// `Some("obj")`, indistinguishable without this flag. The Rust linker reads it
    /// to resolve a type-qualified call to the exact method on the named type
    /// (precise) instead of fanning out to every same-named method (ambiguous).
    ///
    /// Each language analyzer that can tell the two apart sets it: the Rust analyzer
    /// distinguishes `scoped_identifier` (`true`) from `field_expression` (`false`).
    /// The TS/Python/C# analyzers set `false` for every member call — their `.`
    /// syntax is overloaded (a `Type.method()` static call and an `obj.method()`
    /// instance call are *syntactically identical*), so separating them needs
    /// receiver-type inference, deferred (see the per-analyzer notes).
    /// `#[serde(default)]` (= `false`) so a pre-v8 cache entry deserializes; the
    /// schema-version bump invalidates stale caches so the flag is populated on the
    /// next index regardless.
    #[serde(default)]
    pub receiver_is_path: bool,
}

/// A route declaration found in a web framework's routing call, e.g. the
/// Express/router shape `app.get("/users/:id", handler)`. Used by the
/// contract plane to link producer code to an `ApiOperation` (`PRODUCES`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteDecl {
    /// HTTP method, upper-cased from the call name (`app.get` → `GET`).
    pub method: String,
    /// The literal path string as written (`"/users/:id"`); not yet normalized.
    pub path: String,
    /// The handler's name when the handler argument is a plain identifier
    /// (`getUser`); `None` for an inline function/arrow handler.
    pub handler_name: Option<String>,
    /// The function/module fqn the route call is declared in (empty at module
    /// top level), mirroring `CallRef::enclosing_fqn`.
    pub enclosing_fqn: String,
    /// Span of the whole route call expression.
    pub span: Span,
}

/// The sentinel [`RouteDecl::method`] value for a route declared **without an HTTP
/// method** — e.g. a Django `path()`/`re_path()` URL that maps a path to a view
/// which dispatches methods internally. The producer linker treats this specially:
/// it matches on the **normalized path alone** (any method at that path), banding a
/// unique path match at a *lower* `Inferred` tier than a method+path match and
/// several as `Ambiguous` — it never claims a specific method the view may not
/// implement (R5). The angle-bracketed form is deliberately **not a representable
/// HTTP method** (extracted methods are upper-cased alphabetic verbs, so a route
/// decorator can never produce this value), which guarantees an exact
/// `op.method == route.method` comparison never matches it by accident.
pub const ROUTE_METHOD_ANY: &str = "<ANY>";

/// The shape of a URL argument in an outgoing HTTP call, as far as static
/// extraction can tell. Drives the consumer-link tier (Slice 3, M3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UrlShape {
    /// A plain string literal, e.g. `fetch("/users/123")` → `"/users/123"`.
    Literal(String),
    /// A template string whose interpolations are canonicalised to `{}`, e.g.
    /// `` fetch(`/users/${id}`) `` → `"/users/{}"`. Matches a normalized
    /// operation path template.
    Template(String),
    /// A non-literal URL: a bare identifier, a string concatenation, or any
    /// computed expression (`fetch(url)`, `fetch("/users/" + id)`). The path is
    /// opaque, so the link can only ever be `Ambiguous` (R5).
    Dynamic,
}

/// An outgoing HTTP call found in consumer code, e.g. `fetch(...)` or
/// `axios.get(...)`. Used by the contract plane to link consumer code to an
/// `ApiOperation` (`CONSUMES`). Additive to [`AnalyzedFile`] (Slice 3, M3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpCall {
    /// HTTP method, upper-cased (`fetch` defaults to `GET`; `axios.post` →
    /// `POST`). `None` when a `fetch(url, opts)` options object is present but
    /// its `method` is not a plain string literal (method unknown, not assumed).
    pub method: Option<String>,
    /// The URL argument's extracted shape.
    pub url: UrlShape,
    /// The function/module fqn the call is made in (empty at module top level),
    /// mirroring [`CallRef::enclosing_fqn`] — the consumer node a `CONSUMES`
    /// edge originates from.
    pub enclosing_fqn: String,
    /// Span of the whole HTTP call expression.
    pub span: Span,
}

/// A GraphQL document found in client code, used by the contract plane to link
/// consumer code to a `GraphqlField` (`CONSUMES`). Additive to [`AnalyzedFile`]
/// (Slice 4, M2). Two provenances, distinguished by [`tagged`]:
///
/// - **Tagged** (`tagged: true`): a `gql`/`graphql` tagged template
///   (`` gql`query { getUser }` ``) or a `.graphql`/`.gql` operation file. The
///   author explicitly declared it GraphQL, so a parse failure is an *honest
///   miss* — counted in `unparsed_documents` (coverage).
/// - **Untagged** (`tagged: false`): a substitution-free template-literal
///   constant (`` const Q = `query GetX { getX }` ``) — the dominant
///   AppSync/Amplify style, captured only after a cheap content prefilter. It is
///   a *candidate*: at link time `parse_operations` Ok → linked exactly like a
///   tagged doc; Err → **silently skipped** (NOT counted in `unparsed_documents`
///   — it never claimed to be GraphQL).
///
/// The captured `text` is reliable **only** when [`interpolation_free`] is true:
/// an interpolated tagged template (`` gql`query { ${frag} }` ``) is recorded with
/// `interpolation_free: false` and is **counted but never parsed/linked** — a
/// fragment's expansion is opaque, so guessing a field would be confident-wrong
/// (design R1/R5). An untagged template *with* substitutions is not emitted at
/// all.
///
/// [`tagged`]: GqlDocument::tagged
/// [`interpolation_free`]: GqlDocument::interpolation_free
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GqlDocument {
    /// The raw template text (the literal quasis joined). Reliable only when
    /// `interpolation_free` is true; otherwise it is partial/empty and unused.
    pub text: String,
    /// Whether the template had **no** `${…}` interpolations. `false` ⇒ `text` is
    /// unreliable; the document is counted as unparsed and never linked.
    pub interpolation_free: bool,
    /// Whether the document came from an explicit GraphQL surface — a
    /// `gql`/`graphql` tag or a `.graphql`/`.gql` file (`true`), versus an
    /// *untagged* template-constant candidate (`false`). Governs the
    /// parse-failure accounting: a tagged miss is counted in `unparsed_documents`;
    /// an untagged parse failure is silently skipped (it never claimed to be
    /// GraphQL). `#[serde(default = "default_true")]` so a pre-v6 cache entry (all
    /// tagged) deserializes correctly; the schema-version bump also invalidates
    /// stale caches.
    #[serde(default = "default_true")]
    pub tagged: bool,
    /// The function/module fqn the document is declared in (empty at module top
    /// level), mirroring [`CallRef::enclosing_fqn`] — the consumer node a
    /// `CONSUMES` edge originates from.
    pub enclosing_fqn: String,
    /// Span of the whole tagged-template expression.
    pub span: Span,
}

/// One Apollo-style resolver-map entry, e.g. the `getUser` in
/// `{ Query: { getUser } }`. Used by the contract plane to link resolver code to
/// a `GraphqlField` (`PRODUCES`). Additive to [`AnalyzedFile`] (Slice 4, M2).
///
/// Extraction is **conservative**: only object properties whose outer key is
/// *literally* `Query`/`Mutation`/`Subscription` and whose value is an object
/// literal yield entries, and only for inner properties whose value is a
/// function/arrow/identifier (a non-function value like `{ Query: { timeout: 30 }
/// }` yields nothing). A missed resolver is a missed link (surfaced via
/// coverage); a false match is designed out (design R1/R5; spec §3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolverEntry {
    /// The literal outer key: `"Query"` | `"Mutation"` | `"Subscription"`.
    pub op_type: String,
    /// The inner property key (the field name, e.g. `"getUser"`).
    pub field: String,
    /// The handler's name when the inner value is an identifier (`getUser`) or a
    /// named function; `None` for an inline arrow/function expression.
    pub handler_name: Option<String>,
    /// The function/module fqn the resolver map is declared in (empty at module
    /// top level), mirroring [`RouteDecl::enclosing_fqn`].
    pub enclosing_fqn: String,
    /// Span of the inner resolver entry (the field property).
    pub span: Span,
}

/// A string literal in code that **looks like raw SQL**, captured by a language
/// analyzer for the data plane's code→table linking (Slice 16, D3, M2).
///
/// Only string literals matching a cheap case-insensitive SQL-keyword prefilter
/// (`SELECT`/`INSERT INTO`/`UPDATE`/`DELETE FROM`/`… FROM `) are recorded — a
/// non-SQL string is never captured, so a codebase with no SQL adds nothing to the
/// cache (the lean-cache rule, the same spirit as the GraphQL content prefilter).
/// The captured [`text`](SqlCandidate::text) is the literal's **inner** text (the
/// quotes/backticks stripped), so the data-plane linker can hand it straight to
/// `sqlparser`; whether it actually parses to table references is the linker's job,
/// not the analyzer's (a partial fragment that passes the keyword filter but won't
/// parse is simply not linked — honestly absent, never invented).
///
/// **What is NOT captured (honest bound):** a runtime-concatenated or interpolated
/// query (`"SELECT … FROM " + table`, a `${…}` template, an f-string, a C#
/// `$"…"`) is **not a single string literal** — each grammar surfaces the
/// concatenation/interpolation as a distinct node, so only the constant *fragment*
/// (if any) is seen, which will not parse to a complete statement and is therefore
/// not linked. Dynamic SQL is honestly unlinked, not guessed (design R1/R5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SqlCandidate {
    /// The string literal's **inner** text (quotes/backticks/prefix stripped) — a
    /// candidate SQL statement to hand to `sqlparser`. Matched the SQL-keyword
    /// prefilter; whether it parses is decided downstream.
    pub text: String,
    /// The function/method fqn the literal sits in (empty at module top level),
    /// mirroring [`CallRef::enclosing_fqn`] — the code node a `Reads`/`Writes` edge
    /// originates from (the data plane falls back to the file `Module` node when
    /// this is empty).
    pub enclosing_fqn: String,
    /// Span of the string-literal expression.
    pub span: Span,
}

/// The ORM framework an [`OrmModelHint`] came from — which model-class convention
/// the analyzer recognised. Carried so the data plane (and reports) can attribute a
/// `MapsTo` edge to its source dialect; it does NOT change the linking rule (every
/// framework links the same way: explicit literal table name → declared `Table`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrmFramework {
    /// Python SQLAlchemy: a class-body `__tablename__ = "<table>"` assignment.
    SqlAlchemy,
    /// Python Django: a nested `class Meta:` whose body has `db_table = "<table>"`.
    Django,
    /// TypeScript TypeORM: a class decorator `@Entity("<table>")` (first string arg).
    TypeOrm,
    /// TypeScript Drizzle: `export const x = pgTable("<table>", …)`. Reserved for a
    /// future slice — NOT emitted today (the analyzer emits no node for the `const`
    /// to link a model from; see `docs/accuracy/data-linking.md`). Listed so the enum
    /// and its serde round-trip are stable when Drizzle support lands.
    Drizzle,
}

/// An ORM **model class** that declares an **explicit** database table name,
/// captured by a language analyzer for the data plane's model→table linking
/// (Slice 25, D3, M2b). The data-plane analogue of [`SqlCandidate`]: the analyzer
/// records the signal; whether the named table is actually declared is the linker's
/// job (a hint naming a table no parsed schema declares yields NO edge, counted
/// `orm_models_unresolved`, never invented — the same never-invent rule as
/// `Reads`/`Writes`).
///
/// **Only explicit literal names this slice (no convention).** A model with no
/// explicit table name (e.g. a SQLAlchemy class relying on the implicit
/// class-name→table convention, or a bare `@Entity()` decorator) yields **no**
/// hint — convention-derived inference (an `Inferred` tier) is deferred (Phase 2).
/// A dynamic/interpolated table argument (`__tablename__ = PREFIX + "users"`,
/// `@Entity(NAME)`) is likewise **not** a single string literal → no hint (R1/R5):
/// the analyzer never guesses a table name from a non-literal.
///
/// [`table_name`](OrmModelHint::table_name) is the literal's **inner** text
/// (quotes stripped), unquoted, ready to hand to the data plane's `resolve_table`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrmModelHint {
    /// The model class's fully-qualified name (the `fqn` of its [`RawSymbol`]), e.g.
    /// `"User"` — the code node a `MapsTo` edge originates from. The data plane
    /// reconstructs that node's UID as `Uid::new(lang, repo, path, model_fqn, "")`;
    /// if no node exists for it, NO edge is added (never attached to the module for a
    /// class hint).
    pub model_fqn: String,
    /// The declared table name, the string literal's **inner** text (unquoted), e.g.
    /// `"users"`. Always an explicit literal — never convention-derived, never from a
    /// dynamic expression.
    pub table_name: String,
    /// Which ORM convention this hint came from (for attribution; does not affect the
    /// linking rule).
    pub framework: OrmFramework,
    /// Span of the model class declaration the hint was derived from.
    pub span: Span,
}

/// The cheap, case-insensitive prefilter every language analyzer uses to decide
/// whether a string literal is worth capturing as a [`SqlCandidate`]: it must
/// contain a leading DML keyword that begins a statement the data plane links —
/// `SELECT … FROM`, `INSERT INTO`, `UPDATE … `, or `DELETE FROM`. The check is a
/// substring scan over the lower-cased text, deliberately loose (the real arbiter
/// is `sqlparser` at link time), but tight enough that ordinary prose/identifiers
/// don't match: it requires both a verb AND its companion keyword (`from`/`into`/
/// `set`) so a bare word like "update" or "from" in a log message is rejected.
///
/// Keeping this in core (one definition, shared by ts/py/cs via re-export) means
/// every plane applies the identical filter — a SQL string captured in TS is
/// captured in Python and C# too, and the cache-leanness guarantee (a non-SQL
/// codebase adds nothing) holds uniformly.
pub fn looks_like_sql(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    // A SELECT/DELETE both pair with FROM; INSERT pairs with INTO; UPDATE pairs
    // with SET (UPDATE … SET, the only place a bare UPDATE leads a statement). Each
    // pairing requires BOTH tokens present so a stray verb in prose is not a match.
    (lower.contains("select") && lower.contains("from"))
        || lower.contains("insert into")
        || (lower.contains("update") && lower.contains("set"))
        || (lower.contains("delete") && lower.contains("from"))
}

/// Everything one analyzer produces for a single source file.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AnalyzedFile {
    pub symbols: Vec<RawSymbol>,
    pub imports: Vec<ImportRef>,
    pub calls: Vec<CallRef>,
    /// Web-framework route declarations (Slice 3, M2). `#[serde(default)]` so a
    /// pre-v3 cache entry (no `routes` key) deserializes to an empty vec; the
    /// schema-version bump above also invalidates stale caches.
    #[serde(default)]
    pub routes: Vec<RouteDecl>,
    /// Outgoing HTTP calls (Slice 3, M3). `#[serde(default)]` so a pre-v4 cache
    /// entry (no `http_calls` key) deserializes to an empty vec; the
    /// schema-version bump above also invalidates stale caches.
    #[serde(default)]
    pub http_calls: Vec<HttpCall>,
    /// `gql`/`graphql` tagged-template documents (Slice 4, M2). `#[serde(default)]`
    /// so a pre-v5 cache entry (no `gql_documents` key) deserializes to an empty
    /// vec; the schema-version bump above also invalidates stale caches.
    #[serde(default)]
    pub gql_documents: Vec<GqlDocument>,
    /// Apollo-style resolver-map entries (Slice 4, M2). `#[serde(default)]` so a
    /// pre-v5 cache entry (no `resolver_entries` key) deserializes to an empty
    /// vec; the schema-version bump above also invalidates stale caches.
    #[serde(default)]
    pub resolver_entries: Vec<ResolverEntry>,
    /// SQL-looking string literals (Slice 16, D3, M2) — the raw SQL the data plane
    /// parses into `Reads`/`Writes` edges. `#[serde(default)]` so a pre-v7 cache
    /// entry (no `sql_candidates` key) deserializes to an empty vec; the
    /// schema-version bump above also invalidates stale caches.
    #[serde(default)]
    pub sql_candidates: Vec<SqlCandidate>,
    /// ORM model classes with an explicit table name (Slice 25, D3, M2b) — the
    /// signal the data plane turns into `MapsTo` model→table edges. `#[serde(default)]`
    /// so a pre-v9 cache entry (no `orm_models` key) deserializes to an empty vec; the
    /// schema-version bump above also invalidates stale caches.
    #[serde(default)]
    pub orm_models: Vec<OrmModelHint>,
}

/// The adapter interface every language implementation satisfies.
/// Lives in core so the contract is stable; implementations (e.g. strata-lang-ts)
/// depend on core, never the other way round.
pub trait LanguageAnalyzer {
    fn extensions(&self) -> &'static [&'static str];
    fn analyze(&self, path: &str, source: &str) -> AnalyzedFile;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubAnalyzer;

    impl LanguageAnalyzer for StubAnalyzer {
        fn extensions(&self) -> &'static [&'static str] {
            &["ts", "tsx"]
        }
        fn analyze(&self, _path: &str, _source: &str) -> AnalyzedFile {
            AnalyzedFile {
                symbols: vec![RawSymbol {
                    kind: NodeKind::Function,
                    name: "foo".into(),
                    fqn: "foo".into(),
                    container_fqn: None,
                    span: Span::default(),
                }],
                ..AnalyzedFile::default()
            }
        }
    }

    #[test]
    fn analyzer_trait_is_object_safe_and_usable() {
        let analyzer: Box<dyn LanguageAnalyzer> = Box::new(StubAnalyzer);
        assert!(analyzer.extensions().contains(&"ts"));
        let result = analyzer.analyze("src/a.ts", "function foo() {}");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "foo");
    }

    #[test]
    fn analyzed_file_serde_round_trip() {
        let original = AnalyzedFile {
            symbols: vec![
                RawSymbol {
                    kind: NodeKind::Function,
                    name: "foo".into(),
                    fqn: "foo".into(),
                    container_fqn: None,
                    span: Span {
                        start_line: 1,
                        start_col: 0,
                        end_line: 3,
                        end_col: 1,
                    },
                },
                RawSymbol {
                    kind: NodeKind::Method,
                    name: "bar".into(),
                    fqn: "A.bar".into(),
                    container_fqn: Some("A".into()),
                    span: Span {
                        start_line: 10,
                        start_col: 2,
                        end_line: 12,
                        end_col: 3,
                    },
                },
            ],
            imports: vec![ImportRef {
                specifier: "./utils".into(),
                imported_names: vec!["helper".into(), "format".into()],
                span: Span {
                    start_line: 1,
                    start_col: 0,
                    end_line: 1,
                    end_col: 40,
                },
                name_spans: vec![
                    Span {
                        start_line: 1,
                        start_col: 9,
                        end_line: 1,
                        end_col: 15,
                    },
                    Span {
                        start_line: 1,
                        start_col: 17,
                        end_line: 1,
                        end_col: 23,
                    },
                ],
            }],
            calls: vec![CallRef {
                callee_name: "helper".into(),
                receiver: None,
                enclosing_fqn: "foo".into(),
                span: Span {
                    start_line: 2,
                    start_col: 4,
                    end_line: 2,
                    end_col: 14,
                },
                callee_span: Span {
                    start_line: 2,
                    start_col: 4,
                    end_line: 2,
                    end_col: 10,
                },
                receiver_is_path: false,
            }],
            routes: vec![RouteDecl {
                method: "GET".into(),
                path: "/users/:id".into(),
                handler_name: Some("getUser".into()),
                enclosing_fqn: String::new(),
                span: Span {
                    start_line: 5,
                    start_col: 0,
                    end_line: 5,
                    end_col: 35,
                },
            }],
            http_calls: vec![HttpCall {
                method: Some("GET".into()),
                url: UrlShape::Template("/users/{}".into()),
                enclosing_fqn: "foo".into(),
                span: Span {
                    start_line: 7,
                    start_col: 4,
                    end_line: 7,
                    end_col: 30,
                },
            }],
            gql_documents: vec![GqlDocument {
                text: "query { getUser }".into(),
                interpolation_free: true,
                tagged: true,
                enclosing_fqn: "foo".into(),
                span: Span {
                    start_line: 8,
                    start_col: 4,
                    end_line: 8,
                    end_col: 24,
                },
            }],
            resolver_entries: vec![ResolverEntry {
                op_type: "Query".into(),
                field: "getUser".into(),
                handler_name: Some("getUser".into()),
                enclosing_fqn: String::new(),
                span: Span {
                    start_line: 9,
                    start_col: 2,
                    end_line: 9,
                    end_col: 9,
                },
            }],
            sql_candidates: vec![SqlCandidate {
                text: "SELECT email FROM users".into(),
                enclosing_fqn: "foo".into(),
                span: Span {
                    start_line: 11,
                    start_col: 4,
                    end_line: 11,
                    end_col: 30,
                },
            }],
            orm_models: vec![OrmModelHint {
                model_fqn: "User".into(),
                table_name: "users".into(),
                framework: OrmFramework::SqlAlchemy,
                span: Span {
                    start_line: 13,
                    start_col: 0,
                    end_line: 15,
                    end_col: 1,
                },
            }],
        };

        let json = serde_json::to_string(&original).expect("serialize AnalyzedFile");
        let recovered: AnalyzedFile =
            serde_json::from_str(&json).expect("deserialize AnalyzedFile");
        assert_eq!(
            original, recovered,
            "AnalyzedFile must round-trip through serde_json unchanged"
        );
    }

    #[test]
    fn analyzed_file_deserializes_pre_v3_cache_without_routes() {
        // A cache entry written before the `routes`/`http_calls`/`gql_documents`/
        // `resolver_entries` fields existed has none of those keys.
        // `#[serde(default)]` must fill empty vecs rather than failing, so an
        // old-but-version-matching entry still loads.
        let legacy = r#"{"symbols":[],"imports":[],"calls":[]}"#;
        let file: AnalyzedFile =
            serde_json::from_str(legacy).expect("pre-v3 AnalyzedFile must still deserialize");
        assert!(
            file.routes.is_empty(),
            "missing routes key defaults to empty"
        );
        assert!(
            file.http_calls.is_empty(),
            "missing http_calls key defaults to empty"
        );
        assert!(
            file.gql_documents.is_empty(),
            "missing gql_documents key defaults to empty"
        );
        assert!(
            file.resolver_entries.is_empty(),
            "missing resolver_entries key defaults to empty"
        );
        assert!(
            file.sql_candidates.is_empty(),
            "missing sql_candidates key defaults to empty"
        );
    }

    #[test]
    fn looks_like_sql_matches_dml_and_rejects_prose() {
        // The four DML shapes the data plane links.
        assert!(looks_like_sql("SELECT email FROM users"));
        assert!(looks_like_sql("select * from t WHERE x = 1"));
        assert!(looks_like_sql("INSERT INTO orders (id) VALUES (1)"));
        assert!(looks_like_sql("UPDATE users SET last_login = now()"));
        assert!(looks_like_sql("DELETE FROM sessions WHERE id = $1"));
        // Case-insensitive + a JOIN form (SELECT … FROM … JOIN).
        assert!(looks_like_sql(
            "Select u.id From users u JOIN orgs o ON o.id = u.org_id"
        ));

        // Prose / identifiers that contain a lone verb must NOT match (needs the
        // companion keyword too).
        assert!(!looks_like_sql("please update the record"));
        assert!(!looks_like_sql("from the start"));
        assert!(!looks_like_sql("a selection of items"));
        assert!(!looks_like_sql("/users/:id"));
        assert!(!looks_like_sql(""));
        // A bare "insert" without "into" is not a statement lead.
        assert!(!looks_like_sql("insert your name here"));
    }

    #[test]
    fn analyzed_file_deserializes_pre_v7_cache_without_sql_candidates() {
        // A cache entry written before the `sql_candidates` field existed has no
        // such key. `#[serde(default)]` must fill an empty vec rather than failing,
        // so a pre-v7 entry still loads (the schema-version bump invalidates it on
        // the next index regardless, but the default keeps deserialization total).
        let legacy = r#"{
            "symbols":[],"imports":[],"calls":[],
            "routes":[],"http_calls":[],"gql_documents":[],"resolver_entries":[]
        }"#;
        let file: AnalyzedFile =
            serde_json::from_str(legacy).expect("pre-v7 AnalyzedFile must still deserialize");
        assert!(
            file.sql_candidates.is_empty(),
            "missing sql_candidates key defaults to empty"
        );
        assert!(
            file.orm_models.is_empty(),
            "missing orm_models key defaults to empty"
        );
    }

    #[test]
    fn analyzed_file_deserializes_pre_v9_cache_without_orm_models() {
        // A cache entry written before the `orm_models` field existed has no such key.
        // `#[serde(default)]` must fill an empty vec rather than failing, so a pre-v9
        // entry still loads (the schema-version bump invalidates it on the next index
        // regardless, but the default keeps deserialization total).
        let legacy = r#"{
            "symbols":[],"imports":[],"calls":[],
            "routes":[],"http_calls":[],"gql_documents":[],"resolver_entries":[],
            "sql_candidates":[]
        }"#;
        let file: AnalyzedFile =
            serde_json::from_str(legacy).expect("pre-v9 AnalyzedFile must still deserialize");
        assert!(
            file.orm_models.is_empty(),
            "missing orm_models key defaults to empty"
        );
    }

    #[test]
    fn call_ref_receiver_is_path_defaults_to_false_for_pre_v8_cache() {
        // A `calls` entry written before the `receiver_is_path` field existed has no
        // such key. `#[serde(default)]` must fill `false` (a `.` field receiver — the
        // old, less-precise reading of any receiver) rather than failing, so a pre-v8
        // entry still deserializes. The schema-version bump invalidates it on the next
        // index regardless, which is where the flag gets populated correctly.
        let legacy = r#"{
            "symbols":[],"imports":[],
            "calls":[{
                "callee_name":"read",
                "receiver":"IndexStamp",
                "enclosing_fqn":"db_signal",
                "span":{"start_line":1,"start_col":0,"end_line":1,"end_col":1}
            }]
        }"#;
        let file: AnalyzedFile =
            serde_json::from_str(legacy).expect("pre-v8 CallRef must still deserialize");
        assert_eq!(file.calls.len(), 1);
        assert!(
            !file.calls[0].receiver_is_path,
            "a CallRef with no `receiver_is_path` key defaults to false (field receiver)"
        );
    }

    #[test]
    fn gql_document_tagged_defaults_to_true_for_pre_v6_cache() {
        // A `gql_documents` entry written before the `tagged` field existed was
        // always a `gql`/`graphql`-tagged template. `#[serde(default = "default_true")]`
        // must fill `tagged: true` so an old-but-version-matching entry keeps its
        // counted-on-parse-failure behavior.
        let legacy = r#"{
            "symbols":[],"imports":[],"calls":[],
            "gql_documents":[{
                "text":"query { getUser }",
                "interpolation_free":true,
                "enclosing_fqn":"",
                "span":{"start_line":1,"start_col":0,"end_line":1,"end_col":1}
            }]
        }"#;
        let file: AnalyzedFile =
            serde_json::from_str(legacy).expect("pre-v6 gql document must still deserialize");
        assert_eq!(file.gql_documents.len(), 1);
        assert!(
            file.gql_documents[0].tagged,
            "a gql document with no `tagged` key defaults to tagged: true"
        );
    }
}
