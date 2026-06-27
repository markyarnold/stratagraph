//! The GraphQL (SDL) adapter — the second [`ContractAdapter`].
//!
//! It parses GraphQL **schema** text (SDL) into root-operation-field
//! [`OperationDef`]s. A schema declares root operation types — `Query`,
//! `Mutation`, `Subscription` by default, or renamed via an explicit
//! `schema { query: MyQ … }` block — and each *field* of a root type is one
//! callable operation. We emit one [`OperationDef`] per root field.
//!
//! **Key normalization (the load-bearing rule).** A consumer document
//! (`query { getUser }`) knows only the operation *kind*, never the schema's
//! internal root-type name. So the cross-repo join key is canonicalised to the
//! kind: `Query.<field>` / `Mutation.<field>` / `Subscription.<field>`, even
//! when `schema { query: RootQ }` renames the root type. (`RootQ.ping` →
//! `Query.ping`.)
//!
//! Pure and version-agnostic: SDL text in, operations out, no IO. Built on
//! `apollo-parser`'s error-tolerant CST. Because that parser recovers from
//! errors and still yields a (partial) tree, [`extract`](GraphqlAdapter::extract)
//! checks `errors()` first and returns [`ContractError::Parse`] on any syntax
//! error rather than extracting from a broken tree (design R2: degrade visibly,
//! never silently emit half a contract).

use apollo_parser::cst::{self, CstNode, Definition};
use apollo_parser::{Parser, SyntaxKind};

use crate::{ContractAdapter, ContractError, ContractFormat, OperationDef};

/// The three GraphQL root operation kinds, with their canonical key prefix and
/// upper-cased method string. The *canonical* name is what a key uses regardless
/// of a `schema {}` rename, because that is all a consumer document conveys.
#[derive(Debug, Clone, Copy)]
enum RootKind {
    Query,
    Mutation,
    Subscription,
}

impl RootKind {
    /// The canonical type name used in the key prefix (`Query`/`Mutation`/
    /// `Subscription`) — never the renamed root-type name.
    fn canonical(self) -> &'static str {
        match self {
            RootKind::Query => "Query",
            RootKind::Mutation => "Mutation",
            RootKind::Subscription => "Subscription",
        }
    }

    /// The upper-cased `method` string for an [`OperationDef`].
    fn method(self) -> &'static str {
        match self {
            RootKind::Query => "QUERY",
            RootKind::Mutation => "MUTATION",
            RootKind::Subscription => "SUBSCRIPTION",
        }
    }

    /// The public [`OpType`] for this kind (document-parsing surface).
    fn into_op_type(self) -> OpType {
        match self {
            RootKind::Query => OpType::Query,
            RootKind::Mutation => OpType::Mutation,
            RootKind::Subscription => OpType::Subscription,
        }
    }
}

/// Adapter for GraphQL SDL schemas (including AWS AppSync schemas).
pub struct GraphqlAdapter;

impl ContractAdapter for GraphqlAdapter {
    /// Detect a GraphQL **schema** (SDL), as opposed to an executable operation
    /// document or arbitrary text.
    ///
    /// The discriminator is content, not filename: the text parses to a CST that
    /// contains at least one *type-system* definition or extension (`type`,
    /// `interface`, `enum`, `input`, `union`, `scalar`, `schema`, `directive`, or
    /// any `extend …`). A document made only of operations/fragments — which is
    /// the consumer side, handled by [`crate::parse_operations`] — is **not** a
    /// schema and returns `false`, even under a `.graphql`/`.gql` name. Arbitrary
    /// prose/JSON yields no type-system definitions and so is also `false`.
    ///
    /// This is intentionally tolerant of *minor* syntax errors (it does not gate
    /// on `errors()`): detection only needs to recognise the shape; the strict
    /// error check lives in [`extract`](GraphqlAdapter::extract).
    fn detects(&self, _filename: &str, content: &str) -> bool {
        // A quick reject: empty/whitespace-only content is never a schema. (The
        // parser would yield an empty document; this just avoids the work.)
        if content.trim().is_empty() {
            return false;
        }
        let cst = Parser::new(content).parse();
        cst.document()
            .definitions()
            .any(|def| is_type_system_definition(&def))
    }

