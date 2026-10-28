//! `unicorn/catch-error-name`: the binding of a `catch` clause, and the first parameter of
//! the rejection handler passed to `.catch(fn)` / `.then(x, fn)`, must be named `error` (the
//! `name` option). Names ending in the expected name (or its capitalised form), names
//! matching an `ignore` pattern, and those names followed by `_`s are allowed; an unused `_`
//! is left alone. The fix renames the binding everywhere, adding `_`s until the name is free
//! where the binding and its references live.

use super::prelude::*;
use super::unicorn_util::{available_name, estree_ident_span, is_method_call, rename_symbol_fix};
use crate::regex;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/catch-error-name",
        "Enforce a specific parameter name in catch clauses.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<CatchErrorName>,
};

#[derive(Debug)]
struct Pattern {
    source: String,
    ignore_case: bool,
}

#[derive(Debug)]
struct CatchErrorName {
    name: String,
    ignore: Vec<Pattern>,
}

impl FromOptions for CatchErrorName {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mut name = "error".to_owned();
        let mut ignore = Vec::new();
        if let Some(o) = opts.first().and_then(Value::as_object) {
            if let Some(n) = o.get("name") {
                name = n.as_str().ok_or("`name` must be a string")?.to_owned();
            }
            if let Some(list) = o.get("ignore") {
                let list = list.as_array().ok_or("`ignore` must be an array")?;
                for p in list {
                    match p {
                        Value::String(s) => ignore.push(Pattern {
                            source: s.clone(),
                            ignore_case: false,
                        }),
                        // A RegExp, as the oracle data and config loader write it.
                        Value::Object(m) => {
                            if let Some(src) = m.get("regex").and_then(Value::as_str) {
                                let flags = m.get("flags").and_then(Value::as_str).unwrap_or("");
                                ignore.push(Pattern {
                                    source: src.to_owned(),
                                    ignore_case: flags.contains('i'),
                                });
                            }
                        }
                        _ => {
                            return Err(
                                "`ignore` items must be strings or regular expressions".into()
                            );
                        }
                    }
                }
            }
        }
        Ok(CatchErrorName { name, ignore })
    }
}

fn upper_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

impl CatchErrorName {
    fn allowed(&self, n: &str) -> bool {
        n == self.name
            || self
                .ignore
                .iter()
                .any(|p| regex::compile(&p.source).is_some_and(|r| r.is_match(n, p.ignore_case)))
            || n.ends_with(&self.name)
            || n.ends_with(&upper_first(&self.name))
    }

    fn check(&self, cx: &mut Ctx<'_, '_>, ident: NodeId, body: NodeId) {
        let ast = cx.ast();
        let Some(original) = ident_name(ast, ident) else {
            return;
        };
        if self.allowed(original) || self.allowed(original.trim_end_matches('_')) {
            return;
        }
        let sem = cx.semantic();
        let Some(sym) = sem.symbol_of_decl(ident) else {
            return;
        };
        let symbol = sem.symbol(sym);
        if original == "_" && symbol.refs.is_empty() {
            return;
        }
        let mut scopes = vec![symbol.scope];
        scopes.extend(symbol.refs.iter().map(|&r| sem.reference(r).scope));
        if matches!(ast.node(body), Node::Block { .. }) {
            scopes.push(
                sem.scope_of_node(body)
                    .unwrap_or_else(|| sem.enclosing_scope(body)),
            );
        }
        let fixed = available_name(cx, &self.name, &scopes, &|_| true);
        let message = format!(
            "The catch parameter `{original}` should be named `{}`.",
            fixed.as_deref().unwrap_or(&self.name)
        );
        let fix = fixed.map(|f| rename_symbol_fix(cx, sym, &f));
        let span = estree_ident_span(cx, ident);
        cx.report(span, message).id("catch-error-name").fix_opt(fix);
    }
}

impl Rule for CatchErrorName {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::CatchClause, NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::CatchClause {
                param: Some(p),
                body,
                ..
            } if matches!(ast.node(p), Node::Ident { .. }) => self.check(cx, p, body),
            Node::Call { args, .. } => {
                if !is_method_call(ast, node, &["then"], Some(2), Some(false))
                    && !is_method_call(ast, node, &["catch"], Some(1), Some(false))
                {
                    return;
                }
                let Some(&last) = ast.list(args).last() else {
                    return;
                };
                let f = skip_parens(ast, last);
                let (params, body) = match *ast.node(f) {
                    Node::Arrow { params, body, .. } => (params, body),
                    Node::FunctionExpr {
                        params,
                        body: Some(body),
                        ..
                    } => (params, body),
                    _ => return,
                };
                let Some(&first) = ast.list(params).first() else {
                    return;
                };
                let Node::Param {
                    pattern,
                    rest: false,
                    init: None,
                    ..
                } = *ast.node(first)
                else {
                    return;
                };
                if matches!(ast.node(pattern), Node::Ident { .. }) {
                    self.check(cx, pattern, body);
                }
            }
            _ => {}
        }
    }
}
