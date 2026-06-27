//! Python contract-plane *extraction* tests across all four signal kinds: producer
//! routes (Flask/FastAPI/Django), REST consumer `http_calls` (`requests`/`httpx`),
//! GraphQL consumer `gql_documents` (`gql("…")`), and GraphQL producer
//! `resolver_entries` (Graphene/Strawberry/Ariadne). Each runs `analyze` on an
//! inline source string and asserts on the `AnalyzedFile` contract fields. The
//! conservative rule mirrors the TS adapter: a missed signal is acceptable
//! degradation, an invented one is not (R1/R5).

use strata_core::{GqlDocument, HttpCall, ResolverEntry, RouteDecl, UrlShape, ROUTE_METHOD_ANY};
use strata_lang_py::analyze;

/// Routes from a source string, sorted by `(method, path)` for stable assertions.
fn routes(src: &str) -> Vec<RouteDecl> {
    let mut r = analyze("src/app.py", src).routes;
    r.sort_by(|a, b| {
        (a.method.as_str(), a.path.as_str()).cmp(&(b.method.as_str(), b.path.as_str()))
    });
    r
}

// --- Flask / FastAPI decorator routes ---------------------------------------

#[test]
fn flask_route_defaults_to_get() {
    let r = routes("@app.route('/users')\ndef list_users():\n    pass\n");
    assert_eq!(r.len(), 1, "one route: {r:?}");
    assert_eq!(r[0].method, "GET");
    assert_eq!(r[0].path, "/users");
    assert_eq!(r[0].handler_name.as_deref(), Some("list_users"));
}

#[test]
fn flask_route_methods_list_yields_one_route_per_method() {
    let r = routes("@app.route('/users', methods=['GET', 'POST'])\ndef users():\n    pass\n");
    assert_eq!(r.len(), 2, "one route per declared method: {r:?}");
    assert_eq!(r[0].method, "GET");
    assert_eq!(r[1].method, "POST");
    assert!(r.iter().all(|x| x.path == "/users"));
    assert!(r.iter().all(|x| x.handler_name.as_deref() == Some("users")));
}

#[test]
fn fastapi_verb_decorator() {
    let r = routes("@router.post('/items')\ndef create():\n    pass\n");
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].method, "POST");
    assert_eq!(r[0].path, "/items");
    assert_eq!(r[0].handler_name.as_deref(), Some("create"));
}

#[test]
fn flask_get_shorthand() {
    let r = routes("@app.get('/health')\ndef health():\n    return 'ok'\n");
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].method, "GET");
    assert_eq!(r[0].path, "/health");
    assert_eq!(r[0].handler_name.as_deref(), Some("health"));
}

#[test]
fn decorated_method_route_attributes_to_class_method() {
    // A route decorator on a class method: handler is the method, fqn is Class.method.
    let r = routes("class V:\n    @router.get('/v')\n    def handler(self):\n        pass\n");
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].method, "GET");
    assert_eq!(r[0].handler_name.as_deref(), Some("handler"));
    assert_eq!(r[0].enclosing_fqn, "V.handler");
}

// --- Django URLconf routes (method-less) ------------------------------------

#[test]
fn django_path_is_method_less_with_converter() {
    let r = routes("urlpatterns = [path('users/<int:pk>/', views.user_detail)]\n");
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].method, ROUTE_METHOD_ANY);
    // The `path()` converter pattern is canonicalised to the OpenAPI shape (leading
    // slash, no trailing) so it can match an operation path.
    assert_eq!(r[0].path, "/users/<int:pk>");
    // Cross-file view referenced as `views.user_detail` → the trailing leaf name.
    assert_eq!(r[0].handler_name.as_deref(), Some("user_detail"));
}

#[test]
fn django_re_path_with_identifier_view() {
    let r = routes("urlpatterns = [re_path(r'^home$', home_view)]\n");
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].method, ROUTE_METHOD_ANY);
    assert_eq!(r[0].path, "^home$");
    assert_eq!(r[0].handler_name.as_deref(), Some("home_view"));
}

