//! `unicorn/prefer-promise-static-methods`: `new Promise(resolve => resolve(v))` (the global
//! `Promise`, one argument: a non-async, non-generator function or arrow with one or two
//! distinct plain parameters whose only expression calls the first or second parameter
//! with at most one simple argument: a name other than the parameters, the function's own
//! name or a function's `arguments`, a literal or a substitution-free template) is
//! `Promise.resolve(v)` / `Promise.reject(v)`. The fix is withheld for typed parameters,
//! return types or type parameters, type arguments on a `resolve`, or lost comments.

use super::prelude::*;
use super::unicorn_text::would_remove_comments;
use super::unicorn_util::in_unparenthesized_new_callee;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-promise-static-methods",
        "Prefer `Promise.resolve()` and `Promise.reject()` over trivial `new Promise()` calls.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferPromiseStaticMethods>,
};

#[derive(Debug, Default)]
struct PreferPromiseStaticMethods;

impl Rule for PreferPromiseStaticMethods {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::New {
            callee,
            type_args,
            args,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        let callee_inner = skip_parens(ast, callee);
        if ident_name(ast, callee_inner) != Some("Promise")
            || !cx.semantic().refers_to_global(callee_inner)
        {
            return;
        }
        let [executor_outer] = *ast.list(args) else {
            return;
        };
        let executor = skip_parens(ast, executor_outer);
        let (flags, fn_name, params, return_type, type_params, body, is_fn) =
            match *ast.node(executor) {
                Node::Arrow {
                    flags,
                    type_params,
                    params,
                    return_type,
                    body,
                } => (
                    flags,
                    None,
                    params,
                    return_type,
                    type_params,
                    Some(body),
                    false,
                ),
                Node::FunctionExpr {
                    flags,
                    name,
                    type_params,
                    params,
                    return_type,
                    body,
                    ..
                } => (flags, name, params, return_type, type_params, body, true),
                _ => return,
            };
        if flags.contains(FnFlags::ASYNC) || flags.contains(FnFlags::GENERATOR) {
            return;
        }
        let params = ast.list(params);
        if params.is_empty() || params.len() > 2 {
            return;
        }
        let mut names = Vec::new();
        let mut typed = false;
        for &p in params {
            let Node::Param {
                rest: false,
                pattern,
                type_ann,
                init: None,
                ..
            } = *ast.node(p)
            else {
                return;
            };
            let Some(n) = ident_name(ast, pattern)
                .filter(|_| matches!(ast.node(pattern), Node::Ident { .. }))
            else {
                return;
            };
            if n == "this" {
                return;
            }
            typed |= type_ann.is_some();
            names.push(n);
        }
        if names.len() == 2 && names[0] == names[1] {
            return;
        }
        let Some(body) = body else {
            return;
        };
        let only = match *ast.node(body) {
            Node::Block { stmts } => match *ast.list(stmts) {
                [s] => match *ast.node(s) {
                    Node::ExprStmt { expr } => expr,
                    _ => return,
                },
                _ => return,
            },
            _ => body,
        };
        let only = skip_parens(ast, only);
        let Node::Call {
            callee: inner_callee,
            optional: false,
            args: call_args,
            ..
        } = *ast.node(only)
        else {
            return;
        };
        let call_args = ast.list(call_args);
        if call_args.len() > 1
            || call_args
                .iter()
                .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
        {
            return;
        }
        let inner_callee = skip_parens(ast, inner_callee);
        if !matches!(ast.node(inner_callee), Node::Ident { .. }) {
            return;
        }
        let called = ident_name(ast, inner_callee);
        let method = if called == Some(names[0]) {
            "resolve"
        } else if names.len() == 2 && called == Some(names[1]) {
            "reject"
        } else {
            return;
        };
        let value = call_args.first().map(|&v| skip_parens(ast, v));
        if let Some(v) = value {
            let simple = match *ast.node(v) {
                Node::Ident { .. } => {
                    let n = ident_name(ast, v).unwrap_or("");
                    !((n == "arguments" && is_fn)
                        || fn_name.and_then(|f| ident_name(ast, f)) == Some(n)
                        || names.contains(&n))
                }
                Node::StringLit { .. }
                | Node::NumberLit { .. }
                | Node::BigIntLit { .. }
                | Node::BoolLit { .. }
                | Node::NullLit {}
                | Node::RegexLit { .. } => true,
                Node::TemplateLit { parts } => ast.list(parts).len() == 1,
                _ => false,
            };
            if !simple {
                return;
            }
        }
        let span = cx.span(node);
        let blocked = typed
            || return_type.is_some()
            || type_params.is_some()
            || (method == "resolve" && type_args.is_some())
            || would_remove_comments(
                cx,
                span,
                &value.map(|v| cx.span(v)).into_iter().collect::<Vec<_>>(),
            );
        let fix = (!blocked).then(|| {
            let ta = type_args.map_or("", |t| cx.text(t));
            let vt = value.map_or("", |v| cx.text(v));
            let mut text = format!("Promise.{method}{ta}({vt})");
            if in_unparenthesized_new_callee(cx, node) {
                text = format!("({text})");
            }
            Fix::replace(span, text)
        });
        let message = if method == "resolve" {
            "Prefer `Promise.resolve()` over a trivial `new Promise()`."
        } else {
            "Prefer `Promise.reject()` over a trivial `new Promise()`."
        };
        cx.report(span, message).id(method).fix_opt(fix);
    }
}
