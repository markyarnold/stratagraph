//! Python extraction tests. Each runs `analyze` on an inline source string and
//! asserts on the resulting `AnalyzedFile`. TDD red-first per construct
//! (functions, async/decorated, classes, methods, imports, calls).
//!
//! The discipline mirrors `strata-lang-ts/tests/extraction.rs`: a missed symbol
//! is acceptable degradation; an *invented* one is not.

use strata_core::{CallRef, ImportRef, NodeKind, RawSymbol};
use strata_lang_py::analyze;

/// Convenience: all symbols with a given kind.
fn symbols_of(file: &strata_core::AnalyzedFile, kind: NodeKind) -> Vec<&RawSymbol> {
    file.symbols.iter().filter(|s| s.kind == kind).collect()
}

fn names(symbols: &[&RawSymbol]) -> Vec<String> {
    symbols.iter().map(|s| s.name.clone()).collect()
}

fn find_call<'a>(file: &'a strata_core::AnalyzedFile, callee: &str) -> &'a CallRef {
    file.calls
        .iter()
        .find(|c| c.callee_name == callee)
        .unwrap_or_else(|| panic!("missing call to {callee}: {:?}", file.calls))
}

// --- #1: module-level function definitions ----------------------------------

#[test]
fn extracts_module_functions() {
    let src = "def foo(a, b):\n    return a\n\ndef bar():\n    pass\n";
    let file = analyze("src/a.py", src);
    let funcs = symbols_of(&file, NodeKind::Function);
    let mut got = names(&funcs);
    got.sort();
    assert_eq!(got, vec!["bar".to_string(), "foo".to_string()]);

    let foo = funcs.iter().find(|s| s.name == "foo").unwrap();
    let bar = funcs.iter().find(|s| s.name == "bar").unwrap();
    assert_eq!(foo.span.start_line, 1, "foo starts on line 1");
    assert_eq!(bar.span.start_line, 4, "bar starts on line 4");
    assert_eq!(foo.fqn, "foo");
    assert!(foo.container_fqn.is_none());
}

// --- #2: async def + decorated def ------------------------------------------

#[test]
fn extracts_async_and_decorated_functions() {
    // The decorator and `async` keyword must NOT prevent extraction; the symbol
    // is named after the function, decorators are just metadata.
    let src = "@app.route('/x')\nasync def handler(event):\n    return 1\n\n@decorator\ndef plain():\n    pass\n";
    let file = analyze("src/h.py", src);
    let funcs = symbols_of(&file, NodeKind::Function);
    let mut got = names(&funcs);
    got.sort();
    assert_eq!(got, vec!["handler".to_string(), "plain".to_string()]);
    // The decorated function's span should cover the def (not the decorator line),
    // and its fqn is the bare name.
    let handler = funcs.iter().find(|s| s.name == "handler").unwrap();
    assert_eq!(handler.fqn, "handler");
    assert!(handler.container_fqn.is_none());
}

// --- #3: class + methods (incl. staticmethod/classmethod/property) ----------

#[test]
fn extracts_class_and_methods() {
    let src = concat!(
        "class Dog(Animal, Mixin):\n",
        "    @staticmethod\n",
        "    def make():\n",
        "        pass\n",
        "    @classmethod\n",
        "    def species(cls):\n",
        "        return 'dog'\n",
        "    @property\n",
        "    def name(self):\n",
        "        return self._n\n",
        "    def speak(self):\n",
        "        self.bark()\n",
    );
    let file = analyze("src/dog.py", src);

    // The class symbol.
    let classes = symbols_of(&file, NodeKind::Class);
    assert_eq!(names(&classes), vec!["Dog".to_string()]);
    assert_eq!(classes[0].fqn, "Dog");
    assert!(classes[0].container_fqn.is_none());

    // All four members are Methods — @staticmethod/@classmethod/@property are
    // NOT special-cased; they are methods of the class.
    let methods = symbols_of(&file, NodeKind::Method);
    let mut got = names(&methods);
    got.sort();
    assert_eq!(
        got,
        vec![
            "make".to_string(),
            "name".to_string(),
            "speak".to_string(),
            "species".to_string()
        ]
    );
    for m in &methods {
        assert_eq!(
            m.container_fqn.as_deref(),
            Some("Dog"),
            "{} container",
            m.name
        );
        assert_eq!(m.fqn, format!("Dog.{}", m.name), "{} fqn", m.name);
    }
}