#[test]
fn django_class_based_view_handler_falls_back() {
    // `Class.as_view()` is a call expression, not a name → no handler; the producer
    // falls back to the module at link time rather than guessing.
    let r = routes("urlpatterns = [path('items/', ItemList.as_view())]\n");
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].method, ROUTE_METHOD_ANY);
    assert_eq!(r[0].path, "/items");
    assert_eq!(r[0].handler_name, None);
}

// --- false-positive guards (never invent a route) ---------------------------

#[test]
fn non_route_decorators_are_not_routes() {
    // @dataclass / @property (bare identifiers), @functools.wraps(f) and
    // @app.middleware('http') (calls whose verb is not a route verb) — none is a route.
    assert!(routes("@dataclass\nclass X:\n    pass\n").is_empty());
    assert!(routes("@property\ndef x(self):\n    pass\n").is_empty());
    assert!(routes("@functools.wraps(f)\ndef g():\n    pass\n").is_empty());
    assert!(routes("@app.middleware('http')\nasync def mw(req, nxt):\n    pass\n").is_empty());
}

#[test]
fn dynamic_or_fstring_route_path_is_not_guessed() {
    // A non-literal path (a bare identifier or an f-string) yields no route.
    assert!(routes("@app.get(ROUTE)\ndef h():\n    pass\n").is_empty());
    assert!(routes("@app.get(f'/u/{x}')\ndef h():\n    pass\n").is_empty());
}

#[test]
fn bare_path_call_without_string_is_not_a_route() {
    // A `path(...)` identifier call whose first arg is not a string literal is not a
    // Django route (guards against a same-named local function).
    assert!(routes("path(x, y)\n").is_empty());
}

// --- REST consumer calls (requests / httpx) ---------------------------------

/// HTTP calls from a source string (extraction order preserved).
fn http_calls(src: &str) -> Vec<HttpCall> {
    analyze("src/client.py", src).http_calls
}

#[test]
fn requests_get_literal_url() {
    let h = http_calls("requests.get('/users/1')\n");
    assert_eq!(h.len(), 1, "{h:?}");
    assert_eq!(h[0].method.as_deref(), Some("GET"));
    assert_eq!(h[0].url, UrlShape::Literal("/users/1".to_string()));
}

#[test]
fn httpx_post_fstring_is_a_template() {
    let h = http_calls("httpx.post(f'/users/{uid}/posts')\n");
    assert_eq!(h.len(), 1, "{h:?}");
    assert_eq!(h[0].method.as_deref(), Some("POST"));
    assert_eq!(h[0].url, UrlShape::Template("/users/{}/posts".to_string()));
}

#[test]
fn requests_request_takes_method_from_first_arg() {
    let h = http_calls("requests.request('DELETE', '/users/1')\n");
    assert_eq!(h.len(), 1, "{h:?}");
    assert_eq!(h[0].method.as_deref(), Some("DELETE"));
    assert_eq!(h[0].url, UrlShape::Literal("/users/1".to_string()));
}

#[test]
fn dynamic_url_is_opaque() {
    // A bare identifier or a concatenation is neither literal nor template → Dynamic.
    let h = http_calls("requests.get(url)\n");
    assert_eq!(h.len(), 1, "{h:?}");
    assert_eq!(h[0].url, UrlShape::Dynamic);
    let h2 = http_calls("requests.get('/u/' + uid)\n");
    assert_eq!(h2.len(), 1, "{h2:?}");
    assert_eq!(h2[0].url, UrlShape::Dynamic);
}

#[test]
fn http_call_enclosing_is_the_caller_function() {
    let h = http_calls("def fetch():\n    return requests.get('/x')\n");
    assert_eq!(h.len(), 1, "{h:?}");
    assert_eq!(h[0].enclosing_fqn, "fetch");
}

#[test]
fn non_client_dot_get_is_not_an_http_call() {
    // Python `.get()` is ubiquitous; only requests/httpx receivers count.
    assert!(http_calls("d.get('key')\n").is_empty());
    assert!(http_calls("cache.get(user_id)\n").is_empty());
    assert!(http_calls("os.environ.get('PATH')\n").is_empty());
}

#[test]
fn requests_non_request_method_is_not_an_http_call() {
    // requests.Session() / requests.codes etc. are not outgoing requests.
    assert!(http_calls("requests.Session()\n").is_empty());
}