    /// Parse SDL into one [`OperationDef`] per field of each root operation type.
    ///
    /// Any syntax error → [`ContractError::Parse`] (the CST is error-tolerant, so
    /// we must check explicitly). Root operation types are the default
    /// `Query`/`Mutation`/`Subscription`, overridden by an explicit `schema {}`
    /// block; each is matched to its concrete type definition by name, and every
    /// field becomes a canonically-keyed operation.
    fn extract(&self, spec_path: &str, content: &str) -> Result<Vec<OperationDef>, ContractError> {
        let cst = Parser::new(content).parse();

        // Error-tolerant parser: a recovered tree can still be structurally
        // broken. Surface the first error rather than extract half a schema.
        if let Some(err) = cst.errors().next() {
            return Err(ContractError::Parse {
                spec: spec_path.to_string(),
                msg: format!("GraphQL syntax error: {}", err.message()),
            });
        }

        let doc = cst.document();

        // Resolve which concrete type name backs each root kind. Default to the
        // canonical names; an explicit `schema {}` block overrides per kind.
        let mut roots = RootTypeNames::default();
        for def in doc.definitions() {
            if let Definition::SchemaDefinition(schema_def) = def {
                roots.apply_schema_definition(&schema_def);
            }
        }

        // Walk every object type definition; if its name is a resolved root
        // type, emit one operation per field under that root's canonical kind.
        let mut ops = Vec::new();
        for def in doc.definitions() {
            let Definition::ObjectTypeDefinition(obj) = def else {
                continue;
            };
            let Some(type_name) = obj.name().map(|n| n.text().to_string()) else {
                continue;
            };
            let Some(kind) = roots.kind_for(&type_name) else {
                continue; // not a root operation type → contributes no operations
            };
            let Some(fields) = obj.fields_definition() else {
                continue; // a root type with no field block → no operations
            };
            for field in fields.field_definitions() {
                let Some(field_name) = field.name().map(|n| n.text().to_string()) else {
                    continue;
                };
                ops.push(operation_def(kind, &field_name, spec_path));
            }
        }

        Ok(ops)
    }
}

/// Build a canonical [`OperationDef`] for one root field.
fn operation_def(kind: RootKind, field_name: &str, spec_path: &str) -> OperationDef {
    let key = format!("{}.{}", kind.canonical(), field_name);
    OperationDef {
        format: ContractFormat::Graphql,
        key,
        method: kind.method().to_string(),
        path: field_name.to_string(),
        norm_path: field_name.to_string(),
        operation_id: None,
        spec_path: spec_path.to_string(),
    }
}

/// The concrete type name backing each root operation kind. Defaults to the
/// canonical names (`Query`/`Mutation`/`Subscription`); an explicit `schema {}`
/// block overrides any of them (e.g. `query: RootQ`).
struct RootTypeNames {
    query: String,
    mutation: String,
    subscription: String,
}

impl Default for RootTypeNames {
    fn default() -> Self {
        RootTypeNames {
            query: "Query".to_string(),
            mutation: "Mutation".to_string(),
            subscription: "Subscription".to_string(),
        }
    }
}

impl RootTypeNames {
    /// Override the default root type names from an explicit `schema {}` block.
    /// Each `<operation>: <NamedType>` entry repoints one root kind.
    fn apply_schema_definition(&mut self, schema_def: &cst::SchemaDefinition) {
        for rot in schema_def.root_operation_type_definitions() {
            let Some(named) = rot
                .named_type()
                .and_then(|n| n.name())
                .map(|n| n.text().to_string())
            else {
                continue;
            };
            match rot.operation_type().and_then(operation_type_kind) {
                Some(RootKind::Query) => self.query = named,
                Some(RootKind::Mutation) => self.mutation = named,
                Some(RootKind::Subscription) => self.subscription = named,
                None => {}
            }
        }
    }

    /// Which root kind (if any) the concrete type `type_name` backs. A schema
    /// could in principle point two kinds at the same type; we resolve in
    /// Query→Mutation→Subscription order (deterministic; such a schema is
    /// degenerate and not something we need to split).
    fn kind_for(&self, type_name: &str) -> Option<RootKind> {
        if type_name == self.query {
            Some(RootKind::Query)
        } else if type_name == self.mutation {
            Some(RootKind::Mutation)
        } else if type_name == self.subscription {
            Some(RootKind::Subscription)
        } else {
            None
        }
    }
}