// --- #4: nested function is NOT a method, but is a Function -------------------

#[test]
fn nested_function_is_a_module_function_not_a_method() {
    // A `def inner()` nested inside `def top()` is a closure, not a class member.
    // It must be a Function symbol with no container (we do not invent a class).
    let src = "def top():\n    def inner():\n        pass\n    return inner\n";
    let file = analyze("src/n.py", src);
    let funcs = symbols_of(&file, NodeKind::Function);
    let mut got = names(&funcs);
    got.sort();
    assert_eq!(got, vec!["inner".to_string(), "top".to_string()]);
    let inner = funcs.iter().find(|s| s.name == "inner").unwrap();
    assert!(
        inner.container_fqn.is_none(),
        "a nested def is not a class member"
    );
    // No Method symbols at all (nothing is inside a class body here).
    assert!(
        symbols_of(&file, NodeKind::Method).is_empty(),
        "no methods: nested defs are functions, got {:?}",
        file.symbols
    );
}

// --- #5: imports (plain, aliased dotted, relative, deep-relative, star) -------

fn import_with_specifier<'a>(
    file: &'a strata_core::AnalyzedFile,
    spec: &str,
) -> Vec<&'a ImportRef> {
    file.imports
        .iter()
        .filter(|i| i.specifier == spec)
        .collect()
}

#[test]
fn extracts_plain_and_aliased_imports() {
    let src = "import os\nimport os.path as p\n";
    let file = analyze("src/a.py", src);

    // `import os` → specifier "os", bound name "os".
    let os = import_with_specifier(&file, "os");
    assert_eq!(os.len(), 1, "one `import os`, got {:?}", file.imports);
    assert_eq!(os[0].imported_names, vec!["os".to_string()]);

    // `import os.path as p` → specifier "os.path", bound name "p" (the alias).
    let osp = import_with_specifier(&file, "os.path");
    assert_eq!(osp.len(), 1, "one `import os.path as p`");
    assert_eq!(
        osp[0].imported_names,
        vec!["p".to_string()],
        "the alias is the bound name"
    );
}

#[test]
fn extracts_from_imports_relative_and_deep() {
    let src = "from .rel import a, b as c\nfrom ..pkg.sub import d\n";
    let file = analyze("src/pkg/m.py", src);

    // `from .rel import a, b as c` → specifier ".rel"; bound names a, c.
    let rel = import_with_specifier(&file, ".rel");
    assert_eq!(
        rel.len(),
        1,
        "one `from .rel import`, got {:?}",
        file.imports
    );
    assert_eq!(
        rel[0].imported_names,
        vec!["a".to_string(), "c".to_string()],
        "the alias `c` is the local binding for `b`"
    );

    // `from ..pkg.sub import d` → specifier "..pkg.sub"; bound name d.
    let deep = import_with_specifier(&file, "..pkg.sub");
    assert_eq!(deep.len(), 1, "one deep-relative from-import");
    assert_eq!(deep[0].imported_names, vec!["d".to_string()]);
}

#[test]
fn star_import_is_recorded_without_inventing_names() {
    // `from mod import *` is a dynamic surface: we record the import (specifier
    // "mod") but bind NO names — a star import never lets us invent a binding.
    let src = "from mod import *\n";
    let file = analyze("src/a.py", src);
    let star = import_with_specifier(&file, "mod");
    assert_eq!(
        star.len(),
        1,
        "the star import is recorded, got {:?}",
        file.imports
    );
    assert!(
        star[0].imported_names.is_empty(),
        "a star import binds no specific name (never invented): {:?}",
        star[0].imported_names
    );
}

// --- #6: calls with receivers + enclosing fqn --------------------------------

