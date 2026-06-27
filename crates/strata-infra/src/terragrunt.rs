//! Terragrunt structural extraction (Track D1, Slice 14, M2).
//!
//! A `terragrunt.hcl` describes a Terragrunt *unit* — a deployable directory that
//! wraps a Terraform module. We extract its STRUCTURE only:
//! - `terraform { source = "…" }` — where the unit's module comes from.
//! - `dependency "<name>" { config_path = "<relative dir>" }` — a structural
//!   dependency on ANOTHER unit, recorded as the literal `config_path`.
//!
//! ## Honest bound (scope §7): NO Terragrunt evaluation
//!
//! Terragrunt's real power — `read_terragrunt_config`, `find_in_parent_folders`,
//! `dependency.<name>.outputs.<attr>`, `locals`, `try()`, generate/remote-state
//! blocks — is a full HCL-function/dependency evaluator, which is OUT of scope
//! (reimplementing Terragrunt). So:
//! - We capture `source` and `config_path` only when they are STRING LITERALS. A
//!   `source = "${local.base}//mod"` or a `config_path = find_in_parent_folders()`
//!   is an unevaluated expression → recorded as `None`/skipped, never guessed.
//! - We do NOT resolve `dependency.<name>.outputs.*`, so cross-unit *attribute*
//!   wiring stays Unresolved. The unit-to-unit dependency edge (from the literal
//!   `config_path`) is the structural fact we can state honestly.

use hcl::structure::{Body, Structure};
use hcl::Expression;

use crate::InfraError;

/// A structurally-extracted Terragrunt unit (one `terragrunt.hcl`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TerragruntUnit {
    /// The `terragrunt.hcl` path (caller-supplied; repo-relative).
    pub path: String,
    /// The literal `terraform { source = … }`, when it is a string literal (an
    /// unevaluated interpolation/function is `None` — never guessed).
    pub source: Option<String>,
    /// The `dependency`/`dependencies` config paths declared by this unit, as the
    /// literal (relative) directory strings, in declaration order. These are the
    /// structural unit-to-unit dependencies.
    pub dependencies: Vec<TerragruntDependency>,
}

/// One structural Terragrunt dependency: the literal `config_path` and the
/// dependency block's name (when it is a named `dependency "x"` block; the bulk
/// `dependencies { paths = [...] }` form has no per-path name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerragruntDependency {
    /// The dependency block label (`dependency "vpc"` → `vpc`), or `None` for a
    /// path from the bulk `dependencies { paths = [...] }` block.
    pub name: Option<String>,
    /// The literal `config_path` (a relative directory like `../vpc`).
    pub config_path: String,
}

/// Whether `filename` is a Terragrunt config. Terragrunt configs are conventionally
/// named `terragrunt.hcl` (or `*.hcl` includes like `common.hcl`/`root.hcl`); the
/// canonical unit file is `terragrunt.hcl`, which is what we route here. Detection
/// is by basename so a path like `infra/prod/app/terragrunt.hcl` matches.
pub fn is_terragrunt_file(filename: &str) -> bool {
    let base = filename.rsplit('/').next().unwrap_or(filename);
    base == "terragrunt.hcl"
}