// --- GraphQL consumer documents (gql) ---------------------------------------

/// GraphQL documents from a source string.
fn gql_docs(src: &str) -> Vec<GqlDocument> {
    analyze("src/q.py", src).gql_documents
}

#[test]
fn gql_call_is_a_tagged_document() {
    let d = gql_docs("q = gql('query { getUser { id } }')\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].tagged, "gql() is an explicit (tagged) document");
    assert!(d[0].interpolation_free);
    assert!(
        d[0].text.contains("getUser"),
        "captured text: {:?}",
        d[0].text
    );
}

#[test]
fn gql_fstring_is_tagged_but_not_interpolation_free() {
    // An interpolated gql doc is counted (tagged) but never linked (opaque body).
    let d = gql_docs("q = gql(f'query getX {sel}')\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].tagged);
    assert!(!d[0].interpolation_free);
}

#[test]
fn non_gql_call_is_not_a_document() {
    // A different callee, or a non-literal argument, yields no document.
    assert!(gql_docs("other('query { x }')\n").is_empty());
    assert!(gql_docs("gql(variable)\n").is_empty());
}

// --- GraphQL producer resolvers (Graphene / Strawberry / Ariadne) -----------

/// Resolver entries from a source string, sorted by `(op_type, field)`.
fn resolvers(src: &str) -> Vec<ResolverEntry> {
    let mut r = analyze("src/schema.py", src).resolver_entries;
    r.sort_by(|a, b| {
        (a.op_type.as_str(), a.field.as_str()).cmp(&(b.op_type.as_str(), b.field.as_str()))
    });
    r
}

#[test]
fn graphene_resolve_methods_are_producers() {
    let src = concat!(
        "class Query(graphene.ObjectType):\n",
        "    user = graphene.Field(UserType)\n",
        "    def resolve_user(self, info, id):\n",
        "        return None\n",
    );
    let r = resolvers(src);
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].op_type, "Query");
    assert_eq!(r[0].field, "user");
    assert_eq!(r[0].handler_name.as_deref(), Some("resolve_user"));
    assert_eq!(r[0].enclosing_fqn, "Query.resolve_user");
}

#[test]
fn graphene_requires_objecttype_base_and_root_name() {
    // A class named Query WITHOUT the ObjectType base, or WITH the base but not a
    // root name, is not a resolver host (never guessed).
    assert!(resolvers("class Query:\n    def resolve_user(self):\n        pass\n").is_empty());
    assert!(resolvers(
        "class Helper(graphene.ObjectType):\n    def resolve_user(self):\n        pass\n"
    )
    .is_empty());
}

#[test]
fn strawberry_field_methods_are_producers() {
    let src = concat!(
        "@strawberry.type\n",
        "class Query:\n",
        "    @strawberry.field\n",
        "    def user(self) -> User:\n",
        "        return None\n",
    );
    let r = resolvers(src);
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].op_type, "Query");
    assert_eq!(r[0].field, "user");
    assert_eq!(r[0].handler_name.as_deref(), Some("user"));
    assert_eq!(r[0].enclosing_fqn, "Query.user");
}

#[test]
fn ariadne_field_decorator_is_a_producer() {
    let src = concat!(
        "query = QueryType()\n",
        "@query.field('user')\n",
        "def resolve_user(*_):\n",
        "    return None\n",
    );
    let r = resolvers(src);
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].op_type, "Query");
    assert_eq!(r[0].field, "user");
    assert_eq!(r[0].handler_name.as_deref(), Some("resolve_user"));
}

#[test]
fn ariadne_mutation_receiver_maps_to_mutation() {
    let src = concat!(
        "mutation = MutationType()\n",
        "@mutation.field('createUser')\n",
        "def resolve_create(*_):\n",
        "    return None\n",
    );
    let r = resolvers(src);
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].op_type, "Mutation");
    assert_eq!(r[0].field, "createUser");
}

#[test]
fn unrelated_decorated_function_is_not_a_resolver() {
    // A route decorator, or an unknown decorator receiver, yields no resolver entry.
    assert!(resolvers("@app.get('/x')\ndef h():\n    pass\n").is_empty());
    assert!(resolvers("@thing.field('x')\ndef h(*_):\n    pass\n").is_empty());
}