#[test]
fn extracts_calls_with_receiver_and_enclosing_fqn() {
    let src = concat!(
        "def f():\n",
        "    g()\n",
        "    obj.m()\n",
        "    pkg.mod.func()\n",
        "\n",
        "top()\n",
    );
    let file = analyze("src/a.py", src);

    // Bare call `g()`.
    let g = find_call(&file, "g");
    assert_eq!(g.receiver, None);
    assert_eq!(g.enclosing_fqn, "f");

    // Attribute call `obj.m()` → receiver "obj".
    let m = find_call(&file, "m");
    assert_eq!(m.receiver.as_deref(), Some("obj"));
    assert_eq!(m.enclosing_fqn, "f");

    // Nested attribute chain `pkg.mod.func()` → callee "func", receiver the
    // immediate object text `pkg.mod` (the honest receiver chain).
    let func = find_call(&file, "func");
    assert_eq!(func.callee_name, "func");
    assert_eq!(
        func.receiver.as_deref(),
        Some("pkg.mod"),
        "the receiver is the full immediate-object text"
    );
    assert_eq!(func.enclosing_fqn, "f");

    // `top()` at module level → empty enclosing fqn.
    let top = find_call(&file, "top");
    assert_eq!(top.receiver, None);
    assert_eq!(top.enclosing_fqn, "");
}

#[test]
fn method_body_calls_have_method_enclosing_fqn_and_self_receiver() {
    let src = concat!(
        "class C:\n",
        "    def m(self):\n",
        "        helper()\n",
        "        self.n()\n",
    );
    let file = analyze("src/c.py", src);

    let helper = find_call(&file, "helper");
    assert_eq!(
        helper.enclosing_fqn, "C.m",
        "call attributed to the method fqn"
    );
    assert_eq!(helper.receiver, None);

    let n = find_call(&file, "n");
    assert_eq!(n.enclosing_fqn, "C.m");
    assert_eq!(
        n.receiver.as_deref(),
        Some("self"),
        "`self.n()` records receiver `self`"
    );
}

// --- #7: dynamic dispatch is NEVER invented as a static call -----------------

#[test]
fn getattr_dynamic_call_is_not_invented() {
    // `getattr(obj, 'x')()` — the call target is computed. The OUTER call's
    // `function` is itself a call, not a name/attribute, so it yields no
    // attributable callee (we never guess the dynamic target). The INNER
    // `getattr(...)` IS a real bare call and is recorded honestly.
    let src = "def f():\n    getattr(obj, 'x')()\n";
    let file = analyze("src/d.py", src);
    // `getattr` is a real call we can see.
    assert!(
        file.calls
            .iter()
            .any(|c| c.callee_name == "getattr" && c.receiver.is_none()),
        "the getattr call itself is recorded: {:?}",
        file.calls
    );
    // The dynamic outer invocation must NOT invent a callee name.
    assert!(
        !file.calls.iter().any(|c| c.callee_name == "x"),
        "the dynamic getattr target must not be invented as a static call: {:?}",
        file.calls
    );
    // Exactly one call (the getattr) — the outer dynamic invocation adds none.
    assert_eq!(
        file.calls.len(),
        1,
        "only the getattr call is recorded, got {:?}",
        file.calls
    );
}

// --- #8: contract-plane extraction coexists with the code plane ---------------

#[test]
fn contract_extraction_coexists_with_code_plane() {
    // A Flask route + a `requests` consumer call: both contract signals are
    // extracted, and the code plane (symbols + calls) is unaffected. (This source
    // has no GraphQL, so gql_documents/resolver_entries are legitimately empty.)
    let src = concat!(
        "@app.route('/users')\n",
        "def list_users():\n",
        "    return requests.get('/upstream')\n",
    );
    let file = analyze("src/api.py", src);
    assert_eq!(file.routes.len(), 1, "Flask route: {:?}", file.routes);
    assert_eq!(file.routes[0].method, "GET");
    assert_eq!(file.routes[0].path, "/users");
    assert_eq!(
        file.http_calls.len(),
        1,
        "requests consumer: {:?}",
        file.http_calls
    );
    assert_eq!(file.http_calls[0].method.as_deref(), Some("GET"));
    assert!(file.gql_documents.is_empty());
    assert!(file.resolver_entries.is_empty());
    // The code plane is unaffected: the function and its calls still exist.
    assert!(file.symbols.iter().any(|s| s.name == "list_users"));
    assert!(file.calls.iter().any(|c| c.callee_name == "get"));
}