/// Map an apollo-parser `OperationType` token to our [`RootKind`].
fn operation_type_kind(ot: cst::OperationType) -> Option<RootKind> {
    if ot.query_token().is_some() {
        Some(RootKind::Query)
    } else if ot.mutation_token().is_some() {
        Some(RootKind::Mutation)
    } else if ot.subscription_token().is_some() {
        Some(RootKind::Subscription)
    } else {
        None
    }
}

// ── Executable-document parsing (the consumer side) ──────────────────────────
//
// A GraphQL *document* in client code is a set of executable operations. Each
// operation's top-level (root) selections name the contract fields it consumes:
// `query { getUser … }` consumes `Query.getUser`. We read those root selections
// only — nested fields (`getUser { name }`) are a deferred, finer granularity
// (mirrors OpenAPI's operation-level boundary). This is the input to milestone
// 2's consumer linker; it is pure (text in, fields out).

/// A GraphQL root operation kind a consumed field belongs to. The public
/// document-parsing counterpart of the internal [`RootKind`]; an anonymous or
/// `query`-keyword operation is [`Query`](OpType::Query).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpType {
    /// A `query` operation (the default when no operation keyword is present).
    Query,
    /// A `mutation` operation.
    Mutation,
    /// A `subscription` operation.
    Subscription,
}

impl OpType {
    /// The canonical type-name prefix used in a join key (`Query`/`Mutation`/
    /// `Subscription`) — identical to the schema side's
    /// [`RootKind::canonical`], so a document field and a schema operation share
    /// the same `"<Type>.<field>"` key.
    pub fn canonical(self) -> &'static str {
        match self {
            OpType::Query => "Query",
            OpType::Mutation => "Mutation",
            OpType::Subscription => "Subscription",
        }
    }
}

/// One root field an operation consumes, e.g. `{ Query, "getUser" }`. Joins to a
/// schema's [`OperationDef`] by the canonical key `"<op_type>.<field>"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsumedField {
    /// Which root operation kind this field belongs to.
    pub op_type: OpType,
    /// The root field name (e.g. `"getUser"`). Introspection meta fields
    /// (`__`-prefixed, like `__typename`) are excluded.
    pub field: String,
}

/// The result of parsing an executable document: the root fields consumed across
/// all its operations, plus a count of root-level selections we could not
/// resolve to a concrete field (fragment spreads / inline fragments at the
/// operation root). Those are **counted, never guessed** — surfacing the gap
/// honestly rather than inventing a field (design R1/R5).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DocConsumption {
    /// The resolved root fields, in document order across operations.
    pub fields: Vec<ConsumedField>,
    /// How many root-level fragment spreads / inline fragments were seen but not
    /// resolved (a fragment's fields are not expanded here; an inline fragment's
    /// type condition is not evaluated). Reported so coverage can show the gap.
    pub unresolved_root_spreads: usize,
}

