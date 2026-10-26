//! `unicorn/no-async-iterator-callback`: `it.filter/forEach/some/every/find/flatMap(cb, ..)`
//! (the name may be computed when statically known; optional chaining allowed) on an
//! iterator expression, where `cb` (TS wrappers and parentheses skipped) is an `async`
//! non-generator function or arrow, a `const` initialized with one, or a never-written name
//! with exactly one implemented `async` function declaration. Reported at `cb`.
//!
//! The plugin's typed check (a callback whose signature returns a promise) is not used.

use super::prelude::*;
use super::unicorn_iter::{is_iterator_expression, unwrap_expression};
use super::unicorn_no_array_reduce::const_initializer;
use super::unicorn_static::static_value;
use crate::semantic::Resolution;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-async-iterator-callback",
        "Disallow asynchronous callbacks in synchronous iterator helpers.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoAsyncIteratorCallback>,
};

#[derive(Debug, Default)]
struct NoAsyncIteratorCallback;

const METHODS: &[&str] = &["filter", "forEach", "some", "every", "find", "flatMap"];

fn is_async_function(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::Arrow { flags, .. }
        | Node::FunctionExpr { flags, .. }
        | Node::FunctionDecl { flags, .. } => {
            flags.contains(FnFlags::ASYNC) && !flags.contains(FnFlags::GENERATOR)
        }
        _ => false,
    }
}

fn is_async_callback(cx: &Ctx<'_, '_>, callback: NodeId) -> bool {
    let ast = cx.ast();
    let node = unwrap_expression(ast, callback);
    if is_async_function(ast, node) {
        return true;
    }
    if const_initializer(cx, node)
        .is_some_and(|i| is_async_function(ast, unwrap_expression(ast, i)))
    {
        return true;
    }
    if !matches!(ast.node(node), Node::Ident { .. }) {
        return false;
    }
    let sem = cx.semantic();
    let Some(Resolution::Symbol(sym)) = sem.reference_at(node).map(|r| sem.reference(r).resolution)
    else {
        return false;
    };
    let symbol = sem.symbol(sym);
    if symbol.refs.iter().any(|&r| sem.reference(r).is_write()) {
        return false;
    }
    let implementations: Vec<NodeId> = symbol
        .decls
        .iter()
        .filter(|d| {
            d.kind == DeclKind::Function
                && matches!(*ast.node(d.node), Node::FunctionDecl { body: Some(_), .. })
        })
        .map(|d| d.node)
        .collect();
    matches!(*implementations, [f] if is_async_function(ast, f))
}

fn method_name(cx: &Ctx<'_, '_>, callee: NodeId) -> Option<String> {
    let ast = cx.ast();
    match *ast.node(callee) {
        Node::Member { prop, .. } => match ast.node(prop) {
            Node::Ident { .. } => ident_name(ast, prop).map(str::to_owned),
            _ => None,
        },
        Node::Index { index, .. } => static_value(cx, index)?.to_js_string(),
        _ => None,
    }
}

impl Rule for NoAsyncIteratorCallback {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call { callee, args, .. } = *ast.node(id) else {
            return;
        };
        let callee = skip_parens(ast, callee);
        let obj = match *ast.node(callee) {
            Node::Member { obj, .. } | Node::Index { obj, .. } => obj,
            _ => return,
        };
        let Some(&callback) = ast.list(args).first() else {
            return;
        };
        if matches!(ast.node(callback), Node::Spread { .. }) {
            return;
        }
        let Some(method) = method_name(cx, callee) else {
            return;
        };
        if !METHODS.contains(&method.as_str())
            || !is_iterator_expression(cx, obj)
            || !is_async_callback(cx, callback)
        {
            return;
        }
        cx.report(
            cx.span(skip_parens(ast, callback)),
            format!(
                "Do not pass an asynchronous callback to `Iterator#{method}()`; returned promises are not awaited."
            ),
        )
        .id("no-async-iterator-callback");
    }
}