// --- #9: messy/partial file does not panic; valid symbols survive ------------

#[test]
fn messy_file_does_not_panic_and_keeps_valid_symbols() {
    // `ok` is valid and appears before the broken line.
    let src = "def ok():\n    pass\n\ndef broken(:\n    pass\n";
    let file = analyze("src/a.py", src); // must not panic
    let funcs = symbols_of(&file, NodeKind::Function);
    assert!(
        funcs.iter().any(|s| s.name == "ok"),
        "valid symbol before error should survive: {:?}",
        file.symbols
    );
}

// --- SQL candidate capture (data plane, Slice 16 D3 M2) ----------------------
//
// Each SQL-looking string literal becomes a `SqlCandidate` carrying the literal's
// inner text + enclosing function fqn. A non-SQL string is NOT captured; an
// f-string (`f"… {x} …"`) is NOT a single constant literal → dynamic SQL, dropped.

fn sql_texts(file: &strata_core::AnalyzedFile) -> Vec<&str> {
    file.sql_candidates
        .iter()
        .map(|c| c.text.as_str())
        .collect()
}

#[test]
fn py_captures_sql_string_literal_with_enclosing_fqn() {
    let src = "def load_user():\n    return db.execute(\"SELECT email FROM users WHERE id = 1\")\n";
    let file = analyze("app.py", src);
    assert_eq!(
        sql_texts(&file),
        vec!["SELECT email FROM users WHERE id = 1"],
        "the SQL string literal is captured with its quotes stripped"
    );
    assert_eq!(file.sql_candidates[0].enclosing_fqn, "load_user");
}

#[test]
fn py_captures_sql_in_triple_quoted_string() {
    let src = "def f():\n    return run('''INSERT INTO orders (id) VALUES (1)''')\n";
    let file = analyze("app.py", src);
    assert_eq!(sql_texts(&file), vec!["INSERT INTO orders (id) VALUES (1)"]);
    assert_eq!(file.sql_candidates[0].enclosing_fqn, "f");
}

#[test]
fn py_does_not_capture_non_sql_string() {
    let src = "def f():\n    msg = \"please update your profile from the menu\"\n    log(msg)\n";
    // 'update' + 'from' but no 'set' / no 'select' — must NOT match (needs the
    // companion keyword). This guards the prefilter, not the parser.
    let file = analyze("app.py", src);
    assert!(
        file.sql_candidates.is_empty(),
        "a non-SQL prose string must not be captured, got {:?}",
        file.sql_candidates
    );
}

#[test]
fn py_does_not_capture_fstring_sql_dynamic() {
    let src = "def f(table):\n    return db.execute(f\"SELECT * FROM {table}\")\n";
    let file = analyze("app.py", src);
    assert!(
        file.sql_candidates.is_empty(),
        "an f-string (dynamic SQL) must not be captured, got {:?}",
        file.sql_candidates
    );
}

#[test]
fn py_module_top_level_sql_has_empty_enclosing_fqn() {
    let src = "Q = \"DELETE FROM sessions WHERE expired = true\"\n";
    let file = analyze("app.py", src);
    assert_eq!(
        sql_texts(&file),
        vec!["DELETE FROM sessions WHERE expired = true"]
    );
    assert_eq!(file.sql_candidates[0].enclosing_fqn, "");
}

// --- ORM model hints (Slice 25, D3, M2b) ------------------------------------
//
// SQLAlchemy `__tablename__ = "…"` and Django nested `class Meta: db_table = "…"`,
// explicit literal table names only. A dynamic name or a model with no explicit
// name yields NO hint (never invented, R1/R5).

use strata_core::OrmFramework;

#[test]
fn py_sqlalchemy_tablename_yields_one_orm_hint() {
    let src = "class User(Base):\n    __tablename__ = \"users\"\n    id = Column(Integer)\n";
    let file = analyze("models.py", src);
    assert_eq!(
        file.orm_models.len(),
        1,
        "exactly one ORM hint: {:?}",
        file.orm_models
    );
    let h = &file.orm_models[0];
    assert_eq!(h.model_fqn, "User", "the model class fqn");
    assert_eq!(h.table_name, "users", "the unquoted literal table name");
    assert_eq!(h.framework, OrmFramework::SqlAlchemy);
}

