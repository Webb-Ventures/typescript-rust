//! `unicorn/prefer-top-level-await`: at the top level (no enclosing function or class),
//! a `.then`/`.catch`/`.finally` chain, an async IIFE, or a call of a single-definition
//! async function (a declaration or a `const` initialized with one). Skipped: `.cjs` and
//! `.svelte` files; calls whose value is used as the object of another promise method,
//! awaited, used as a variable initializer or assigned (`=`, `??=`, `||=`, `&&=`),
//! discarded with `void`, or passed in the array of `Promise.all`/`allSettled`/`any`/`race`
//! (through `?:` branches and `||`/`&&` operands); `.catch` on `Effect` and on schema
//! builders (`fooSchema.optional()...`, `z.string()...`).
//!
//! Reports: the method name, the function head of the IIFE, or the call. Calls of a known
//! async function get an `await` suggestion (around the call with its TS wrappers;
//! parenthesized below unary precedence and as the left side of `**`), except when the
//! call is the unparenthesized object of a member, call or tag.
//!
//! No type information is used, so `isKnownNonPromise` is always false here.

use super::prelude::*;
use super::unicorn_util::prev_token_end;
use ts_r_ast::{Ast, FnFlags};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-top-level-await",
        "Prefer top-level await over top-level promises and async function calls.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<PreferTopLevelAwait>,
};

#[derive(Debug, Default)]
struct PreferTopLevelAwait;

const PROMISE_METHODS: &[&str] = &["then", "catch", "finally"];

/// A TS expression wrapper whose expression is `child`.
fn is_ts_wrapper_of(ast: &Ast, parent: NodeId, child: NodeId) -> bool {
    match *ast.node(parent) {
        Node::As { expr, .. }
        | Node::Satisfies { expr, .. }
        | Node::NonNull { expr }
        | Node::TypeAssertion { expr, .. }
        | Node::Instantiation { expr, .. } => skip_parens(ast, expr) == child,
        _ => false,
    }
}

/// `node` with the TS wrappers around it (parentheses looked through).
fn outermost_wrapper(cx: &Ctx<'_, '_>, node: NodeId) -> NodeId {
    let ast = cx.ast();
    let mut cur = node;
    while let Some(p) = parent_skip_parens(ast, cx.file.parents, cur) {
        if !is_ts_wrapper_of(ast, p, cur) {
            break;
        }
        cur = p;
    }
    cur
}

fn parent_np(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    parent_skip_parens(cx.ast(), cx.file.parents, id)
}

fn is_top_level(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    !ancestors_inclusive(cx.file.parents, node).skip(1).any(|a| {
        matches!(
            cx.node(a).kind(),
            NodeKind::FunctionDecl
                | NodeKind::FunctionExpr
                | NodeKind::Arrow
                | NodeKind::ClassDecl
                | NodeKind::ClassExpr
        )
    })
}

/// `void call()...`, through member calls chained on the value and TS wrappers.
fn discarded_with_void(cx: &Ctx<'_, '_>, call: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = call;
    for _ in 0..100_000 {
        let Some(p) = parent_np(cx, cur) else {
            return false;
        };
        match *ast.node(p) {
            Node::Member { obj, .. } if skip_parens(ast, obj) == cur => {
                match parent_np(cx, p).map(|g| (g, ast.node(g))) {
                    Some((g, Node::Call { callee, .. })) if skip_parens(ast, *callee) == p => {
                        cur = g;
                    }
                    _ => return false,
                }
            }
            Node::Unary {
                op: UnaryOp::Void, ..
            } => return true,
            _ if is_ts_wrapper_of(ast, p, cur) => cur = p,
            _ => return false,
        }
    }
    false
}

/// The value is the object of `.then`/`.catch`/`.finally(...)`.
fn is_promise_method_object(cx: &Ctx<'_, '_>, call: NodeId) -> bool {
    let ast = cx.ast();
    let node = outermost_wrapper(cx, call);
    let Some(member) = parent_np(cx, node) else {
        return false;
    };
    let Node::Member { obj, prop, .. } = *ast.node(member) else {
        return false;
    };
    skip_parens(ast, obj) == node
        && ident_name(ast, prop).is_some_and(|p| PROMISE_METHODS.contains(&p))
        && parent_np(cx, member).is_some_and(|g| {
            matches!(*ast.node(g), Node::Call { callee, .. } if skip_parens(ast, callee) == member)
        })
}