/// Parse an executable GraphQL document and return the root fields each of its
/// operations consumes.
///
/// Handles named (`query Foo { … }`) and anonymous (`{ … }`) operations,
/// multiple operations in one document, and `mutation`/`subscription`. Only the
/// **root** selection set of each operation is read (one level — nested field
/// selections are out of scope, mirroring the operation-level contract
/// boundary).
///
/// - `__`-prefixed meta fields (e.g. `__typename`) are skipped (introspection,
///   not a contract operation).
/// - A root-level **fragment spread** (`...Frag`) or **inline fragment**
///   (`... on T { … }`) is counted in [`DocConsumption::unresolved_root_spreads`]
///   and never expanded into guessed fields.
/// - Fragment *definitions* in the document are ignored (only operations
///   consume); a fragment's body is not walked.
///
/// A syntax error → [`ContractError::Parse`] (no panic), like the schema path.
pub fn parse_operations(doc_path: &str, text: &str) -> Result<DocConsumption, ContractError> {
    let cst = Parser::new(text).parse();

    if let Some(err) = cst.errors().next() {
        return Err(ContractError::Parse {
            spec: doc_path.to_string(),
            msg: format!("GraphQL syntax error: {}", err.message()),
        });
    }

    let mut out = DocConsumption::default();
    for def in cst.document().definitions() {
        // Only operations consume fields. Fragment definitions (and any stray
        // type-system definition) are not consumers and are skipped.
        let Definition::OperationDefinition(op) = def else {
            continue;
        };
        // The operation kind: an explicit `query`/`mutation`/`subscription`
        // keyword, defaulting to Query for an anonymous `{ … }` operation.
        let op_type = op
            .operation_type()
            .and_then(operation_type_kind)
            .map(RootKind::into_op_type)
            .unwrap_or(OpType::Query);

        let Some(sel_set) = op.selection_set() else {
            continue;
        };
        for sel in sel_set.selections() {
            match sel {
                cst::Selection::Field(field) => {
                    let Some(name) = field.name().map(|n| n.text().to_string()) else {
                        continue;
                    };
                    // Skip introspection meta fields (`__typename`, `__schema`, …):
                    // they are not contract operations.
                    if name.starts_with("__") {
                        continue;
                    }
                    out.fields.push(ConsumedField {
                        op_type,
                        field: name,
                    });
                }
                // A root fragment spread / inline fragment: we do not expand it
                // into fields (no schema here to resolve a spread; an inline
                // fragment's condition is unevaluated). Count it, never guess.
                cst::Selection::FragmentSpread(_) | cst::Selection::InlineFragment(_) => {
                    out.unresolved_root_spreads += 1;
                }
            }
        }
    }

    Ok(out)
}

/// Whether a CST definition is a *type-system* definition or extension (the SDL
/// surface), as opposed to an executable `OperationDefinition`/
/// `FragmentDefinition`. Used by [`GraphqlAdapter::detects`] to tell a schema
/// from an operation document.
fn is_type_system_definition(def: &Definition) -> bool {
    matches!(
        def.syntax().kind(),
        SyntaxKind::SCHEMA_DEFINITION
            | SyntaxKind::SCALAR_TYPE_DEFINITION
            | SyntaxKind::OBJECT_TYPE_DEFINITION
            | SyntaxKind::INTERFACE_TYPE_DEFINITION
            | SyntaxKind::UNION_TYPE_DEFINITION
            | SyntaxKind::ENUM_TYPE_DEFINITION
            | SyntaxKind::INPUT_OBJECT_TYPE_DEFINITION
            | SyntaxKind::DIRECTIVE_DEFINITION
            | SyntaxKind::SCHEMA_EXTENSION
            | SyntaxKind::SCALAR_TYPE_EXTENSION
            | SyntaxKind::OBJECT_TYPE_EXTENSION
            | SyntaxKind::INTERFACE_TYPE_EXTENSION
            | SyntaxKind::UNION_TYPE_EXTENSION
            | SyntaxKind::ENUM_TYPE_EXTENSION
            | SyntaxKind::INPUT_OBJECT_TYPE_EXTENSION
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A default `Subscription` root (no explicit `schema {}` block) extracts as
    /// `Subscription.<field>` — the third default root, not just Query/Mutation.
    #[test]
    fn default_subscription_root_extracts() {
        let sdl = "type Subscription { onPing: String }\n";
        let ops = GraphqlAdapter.extract("s.graphql", sdl).expect("parses");
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].key, "Subscription.onPing");
        assert_eq!(ops[0].method, "SUBSCRIPTION");
    }

    /// A `schema {}` block pointing at a type that does not exist degrades to
    /// zero operations — no panic, no invented operation.
    #[test]
    fn dangling_schema_root_yields_no_ops() {
        let sdl = "schema { query: Missing }\ntype Other { x: String }\n";
        let ops = GraphqlAdapter.extract("s.graphql", sdl).expect("parses");
        assert!(
            ops.is_empty(),
            "a schema root naming a missing type extracts nothing, got {ops:?}"
        );
    }

    /// A standalone type-system extension (`extend type`) is recognised as a
    /// schema by `detects` (it carries a type-system definition), even though its
    /// fields are not extracted without a base type definition.
    #[test]
    fn detects_standalone_extension_as_schema() {
        assert!(GraphqlAdapter.detects("s.graphql", "extend type Query { extra: String }\n"));
    }
}
