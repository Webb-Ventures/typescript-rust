//! `import/extensions`: file extensions in import paths, per extension (`always`,
//! `never`, `ignorePackages`; default `never`).
//!
//! - Builtins and root packages (`x`, `@scope/x`) are left alone unless a
//!   `pathGroupOverrides` glob says `enforce`; an `ignore` override skips the path.
//! - A `?query` is dropped. The extension is the resolved file's (else the path's).
//! - A path that does not end in that extension: "Missing file extension" when its
//!   extension is `always` (packages exempt under `ignorePackages`). Type-only imports and
//!   exports are skipped unless `checkTypeImports`.
//! - A path that ends in it: "Unexpected use of file extension" when it is `never` and the
//!   path without the extension resolves to the same file (or neither resolves).

use super::import_util::{
    MODULE_KINDS, ModuleSystems, VisitorOptions, is_type_only_declaration, resolve,
};
use super::prelude::*;
use crate::config::glob::Glob;
use crate::modules::extname;
use crate::modules::import_type::{ImportType, import_type, is_builtin, is_module, is_scoped};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/extensions",
        "Ensure consistent use of file extension within the import path.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<Extensions>,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Modifier {
    Always,
    Never,
    Other,
}

fn modifier(v: &Value) -> Option<Modifier> {
    match v {
        Value::String(s) if s == "always" => Some(Modifier::Always),
        Value::String(s) if s == "never" => Some(Modifier::Never),
        Value::String(s) if s.is_empty() => None,
        Value::Null | Value::Bool(false) => None,
        _ => Some(Modifier::Other),
    }
}

#[derive(Debug)]
struct Extensions {
    default: Modifier,
    pattern: Vec<(String, Modifier)>,
    ignore_packages: bool,
    check_type_imports: bool,
    overrides: Vec<(Glob, bool)>,
}

impl FromOptions for Extensions {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mut default_name = "never".to_owned();
        let mut pattern: Vec<(String, Modifier)> = Vec::new();
        let mut ignore_packages = false;
        let mut check_type_imports = false;
        let mut overrides = Vec::new();
        let merge = |o: &serde_json::Map<String, Value>, pattern: &mut Vec<_>| {
            for (k, v) in o {
                pattern.retain(|(p, _): &(String, Modifier)| p != k);
                if let Some(m) = modifier(v) {
                    pattern.push((k.clone(), m));
                }
            }
        };
        for o in opts {
            match o {
                Value::String(s) => default_name = s.clone(),
                Value::Object(o) => {
                    let has = |k| o.get(k).is_some_and(|v| !v.is_null());
                    if !has("pattern") && !has("ignorePackages") && !has("checkTypeImports") {
                        merge(o, &mut pattern);
                        continue;
                    }
                    if let Some(Value::Object(p)) = o.get("pattern") {
                        merge(p, &mut pattern);
                    }
                    if let Some(v) = o.get("ignorePackages").filter(|v| !v.is_null()) {
                        ignore_packages = is_truthy(v);
                    }
                    if let Some(v) = o.get("checkTypeImports").filter(|v| !v.is_null()) {
                        check_type_imports = is_truthy(v);
                    }
                    if let Some(Value::Array(list)) = o.get("pathGroupOverrides") {
                        overrides = list
                            .iter()
                            .filter_map(|g| {
                                let pat = g["pattern"].as_str()?;
                                Some((Glob::new(pat), g["action"].as_str() == Some("enforce")))
                            })
                            .collect();
                    }
                }
                _ => {}
            }
        }
        if default_name == "ignorePackages" {
            default_name = "always".to_owned();
            ignore_packages = true;
        }
        Ok(Extensions {
            default: modifier(&Value::String(default_name)).unwrap_or(Modifier::Never),
            pattern,
            ignore_packages,
            check_type_imports,
            overrides,
        })
    }
}

fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        _ => true,
    }
}

/// `x` or `@scope/x`: a package root, whose name may itself end in `.js`.
fn is_external_root(file: &str) -> bool {
    if file == "." || file == ".." {
        return false;
    }
    let slashes = file.matches('/').count();
    slashes == 0 || (is_scoped(file) && slashes <= 1)
}

impl Extensions {
    fn modifier(&self, ext: &str) -> Modifier {
        self.pattern
            .iter()
            .find(|(k, _)| k == ext)
            .map_or(self.default, |(_, m)| *m)
    }

    fn check(&self, cx: &mut Ctx<'_, '_>, source: NodeId, importer: NodeId, full: &str) {
        if full.is_empty() {
            return;
        }
        // Some(true): enforce; Some(false): ignore.
        let action = self
            .overrides
            .iter()
            .find(|(g, _)| g.matches(full))
            .map(|(_, e)| *e);
        if action == Some(false) {
            return;
        }
        let enforce = action == Some(true);
        if !enforce && is_builtin(cx.file.modules(), full, None) {
            return;
        }
        let import_path = full.find('?').map_or(full, |i| &full[..i]);
        if !enforce && is_external_root(import_path) {
            return;
        }
        let Some(resolved) = resolve(cx, import_path) else {
            return;
        };
        let resolved_path = resolved.path().map(str::to_owned);
        let ext = extname(resolved_path.as_deref().unwrap_or(import_path));
        let ext = ext.strip_prefix('.').unwrap_or(ext).to_owned();
        let m = cx.file.modules();
        let is_package = ((is_module(import_path) || is_scoped(import_path))
            && import_type(m, import_path, resolved_path.as_deref()) == ImportType::External)
            || is_scoped(import_path);
        let span = cx.span(source);
        if ext.is_empty() || !import_path.ends_with(&format!(".{ext}")) {
            if !self.check_type_imports && is_type_only_declaration(cx.ast(), importer) {
                return;
            }
            let modifier = self.modifier(&ext);
            let required =
                modifier == Modifier::Always && (!self.ignore_packages || enforce || !is_package);
            if required {
                let shown = if ext.is_empty() {
                    String::new()
                } else {
                    format!("\"{ext}\" ")
                };
                cx.report(
                    span,
                    format!("Missing file extension {shown}for \"{full}\""),
                );
            }
        } else if self.modifier(&ext) == Modifier::Never {
            let without = &import_path[..import_path.len() - ext.len() - 1];
            let Some(bare) = resolve(cx, without) else {
                return;
            };
            if bare.path() == resolved_path.as_deref() {
                cx.report(
                    span,
                    format!("Unexpected use of file extension \"{ext}\" for \"{full}\""),
                );
            }
        }
    }
}

impl Rule for Extensions {
    fn kinds(&self) -> &'static [NodeKind] {
        MODULE_KINDS
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let visitor = VisitorOptions {
            systems: ModuleSystems {
                esmodule: true,
                commonjs: true,
                amd: false,
            },
            ignore: Vec::new(),
        };
        for (r, value) in visitor.refs(cx.ast(), node) {
            self.check(cx, r.source, r.importer, value);
        }
    }
}