fn is_used_value(cx: &Ctx<'_, '_>, call: NodeId) -> bool {
    let ast = cx.ast();
    let node = outermost_wrapper(cx, call);
    let Some(p) = parent_np(cx, node) else {
        return false;
    };
    match *ast.node(p) {
        Node::Await { arg } => skip_parens(ast, arg) == node,
        Node::VarDeclarator { init: Some(i), .. } => skip_parens(ast, i) == node,
        Node::Assign { op, right, .. } => {
            skip_parens(ast, right) == node
                && matches!(
                    op,
                    AssignOp::Assign
                        | AssignOp::CoalesceAssign
                        | AssignOp::OrAssign
                        | AssignOp::AndAssign
                )
        }
        _ => false,
    }
}

/// An element (through `?:` branches and `||`/`&&` operands) of the array passed to
/// `Promise.all/allSettled/any/race`.
fn in_promise_combinator(cx: &Ctx<'_, '_>, call: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = call;
    let array = loop {
        let Some(p) = parent_np(cx, cur) else {
            return false;
        };
        match *ast.node(p) {
            Node::Conditional { cons, alt, .. }
                if skip_parens(ast, cons) == cur || skip_parens(ast, alt) == cur =>
            {
                cur = p;
            }
            Node::Binary {
                left,
                op: op @ (BinaryOp::LogicalAnd | BinaryOp::LogicalOr),
                right,
            } if skip_parens(ast, right) == cur
                || (skip_parens(ast, left) == cur && op != BinaryOp::LogicalAnd) =>
            {
                cur = p;
            }
            Node::ArrayLit { .. } => break p,
            _ => return false,
        }
    };
    let Some(c) = parent_np(cx, array) else {
        return false;
    };
    let Node::Call { callee, args, .. } = *ast.node(c) else {
        return false;
    };
    let Node::Member { obj, prop, .. } = *ast.node(callee) else {
        return false;
    };
    ident_name(ast, obj) == Some("Promise")
        && ident_name(ast, prop).is_some_and(|m| matches!(m, "all" | "allSettled" | "any" | "race"))
        && matches!(ast.list(args), [a] if skip_parens(ast, *a) == array)
}

fn is_schema_identifier(ast: &Ast, id: NodeId) -> bool {
    ident_name(ast, id).is_some_and(|n| n == "schema" || n.ends_with("Schema"))
}

fn unsupported_schema_property(name: &str, called: bool) -> bool {
    matches!(name, "then" | "finally" | "parse" | "safeParse" | "spa")
        || name.ends_with("Async")
        || (called && name == "coerce")
}

/// A schema-builder chain (`fooSchema.optional()`, `z.string().nullable()`) whose
/// `.catch` is not a promise method.
fn is_schema_catch_object(ast: &Ast, node: NodeId) -> bool {
    let mut expr = skip_parens(ast, node);
    let (mut has_call, mut has_member, mut called) = (false, false, false);
    let mut has_uncalled = false;
    let mut called_after_uncalled = false;
    let mut methods = Vec::new();
    let mut uncalled = Vec::new();
    for _ in 0..100_000 {
        match *ast.node(expr) {
            Node::Call { callee, .. } => {
                has_call = true;
                called = true;
                expr = skip_parens(ast, callee);
            }
            Node::Member { obj, prop, .. } => {
                let Some(name) = ident_name(ast, prop) else {
                    return false;
                };
                if unsupported_schema_property(name, called) {
                    return false;
                }
                has_member = true;
                if !called {
                    has_uncalled = true;
                    uncalled.push(name);
                } else if has_uncalled {
                    called_after_uncalled = true;
                }
                methods.push(name);
                expr = skip_parens(ast, obj);
                called = false;
            }
            Node::Index { .. } => return false,
            _ => break,
        }
    }
    if is_schema_identifier(ast, expr)
        && !has_uncalled
        && methods.iter().all(|m| {
            matches!(
                *m,
                "catch" | "default" | "nullable" | "nullish" | "optional"
            )
        })
    {
        return true;
    }
    ident_name(ast, expr) == Some("z")
        && has_call
        && has_member
        && !called_after_uncalled
        && uncalled.iter().all(|n| *n == "coerce")
}

/// eslint-utils' function head: `=>` of an arrow; from the start (with `async`) to the
/// `(` of a function expression.
fn function_head(cx: &Ctx<'_, '_>, f: NodeId) -> Span {
    let src = cx.src();
    let span = cx.span(f);
    match *cx.node(f) {
        Node::Arrow { body, .. } => {
            let end = prev_token_end(cx, cx.span(body).start);
            if src[..end as usize].ends_with("=>") {
                Span::new(end - 2, end)
            } else {
                span
            }
        }
        Node::FunctionExpr {
            type_params, name, ..
        } => {
            let after = type_params
                .map(|t| cx.span(t).end)
                .or(name.map(|n| cx.span(n).end))
                .unwrap_or(span.start);
            let mut pos = after as usize;
            while pos < span.end as usize {
                pos = skip_trivia_forward(src, pos);
                if src.as_bytes().get(pos) == Some(&b'(') {
                    return Span::new(span.start, pos as u32);
                }
                pos += 1;
            }
            span
        }
        _ => span,
    }
}

