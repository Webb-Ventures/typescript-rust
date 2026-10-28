//! `unicorn/import-style`: per-module allowed import styles (`unassigned`, `default`,
//! `namespace`, `named`); defaults `chalk` and `path` default-only, `util` named-only.
//! Imports, dynamic imports (`await import()` assigned to a variable is judged by its
//! pattern), `export ... from` (opt-in) and `require()` (where a `default` allowance also
//! accepts `x = require()`) are checked against the module name with any `node:` prefix
//! dropped. A module whose four styles are all `false` gets the "banned" message instead.

use super::prelude::*;
use super::unicorn_names::disjunction;
use super::unicorn_static::{Static, static_value};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/import-style",
        "Enforce specific import styles per module.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<ImportStyle>,
};

const UNASSIGNED: u8 = 1;
const DEFAULT: u8 = 2;
const NAMESPACE: u8 = 4;
const NAMED: u8 = 8;

#[derive(Debug)]
struct ModuleStyles {
    /// Allowed styles, in option order (names for the message).
    allowed: Vec<&'static str>,
    mask: u8,
    banned: bool,
}

#[derive(Debug)]
struct ImportStyle {
    styles: Vec<(String, ModuleStyles)>,
    check_import: bool,
    check_dynamic: bool,
    check_export: bool,
    check_require: bool,
}

fn style_bit(s: &str) -> Option<(&'static str, u8)> {
    Some(match s {
        "unassigned" => ("unassigned", UNASSIGNED),
        "default" => ("default", DEFAULT),
        "namespace" => ("namespace", NAMESPACE),
        "named" => ("named", NAMED),
        _ => return None,
    })
}

impl FromOptions for ImportStyle {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let flag = |k: &str, d: bool| o.get(k).and_then(Value::as_bool).unwrap_or(d);
        let user = o.get("styles").and_then(Value::as_object);
        let defaults: [(&str, &str); 3] =
            [("chalk", "default"), ("path", "default"), ("util", "named")];
        // Module -> ordered (style, allowed) entries.
        let mut merged: Vec<(String, Vec<(String, Value)>)> = Vec::new();
        let extend = flag("extendDefaultStyles", true);
        let mut names: Vec<String> = Vec::new();
        if extend {
            names.extend(defaults.iter().map(|(m, _)| (*m).to_owned()));
        }
        if let Some(u) = user {
            for k in u.keys() {
                if !names.contains(k) {
                    names.push(k.clone());
                }
            }
        }
        for name in names {
            let given = user.and_then(|u| u.get(&name));
            let mut entries: Vec<(String, Value)> = Vec::new();
            if given == Some(&Value::Bool(false)) {
                merged.push((name, entries));
                continue;
            }
            if extend && let Some((_, s)) = defaults.iter().find(|(m, _)| *m == name) {
                entries.push(((*s).to_owned(), Value::Bool(true)));
            }
            if let Some(m) = given.and_then(Value::as_object) {
                for (k, v) in m {
                    match entries.iter_mut().find(|(e, _)| e == k) {
                        Some(e) => e.1 = v.clone(),
                        None => entries.push((k.clone(), v.clone())),
                    }
                }
            }
            merged.push((name, entries));
        }
        let styles = merged
            .into_iter()
            .map(|(name, entries)| {
                let banned = ["unassigned", "default", "namespace", "named"]
                    .iter()
                    .all(|s| {
                        entries
                            .iter()
                            .any(|(k, v)| k == s && *v == Value::Bool(false))
                    });
                let mut allowed = Vec::new();
                let mut mask = 0;
                for (k, v) in &entries {
                    let truthy = match v {
                        Value::Bool(b) => *b,
                        Value::Null => false,
                        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
                        Value::String(s) => !s.is_empty(),
                        _ => true,
                    };
                    if truthy && let Some((n, bit)) = style_bit(k) {
                        allowed.push(n);
                        mask |= bit;
                    }
                }
                (
                    name,
                    ModuleStyles {
                        allowed,
                        mask,
                        banned,
                    },
                )
            })
            .collect();
        Ok(ImportStyle {
            styles,
            check_import: flag("checkImport", true),
            check_dynamic: flag("checkDynamicImport", true),
            check_export: flag("checkExportFrom", false),
            check_require: flag("checkRequire", true),
        })
    }
}

/// The styles a binding pattern (`x`, `[a]`, `{a, default: b, ...c}`) takes from a module.
fn pattern_styles(ast: &ts_r_ast::Ast, pat: NodeId) -> u8 {
    match *ast.node(pat) {
        Node::Ident { .. } | Node::ArrayPattern { .. } => NAMESPACE,
        Node::ObjectPattern { props } => {
            let props = ast.list(props);
            if props.is_empty() {
                return UNASSIGNED;
            }
            let mut s = 0;
            for &p in props {
                match *ast.node(p) {
                    Node::Rest { .. } => s |= NAMED,
                    Node::BindingProperty { key, .. } => {
                        if let Node::Ident { name } = *ast.node(key) {
                            s |= if ast.atom(name) == "default" {
                                DEFAULT
                            } else {
                                NAMED
                            };
                        }
                    }
                    _ => {}
                }
            }
            s
        }
        _ => 0,
    }
}

impl ImportStyle {
    fn module_name(cx: &Ctx<'_, '_>, id: NodeId) -> Option<String> {
        match static_value(cx, id)? {
            Static::Str(s) => Some(s),
            _ => None,
        }
    }