#[test]
fn py_django_meta_db_table_yields_one_orm_hint() {
    let src = "class User(models.Model):\n    name = models.CharField()\n    class Meta:\n        db_table = \"users\"\n";
    let file = analyze("models.py", src);
    assert_eq!(
        file.orm_models.len(),
        1,
        "exactly one ORM hint: {:?}",
        file.orm_models
    );
    let h = &file.orm_models[0];
    assert_eq!(h.model_fqn, "User", "the OUTER model class fqn, not Meta");
    assert_eq!(h.table_name, "users");
    assert_eq!(h.framework, OrmFramework::Django);
}

#[test]
fn py_dynamic_tablename_yields_no_orm_hint() {
    // `__tablename__ = PREFIX + "users"` is a binary_operator, not a string literal
    // → never invented.
    let src = "class User(Base):\n    __tablename__ = PREFIX + \"users\"\n";
    let file = analyze("models.py", src);
    assert!(
        file.orm_models.is_empty(),
        "a dynamic table name yields no hint: {:?}",
        file.orm_models
    );
}

#[test]
fn py_sqlalchemy_annotated_tablename_yields_one_orm_hint() {
    // SQLAlchemy 2.0 declarative style annotates the assignment:
    // `__tablename__: str = "users"`. tree-sitter parses this as the same
    // `assignment` node carrying an extra `type:` field — the explicit literal
    // table name must still be captured.
    let src = "class User(Base):\n    __tablename__: str = \"users\"\n    id = Column(Integer)\n";
    let file = analyze("models.py", src);
    assert_eq!(
        file.orm_models.len(),
        1,
        "exactly one ORM hint for the annotated form: {:?}",
        file.orm_models
    );
    let h = &file.orm_models[0];
    assert_eq!(h.model_fqn, "User", "the model class fqn");
    assert_eq!(h.table_name, "users", "the unquoted literal table name");
    assert_eq!(h.framework, OrmFramework::SqlAlchemy);
}

#[test]
fn py_django_annotated_meta_db_table_yields_one_orm_hint() {
    // Django can be written with an annotated `db_table: str = "users"` inside
    // `class Meta` — same `assignment`-with-`type` shape, still one hint.
    let src = "class User(models.Model):\n    name = models.CharField()\n    class Meta:\n        db_table: str = \"users\"\n";
    let file = analyze("models.py", src);
    assert_eq!(
        file.orm_models.len(),
        1,
        "exactly one ORM hint for the annotated Django form: {:?}",
        file.orm_models
    );
    let h = &file.orm_models[0];
    assert_eq!(h.model_fqn, "User", "the OUTER model class fqn, not Meta");
    assert_eq!(h.table_name, "users");
    assert_eq!(h.framework, OrmFramework::Django);
}

#[test]
fn py_dynamic_annotated_tablename_yields_no_orm_hint() {
    // An annotated assignment whose RHS is still dynamic
    // (`__tablename__: str = PREFIX + "x"`) is a binary_operator, not a string
    // literal → never invented even though the annotation is present.
    let src = "class User(Base):\n    __tablename__: str = PREFIX + \"x\"\n";
    let file = analyze("models.py", src);
    assert!(
        file.orm_models.is_empty(),
        "a dynamic annotated table name yields no hint: {:?}",
        file.orm_models
    );
}

#[test]
fn py_model_without_explicit_table_name_yields_no_orm_hint() {
    // A SQLAlchemy class relying on the implicit class-name convention (no
    // `__tablename__`) → no hint this slice (convention inference is deferred).
    let src = "class User(Base):\n    id = Column(Integer)\n";
    let file = analyze("models.py", src);
    assert!(
        file.orm_models.is_empty(),
        "no explicit name → no hint: {:?}",
        file.orm_models
    );
}

#[test]
fn py_plain_class_yields_no_orm_hint() {
    // An ordinary (non-model) class with no table declaration → no hint.
    let src = "class Helper:\n    def run(self):\n        return 1\n";
    let file = analyze("util.py", src);
    assert!(
        file.orm_models.is_empty(),
        "a plain class yields no hint: {:?}",
        file.orm_models
    );
}
