//! Best-effort `tsconfig.json` → [`ResolveOptions`] parsing.
//!
//! Reads `compilerOptions.baseUrl` and `compilerOptions.paths`. Anything missing
//! or malformed yields [`ResolveOptions::default`] — indexing never fails because
//! of tsconfig. Note: standard `serde_json` is used, so JSONC features (comments,
//! trailing commas) are not supported and a tsconfig using them falls back to
//! defaults.

use serde_json::Value;
use strata_lang_ts::ResolveOptions;

/// Parse a tsconfig JSON string into [`ResolveOptions`]. Returns default options
/// for absent fields or any parse error.
pub fn parse_tsconfig(contents: &str) -> ResolveOptions {
    let Ok(root) = serde_json::from_str::<Value>(contents) else {
        return ResolveOptions::default();
    };
    let compiler_options = root.get("compilerOptions");

    let base_url = compiler_options
        .and_then(|c| c.get("baseUrl"))
        .and_then(Value::as_str)
        .map(str::to_string);

    let paths = compiler_options
        .and_then(|c| c.get("paths"))
        .and_then(Value::as_object)
        .map(|obj| {
            obj.iter()
                .map(|(pattern, targets)| {
                    let targets: Vec<String> = targets
                        .as_array()
                        .map(|arr| {
                            arr.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default();
                    (pattern.clone(), targets)
                })
                .collect()
        })
        .unwrap_or_default();

    ResolveOptions { base_url, paths }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_base_url_and_paths() {
        let json = r#"{
            "compilerOptions": {
                "baseUrl": "src",
                "paths": { "@app/*": ["app/*"], "@lib": ["lib/index.ts"] }
            }
        }"#;
        let opts = parse_tsconfig(json);
        assert_eq!(opts.base_url.as_deref(), Some("src"));
        assert!(opts
            .paths
            .iter()
            .any(|(p, t)| p == "@app/*" && t == &vec!["app/*".to_string()]));
        assert!(opts
            .paths
            .iter()
            .any(|(p, t)| p == "@lib" && t == &vec!["lib/index.ts".to_string()]));
    }

    #[test]
    fn missing_compiler_options_is_default() {
        let opts = parse_tsconfig("{}");
        assert!(opts.base_url.is_none());
        assert!(opts.paths.is_empty());
    }

    #[test]
    fn malformed_json_is_default() {
        let opts = parse_tsconfig("{ not valid json");
        assert!(opts.base_url.is_none());
        assert!(opts.paths.is_empty());
    }
}