    fn check(&self, cx: &mut Ctx<'_, '_>, span: Span, module: &str, actual: u8, is_require: bool) {
        let key = module.strip_prefix("node:").unwrap_or(module);
        let Some((_, st)) = self.styles.iter().find(|(n, _)| n == key) else {
            return;
        };
        if st.mask == 0 {
            if st.banned {
                cx.report(
                    span,
                    format!(
                        "All import styles are disabled for module `{module}`. Use the `no-restricted-imports` rule to disallow a module."
                    ),
                )
                .id("importStyleBanned");
            }
            return;
        }
        let mut effective = st.mask;
        if is_require && st.mask & DEFAULT != 0 && st.mask & NAMESPACE == 0 {
            effective |= NAMESPACE;
        }
        if actual & !effective == 0 {
            return;
        }
        let names: Vec<String> = st.allowed.iter().map(|s| (*s).to_owned()).collect();
        cx.report(
            span,
            format!("Use {} import for module `{module}`.", disjunction(&names)),
        )
        .id("importStyle");
    }
}

/// `node` without the parentheses around it, and its parent as ESTree sees it.
fn estree_parent(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    parent_skip_parens(cx.ast(), cx.file.parents, id)
}

impl Rule for ImportStyle {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::ImportDecl,
            NodeKind::ImportCall,
            NodeKind::VarDeclarator,
            NodeKind::ExportAll,
            NodeKind::ExportNamed,
            NodeKind::Call,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::ImportDecl {
                type_only,
                specifiers,
                source,
                ..
            } if self.check_import => {
                let Some(module) = Self::module_name(cx, source) else {
                    return;
                };
                let mut actual = 0;
                if !type_only {
                    let specs = ast.list(specifiers);
                    if specs.is_empty() {
                        actual = UNASSIGNED;
                    }
                    for &s in specs {
                        match *ast.node(s) {
                            Node::ImportDefaultSpecifier { .. } => actual |= DEFAULT,
                            Node::ImportNamespaceSpecifier { .. } => actual |= NAMESPACE,
                            Node::ImportSpecifier {
                                type_only: false,
                                imported,
                                local,
                            } => {
                                let name = imported.unwrap_or(local);
                                actual |= if matches!(*ast.node(name), Node::Ident { name } if ast.atom(name) == "default")
                                {
                                    DEFAULT
                                } else {
                                    NAMED
                                };
                            }
                            _ => {}
                        }
                    }
                }
                self.check(cx, cx.span(node), &module, actual, false);
            }
            Node::ImportCall { arg, .. } if self.check_dynamic => {
                // `const x = await import(..)` is judged by its declarator.
                if let Some(p) = estree_parent(cx, node)
                    && matches!(cx.node(p), Node::Await { .. })
                    && let Some(d) = estree_parent(cx, p)
                    && matches!(*cx.node(d), Node::VarDeclarator { init: Some(i), .. } if skip_parens(ast, i) == p)
                {
                    return;
                }
                let Some(module) = Self::module_name(cx, arg) else {
                    return;
                };
                self.check(cx, cx.span(node), &module, UNASSIGNED, false);
            }
            Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } => {
                let init = skip_parens(ast, init);
                match *ast.node(init) {
                    Node::Await { arg } if self.check_dynamic => {
                        let Node::ImportCall { arg: source, .. } = *ast.node(skip_parens(ast, arg))
                        else {
                            return;
                        };
                        let Some(module) = Self::module_name(cx, source) else {
                            return;
                        };
                        let actual = pattern_styles(ast, name);
                        self.check(cx, cx.span(node), &module, actual, false);
                    }
                    Node::Call { callee, args, .. } if self.check_require => {
                        if ident_name(ast, callee) != Some("require") {
                            return;
                        }
                        let Some(&first) = ast.list(args).first() else {
                            return;
                        };
                        let Some(module) = Self::module_name(cx, first) else {
                            return;
                        };
                        let actual = pattern_styles(ast, name);
                        self.check(cx, cx.span(node), &module, actual, true);
                    }
                    _ => {}
                }
            }
            Node::ExportAll { source, .. } if self.check_export => {
                let Some(module) = Self::module_name(cx, source) else {
                    return;
                };
                self.check(cx, cx.span(node), &module, NAMESPACE, false);
            }
            Node::ExportNamed {
                specifiers,
                source: Some(source),
                ..
            } if self.check_export => {
                let Some(module) = Self::module_name(cx, source) else {
                    return;
                };
                let specs = ast.list(specifiers);
                let mut actual = if specs.is_empty() { UNASSIGNED } else { 0 };
                for &s in specs {
                    if let Node::ExportSpecifier {
                        local, exported, ..
                    } = *ast.node(s)
                    {
                        let name = exported.unwrap_or(local);
                        actual |= if matches!(*ast.node(name), Node::Ident { name } if ast.atom(name) == "default")
                        {
                            DEFAULT
                        } else {
                            NAMED
                        };
                    }
                }
                self.check(cx, cx.span(node), &module, actual, false);
            }
            Node::Call {
                callee,
                optional: false,
                args,
                ..
            } if self.check_require => {
                if ident_name(ast, callee) != Some("require") {
                    return;
                }
                let args = ast.list(args);
                if args.len() != 1 || matches!(ast.node(args[0]), Node::Spread { .. }) {
                    return;
                }
                if !estree_parent(cx, node)
                    .is_some_and(|p| matches!(cx.node(p), Node::ExprStmt { .. }))
                {
                    return;
                }
                let Some(module) = Self::module_name(cx, args[0]) else {
                    return;
                };
                self.check(cx, cx.span(node), &module, UNASSIGNED, true);
            }
            _ => {}
        }
    }
}