/// Parse a `terragrunt.hcl` into a [`TerragruntUnit`] (structure only). A malformed
/// config returns [`InfraError::Parse`] so the caller degrades visibly; an empty or
/// dependency-free unit yields a unit with no dependencies (still valid).
pub fn extract_unit(path: &str, content: &str) -> Result<TerragruntUnit, InfraError> {
    let body = hcl::from_str::<Body>(content).map_err(|e| InfraError::Parse {
        path: path.to_string(),
        msg: format!("invalid HCL: {e}"),
    })?;

    let mut unit = TerragruntUnit {
        path: path.to_string(),
        source: None,
        dependencies: Vec::new(),
    };

    for structure in body.iter() {
        let Structure::Block(block) = structure else {
            continue;
        };
        match block.identifier() {
            // terraform { source = "…" } — record a LITERAL source only.
            "terraform" => {
                if let Some(Expression::String(s)) = attr_expr(block.body(), "source") {
                    unit.source = Some(s.clone());
                }
            }
            // dependency "name" { config_path = "../x" } — a named structural dep.
            "dependency" => {
                let name = block.labels().first().map(|l| l.as_str().to_string());
                if let Some(Expression::String(cp)) = attr_expr(block.body(), "config_path") {
                    unit.dependencies.push(TerragruntDependency {
                        name,
                        config_path: cp.clone(),
                    });
                }
            }
            // dependencies { paths = ["../a", "../b"] } — the bulk form: each
            // literal path is a structural dep (no per-path name).
            "dependencies" => {
                if let Some(Expression::Array(items)) = attr_expr(block.body(), "paths") {
                    for item in items {
                        if let Expression::String(cp) = item {
                            unit.dependencies.push(TerragruntDependency {
                                name: None,
                                config_path: cp.clone(),
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    Ok(unit)
}

/// The expression of attribute `key` directly under `body`, or `None`.
fn attr_expr<'a>(body: &'a Body, key: &str) -> Option<&'a Expression> {
    body.iter().find_map(|s| match s {
        Structure::Attribute(a) if a.key() == key => Some(a.expr()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNIT: &str = r#"
locals {
  env = "prod"
}

include "root" {
  path = find_in_parent_folders()
}

terraform {
  source = "git::git@github.com:acme/modules.git//lambda?ref=v1.2.0"
}

dependency "vpc" {
  config_path = "../vpc"
  mock_outputs = {
    vpc_id = "vpc-fake"
  }
}

dependency "db" {
  config_path = "../../shared/db"
}

inputs = {
  vpc_id = dependency.vpc.outputs.vpc_id
}
"#;

    #[test]
    fn detects_terragrunt_by_basename() {
        assert!(is_terragrunt_file("terragrunt.hcl"));
        assert!(is_terragrunt_file("infra/prod/app/terragrunt.hcl"));
        assert!(!is_terragrunt_file("main.tf"));
        assert!(!is_terragrunt_file("common.hcl"));
    }

    #[test]
    fn extracts_source_and_dependencies_structurally() {
        let u = extract_unit("infra/prod/app/terragrunt.hcl", UNIT).expect("parses");
        assert_eq!(
            u.source.as_deref(),
            Some("git::git@github.com:acme/modules.git//lambda?ref=v1.2.0")
        );
        assert_eq!(
            u.dependencies,
            vec![
                TerragruntDependency {
                    name: Some("vpc".to_string()),
                    config_path: "../vpc".to_string()
                },
                TerragruntDependency {
                    name: Some("db".to_string()),
                    config_path: "../../shared/db".to_string()
                },
            ]
        );
    }

    #[test]
    fn unevaluated_source_is_none_never_guessed() {
        // `source = "${local.base}//mod"` is an interpolation we do NOT evaluate →
        // None, never a guessed value.
        let src = concat!(
            "terraform {\n",
            "  source = \"${local.base}//mod\"\n",
            "}\n",
        );
        let u = extract_unit("t/terragrunt.hcl", src).expect("parses");
        assert_eq!(u.source, None, "an interpolated source is not captured");
    }

    #[test]
    fn dependency_with_nonliteral_config_path_is_skipped() {
        // `config_path = find_in_parent_folders()` is a function call we never
        // evaluate → no structural dependency recorded (never invented).
        let src = concat!(
            "dependency \"x\" {\n",
            "  config_path = find_in_parent_folders()\n",
            "}\n",
        );
        let u = extract_unit("t/terragrunt.hcl", src).expect("parses");
        assert!(
            u.dependencies.is_empty(),
            "a non-literal config_path records no dependency: {:?}",
            u.dependencies
        );
    }

    #[test]
    fn bulk_dependencies_paths_form() {
        let src = concat!(
            "dependencies {\n",
            "  paths = [\"../a\", \"../b\"]\n",
            "}\n",
        );
        let u = extract_unit("t/terragrunt.hcl", src).expect("parses");
        assert_eq!(
            u.dependencies,
            vec![
                TerragruntDependency {
                    name: None,
                    config_path: "../a".to_string()
                },
                TerragruntDependency {
                    name: None,
                    config_path: "../b".to_string()
                },
            ]
        );
    }

    #[test]
    fn malformed_terragrunt_returns_parse_error() {
        let err = extract_unit("bad/terragrunt.hcl", "dependency \"x\" {\n").unwrap_err();
        match err {
            InfraError::Parse { path, .. } => assert_eq!(path, "bad/terragrunt.hcl"),
        }
    }
}