fn is_async_function(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::FunctionDecl { flags, .. }
        | Node::FunctionExpr { flags, .. }
        | Node::Arrow { flags, .. } => {
            flags.contains(FnFlags::ASYNC) && !flags.contains(FnFlags::GENERATOR)
        }
        _ => false,
    }
}

/// `callee` names a single-definition async function declaration or `const` arrow/function.
fn is_async_function_name(cx: &Ctx<'_, '_>, callee: NodeId) -> bool {
    let ast = cx.ast();
    let sem = cx.semantic();
    let Some(r) = sem.reference_at(callee) else {
        return false;
    };
    let crate::semantic::Resolution::Symbol(sym) = sem.reference(r).resolution else {
        return false;
    };
    let symbol = sem.symbol(sym);
    let [decl] = symbol.decls.as_slice() else {
        return false;
    };
    let Some(p) = cx.parent(decl.name) else {
        return false;
    };
    match *ast.node(p) {
        Node::FunctionDecl { .. } => is_async_function(ast, p),
        Node::VarDeclarator {
            name,
            init: Some(init),
            ..
        } if name == decl.name => {
            let is_const = cx.parent(p).is_some_and(|d| {
                matches!(
                    ast.node(d),
                    Node::VarDecl {
                        kind: VarKind::Const,
                        ..
                    }
                )
            });
            is_const && is_async_function(ast, skip_parens(ast, init))
        }
        _ => false,
    }
}

impl Rule for PreferTopLevelAwait {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let path = cx.file.path.to_ascii_lowercase();
        if path.ends_with(".cjs") || path.ends_with(".svelte") {
            return;
        }
        let ast = cx.ast();
        if !is_top_level(cx, node)
            || discarded_with_void(cx, node)
            || is_promise_method_object(cx, node)
            || is_used_value(cx, node)
            || in_promise_combinator(cx, node)
        {
            return;
        }
        let Node::Call { callee, .. } = *ast.node(node) else {
            return;
        };
        let callee = skip_parens(ast, callee);
        if let Node::Member { obj, prop, .. } = *ast.node(callee)
            && let Some(method) = ident_name(ast, prop)
            && PROMISE_METHODS.contains(&method)
        {
            let obj = skip_parens(ast, obj);
            if method == "catch"
                && (ident_name(ast, obj) == Some("Effect") || is_schema_catch_object(ast, obj))
            {
                return;
            }
            cx.report_node(prop, "Prefer top-level await over using a promise chain.")
                .id("promise");
            return;
        }
        if matches!(
            ast.node(callee),
            Node::FunctionExpr { .. } | Node::Arrow { .. }
        ) && is_async_function(ast, callee)
        {
            let head = function_head(cx, callee);
            cx.report(head, "Prefer top-level await over an async IIFE.")
                .id("iife");
            return;
        }
        let Some(name) = ident_name(ast, callee) else {
            return;
        };
        if !is_async_function_name(cx, callee) {
            return;
        }
        let expression = outermost_wrapper(cx, node);
        let parenthesized = cx
            .parent(expression)
            .is_some_and(|p| matches!(cx.node(p), Node::Paren { .. }));
        let chain_object = !parenthesized
            && cx.parent(expression).is_some_and(|p| match *cx.node(p) {
                Node::Member { obj, .. } | Node::Index { obj, .. } => obj == expression,
                Node::Call { callee, .. } => callee == expression,
                Node::TaggedTemplate { tag, .. } => tag == expression,
                _ => false,
            });
        let message = format!("Prefer top-level await over an async function `{name}` call.");
        let fix = (!chain_object).then(|| {
            let text = cx.text(expression);
            let mut text = if precedence(ast, expression) < 16 {
                format!("await ({text})")
            } else {
                format!("await {text}")
            };
            let left_of_exp = !parenthesized
                && cx.parent(expression).is_some_and(|p| {
                    matches!(*cx.node(p), Node::Binary { op: BinaryOp::Exp, left, .. } if left == expression)
                });
            if left_of_exp {
                text = format!("({text})");
            }
            Fix::replace(cx.span(expression), text)
        });
        let report = cx.report_node(node, message).id("identifier");
        if let Some(f) = fix {
            report.suggest("add-await", "Insert `await`.", f);
        }
    }
}
