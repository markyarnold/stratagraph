//! The OpenAPI / Swagger adapter.
//!
//! Version-agnostic by construction: it parses the spec text into a generic
//! value (JSON via `serde_json`, YAML via `yaml-rust2`) and walks the `paths`
//! object — `path` → HTTP-method key → operation → `operationId`. Swagger v2 and
//! OpenAPI v3 both carry this same `paths` shape, so a single walk handles both
//! versions in both encodings.

use serde_json::{Map, Value};
use yaml_rust2::{Yaml, YamlLoader};

use crate::{normalize_path, ContractAdapter, ContractError, ContractFormat, OperationDef};

/// The HTTP-method keys an OpenAPI/Swagger path-item object may carry. Anything
/// else under a path entry (e.g. `parameters`, `summary`, `$ref`, `servers`) is
/// ignored — only these keys introduce an operation.
const HTTP_METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "patch", "head", "options", "trace",
];

/// Adapter for OpenAPI v3 and Swagger v2 specs, JSON or YAML.
pub struct OpenApiAdapter;

impl ContractAdapter for OpenApiAdapter {
    /// Detect an OpenAPI/Swagger spec.
    ///
    /// True when EITHER the filename looks like a spec (`openapi`/`swagger` stem)
    /// with a recognised data extension, OR the content parses and has a
    /// top-level `openapi`/`swagger` version key together with a `paths` map.
    /// The content check guards against a file merely *named* like a spec, and
    /// catches specs under other names.
    fn detects(&self, filename: &str, content: &str) -> bool {
        if filename_looks_like_spec(filename) {
            return true;
        }
        // Content heuristic: a real spec has a version marker + a paths map.
        match parse_to_value(content) {
            Ok(value) => {
                let has_version = value.get("openapi").is_some() || value.get("swagger").is_some();
                let has_paths = value.get("paths").map(Value::is_object).unwrap_or(false);
                has_version && has_paths
            }
            Err(_) => false,
        }
    }

    fn extract(&self, spec_path: &str, content: &str) -> Result<Vec<OperationDef>, ContractError> {
        let value = parse_to_value(content).map_err(|msg| ContractError::Parse {
            spec: spec_path.to_string(),
            msg,
        })?;

        // A spec must carry a `paths` object. Its absence is a structural parse
        // error (rather than yielding zero operations silently), so a truncated
        // or wrong-shaped document degrades visibly via the caller.
        let paths = value
            .get("paths")
            .and_then(Value::as_object)
            .ok_or_else(|| ContractError::Parse {
                spec: spec_path.to_string(),
                msg: "missing or non-object `paths`".to_string(),
            })?;

        Ok(extract_operations(paths, spec_path))
    }
}

/// Whether `filename` (any path; only the final component matters) has an
/// `openapi`/`swagger` stem and a `.json`/`.yaml`/`.yml` extension.
fn filename_looks_like_spec(filename: &str) -> bool {
    let base = filename.rsplit(['/', '\\']).next().unwrap_or(filename);
    let lower = base.to_ascii_lowercase();
    let stem_ok = lower.starts_with("openapi") || lower.starts_with("swagger");
    let ext_ok = lower.ends_with(".json") || lower.ends_with(".yaml") || lower.ends_with(".yml");
    stem_ok && ext_ok
}

/// Walk a `paths` object into [`OperationDef`]s, in a deterministic order
/// (path order as the underlying map yields, then [`HTTP_METHODS`] order).
fn extract_operations(paths: &Map<String, Value>, spec_path: &str) -> Vec<OperationDef> {
    let mut ops = Vec::new();
    for (raw_path, path_item) in paths {
        let Some(item_obj) = path_item.as_object() else {
            // A non-object path entry (e.g. a bare `$ref` string is uncommon at
            // this position) introduces no operations; skip it.
            continue;
        };
        for method in HTTP_METHODS {
            let Some(op_obj) = item_obj.get(method) else {
                continue;
            };
            // The operation value should be an object; a malformed scalar here
            // is simply treated as "no operationId".
            let operation_id = op_obj
                .as_object()
                .and_then(|o| o.get("operationId"))
                .and_then(Value::as_str)
                .map(str::to_string);

            let method_upper = method.to_ascii_uppercase();
            let norm_path = normalize_path(raw_path);
            let key = match &operation_id {
                Some(id) => id.clone(),
                None => format!("{method_upper} {norm_path}"),
            };

            ops.push(OperationDef {
                format: ContractFormat::OpenApi,
                key,
                method: method_upper,
                path: raw_path.clone(),
                norm_path,
                operation_id,
                spec_path: spec_path.to_string(),
            });
        }
    }
    ops
}

/// Parse spec text into a generic [`serde_json::Value`]. Tries JSON first (a
/// superset-friendly fast path), then YAML. On failure returns a human-readable
/// reason. YAML is converted into the same `Value` type so there is exactly one
/// downstream walker.
fn parse_to_value(content: &str) -> Result<Value, String> {
    // JSON first: a valid JSON document is also valid YAML, but parsing it as
    // JSON is unambiguous and avoids any YAML-specific coercion of, say, numeric
    // keys. If it isn't JSON we fall through to YAML.
    if let Ok(value) = serde_json::from_str::<Value>(content) {
        return Ok(value);
    }

    let docs = YamlLoader::load_from_str(content).map_err(|e| format!("invalid YAML: {e}"))?;
    let doc = docs
        .into_iter()
        .next()
        .ok_or_else(|| "empty YAML document".to_string())?;
    Ok(yaml_to_json(doc))
}

/// Convert a `yaml-rust2` [`Yaml`] value into a [`serde_json::Value`].
///
/// Mapping keys are coerced to strings (a YAML map can key on non-strings; an
/// OpenAPI spec never does, but coercing keeps the walk total). YAML's `Real`,
/// `BadValue`, and alias placeholders degrade to the closest JSON shape; none of
/// them appear at the spec positions we read (`paths`, method keys,
/// `operationId`), so this conversion is lossless for our purposes.
fn yaml_to_json(y: Yaml) -> Value {
    match y {
        Yaml::Null => Value::Null,
        Yaml::Boolean(b) => Value::Bool(b),
        Yaml::Integer(i) => Value::from(i),
        Yaml::Real(s) => s
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or(Value::String(s)),
        Yaml::String(s) => Value::String(s),
        Yaml::Array(items) => Value::Array(items.into_iter().map(yaml_to_json).collect()),
        Yaml::Hash(map) => {
            let mut obj = Map::new();
            for (k, v) in map {
                obj.insert(yaml_key_to_string(k), yaml_to_json(v));
            }
            Value::Object(obj)
        }
        // Anchors/aliases are resolved by the loader before we see them; a
        // residual alias index or a BadValue carries no spec data we read.
        Yaml::Alias(_) | Yaml::BadValue => Value::Null,
    }
}

/// Coerce a YAML mapping key to a `String`. Spec keys (`paths`, `/users/{id}`,
/// `get`, `operationId`) are always strings already; numeric/bool keys (legal in
/// YAML, absent in OpenAPI) get a stable textual form so no entry is dropped.
fn yaml_key_to_string(k: Yaml) -> String {
    match k {
        Yaml::String(s) => s,
        Yaml::Integer(i) => i.to_string(),
        Yaml::Boolean(b) => b.to_string(),
        Yaml::Real(s) => s,
        other => format!("{other:?}"),
    }
}
