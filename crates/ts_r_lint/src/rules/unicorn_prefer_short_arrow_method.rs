//! `unicorn/prefer-short-arrow-method`: an object literal method (not a getter, setter or
//! generator, not `__proto__`, no `this` parameter, and no `this`, `super`, `new.target`,
//! `arguments` or direct `eval()` anywhere inside) whose body is a single `return <expr>;`,
//! reported on the property. The fix writes `key: async (params): Ret => expr` (the body
//! wrapped like any concise arrow body); generic methods get no fix.
//!
//! With `"consistent-as-needed"`, an object is only reported when every method in it could
//! be fixed this way, and a comment in any of its methods drops the fix; with the default
//! `"always"`, a comment in the method itself drops it.

use super::prelude::*;
use super::unicorn_default_export_style::has_unsafe_arrow_reference;
use super::unicorn_no_useless_boolean_cast::concise_arrow_body_text;
use super::unicorn_text::{has_comment_in, paren_text};
use ts_r_ast::{FnFlags, MethodKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-short-arrow-method",
        "Prefer arrow function properties over methods with a single return.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<PreferShortArrowMethod>,
};

#[derive(Debug)]
struct PreferShortArrowMethod {
    consistent: bool,
}

impl FromOptions for PreferShortArrowMethod {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        match opts.first().and_then(Value::as_str) {
            None | Some("always") => Ok(Self { consistent: false }),
            Some("consistent-as-needed") => Ok(Self { consistent: true }),
            Some(other) => Err(format!("unknown mode `{other}`")),
        }
    }
}

/// A plain method (`key() {}`) of an object literal.
fn is_init_method(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    matches!(
        cx.node(id),
        Node::MethodDef {
            kind: MethodKind::Method,
            ..
        }
    ) && cx
        .parent(id)
        .is_some_and(|p| matches!(cx.node(p), Node::ObjectLit { .. }))
}

/// The returned expression of a method that can become an arrow property.
fn return_expression(cx: &Ctx<'_, '_>, method: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    if !is_init_method(cx, method) {
        return None;
    }
    let Node::MethodDef { key, value, .. } = *ast.node(method) else {
        return None;
    };
    let Node::FunctionExpr {
        flags,
        params,
        body: Some(body),
        ..
    } = *ast.node(value)
    else {
        return None;
    };
    let is_proto = match *ast.node(key) {
        Node::Ident { name } | Node::StringLit { value: name } => ast.atom(name) == "__proto__",
        _ => false,
    };
    if flags.contains(FnFlags::GENERATOR)
        || is_proto
        || ast.list(params).iter().any(|&p| {
            matches!(*ast.node(p), Node::Param { pattern, .. } if ident_name(ast, pattern) == Some("this"))
        })
        || has_unsafe_arrow_reference(ast, value)
    {
        return None;
    }
    let Node::Block { stmts } = *ast.node(body) else {
        return None;
    };
    match *ast.list(stmts) {
        [s] => match *ast.node(s) {
            Node::Return { arg } => arg,
            _ => None,
        },
        _ => None,
    }
}

fn replacement(cx: &Ctx<'_, '_>, method: NodeId, expr: NodeId) -> Option<String> {
    let ast = cx.ast();
    let Node::MethodDef { key, value, .. } = *ast.node(method) else {
        return None;
    };
    let Node::FunctionExpr {
        flags,
        type_params,
        params,
        return_type,
        ..
    } = *ast.node(value)
    else {
        return None;
    };
    if type_params.is_some() {
        return None;
    }
    let key_text = match *ast.node(key) {
        Node::ComputedKey { expr } => format!("[{}]", paren_text(cx, expr)),
        _ => cx.text(key).to_owned(),
    };
    let asyncness = if flags.contains(FnFlags::ASYNC) {
        "async "
    } else {
        ""
    };
    let params = ast
        .list(params)
        .iter()
        .map(|&p| cx.text(p))
        .collect::<Vec<_>>()
        .join(", ");
    let ret = match return_type {
        Some(t) => {
            // ESTree's return type annotation starts at its `:`.
            let src = cx.src();
            let start = cx.span(t).start as usize;
            let colon = src[..start].rfind(':').unwrap_or(start);
            &src[colon..cx.span(t).end as usize]
        }
        None => "",
    };
    let body = concise_arrow_body_text(cx, expr);
    Some(format!("{key_text}: {asyncness}({params}){ret} => {body}"))
}

impl Rule for PreferShortArrowMethod {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::MethodDef]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Some(expr) = return_expression(cx, node) else {
            return;
        };
        let mut affected = vec![node];
        if self.consistent {
            let Some(parent) = cx.parent(node) else {
                return;
            };
            let Node::ObjectLit { props } = *cx.node(parent) else {
                return;
            };
            affected = cx
                .ast()
                .list(props)
                .iter()
                .copied()
                .filter(|&p| is_init_method(cx, p))
                .collect();
            let all = affected.iter().all(|&p| {
                return_expression(cx, p).is_some_and(|e| replacement(cx, p, e).is_some())
            });
            if !all {
                return;
            }
        }
        let fix = replacement(cx, node, expr)
            .filter(|_| affected.iter().all(|&p| !has_comment_in(cx, cx.span(p))))
            .map(|text| Fix::replace(cx.span(node), text));
        cx.report_node(
            node,
            "Prefer an arrow function property over a method with a single return.",
        )
        .id("prefer-short-arrow-method")
        .fix_opt(fix);
    }
}
