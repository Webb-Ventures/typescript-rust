//! `unicorn/prefer-promise-try`: `new Promise(resolve => resolve(f()))` (the global `Promise`,
//! a non-async arrow with one plain parameter whose only expression calls it with one call
//! argument that doesn't use the parameter) is `Promise.try(() => f())`; so is
//! `Promise.resolve().then(callback)` (a function, member or name other than `undefined`),
//! which is only reported. The fix keeps `new Promise`'s type arguments; it is withheld for
//! typed executors, a `new Promise` inside another `new` callee, or comments.

use super::prelude::*;
use super::unicorn_ast::{declared_name, method_call};
use super::unicorn_no_useless_boolean_cast::concise_arrow_body_text;
use super::unicorn_text::{has_comment_in, paren_span};
use ts_r_ast::Ast;
use ts_r_ast::visit::{Visitor, Walk, walk};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-promise-try",
        "Prefer `Promise.try()` over promise-wrapping boilerplate.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferPromiseTry>,
};

#[derive(Debug, Default)]
struct PreferPromiseTry;

const MESSAGE: &str = "Prefer `Promise.try()` over promise-wrapping boilerplate.";

fn is_ts_wrapper(n: &Node) -> bool {
    matches!(
        n,
        Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
            | Node::Instantiation { .. }
    )
}

fn unwrap_ts(ast: &Ast, mut id: NodeId) -> NodeId {
    for _ in 0..10_000 {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
            | Node::Instantiation { expr, .. } => id = expr,
            _ => return id,
        }
    }
    id
}

fn is_global_promise(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    matches!(ast.node(id), Node::Ident { .. })
        && ident_name(ast, id) == Some("Promise")
        && cx.semantic().refers_to_global(id)
}

/// A name in the subtree under `root` that refers to the parameter `param`.
fn uses_param(cx: &Ctx<'_, '_>, root: NodeId, param: NodeId) -> bool {
    struct Find<'c, 'a, 'b> {
        cx: &'c Ctx<'a, 'b>,
        param: NodeId,
        found: bool,
    }
    impl Visitor for Find<'_, '_, '_> {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
            if matches!(ast.node(id), Node::Ident { .. })
                && declared_name(self.cx, id) == Some(self.param)
            {
                self.found = true;
                return Walk::Stop;
            }
            Walk::Continue
        }
    }
    let mut f = Find {
        cx,
        param,
        found: false,
    };
    walk(cx.ast(), root, &mut f);
    f.found
}

/// `node` (through member objects, parentheses and TS wrappers) is a `new` callee.
fn in_new_callee(cx: &Ctx<'_, '_>, mut node: NodeId) -> bool {
    let ast = cx.ast();
    for _ in 0..10_000 {
        let Some(p) = cx.parent(node) else {
            return false;
        };
        match *ast.node(p) {
            Node::Member { obj, .. } | Node::Index { obj, .. } if obj == node => node = p,
            Node::Paren { .. } => node = p,
            ref n if is_ts_wrapper(n) => node = p,
            Node::New { callee, .. } => return callee == node,
            _ => return false,
        }
    }
    false
}

impl PreferPromiseTry {
    fn check_new(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
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
        if !is_global_promise(cx, callee) {
            return;
        }
        let [executor] = *ast.list(args) else {
            return;
        };
        let executor = skip_parens(ast, executor);
        let Node::Arrow {
            flags,
            type_params,
            params,
            return_type,
            body,
        } = *ast.node(executor)
        else {
            return;
        };
        if flags.contains(FnFlags::ASYNC) || flags.contains(FnFlags::GENERATOR) {
            return;
        }
        let [param] = *ast.list(params) else {
            return;
        };
        let Node::Param {
            rest: false,
            pattern,
            type_ann,
            init: None,
            ..
        } = *ast.node(param)
        else {
            return;
        };
        if !matches!(ast.node(pattern), Node::Ident { .. }) {
            return;
        }
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
        let Node::Call {
            callee: resolve,
            optional: false,
            args: call_args,
            ..
        } = *ast.node(skip_parens(ast, only))
        else {
            return;
        };
        let resolve = skip_parens(ast, resolve);
        if !matches!(ast.node(resolve), Node::Ident { .. })
            || ident_name(ast, resolve) != ident_name(ast, pattern)
        {
            return;
        }
        let [value] = *ast.list(call_args) else {
            return;
        };
        if !matches!(ast.node(unwrap_ts(ast, value)), Node::Call { .. })
            || uses_param(cx, value, pattern)
        {
            return;
        }
        let range = paren_span(cx, node);
        let fix = (type_ann.is_none()
            && return_type.is_none()
            && type_params.is_none()
            && !in_new_callee(cx, node)
            && !has_comment_in(cx, range))
        .then(|| {
            let ta = type_args.map_or("", |t| cx.text(t));
            Fix::replace(
                range,
                format!(
                    "Promise.try{ta}(() => {})",
                    concise_arrow_body_text(cx, value)
                ),
            )
        });
        let span = cx.span(node);
        cx.report(span, MESSAGE).fix_opt(fix);
    }

    fn check_call(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(then) = method_call(ast, node) else {
            return;
        };
        if then.name != "then" || !then.plain() || then.args.len() != 1 {
            return;
        }
        let cb = then.args[0];
        let Some(resolve) = method_call(ast, skip_parens(ast, then.obj)) else {
            return;
        };
        if resolve.name != "resolve"
            || !resolve.plain()
            || !resolve.args.is_empty()
            || !matches!(ast.node(skip_parens(ast, resolve.obj)), Node::Ident { .. })
            || !is_global_promise(cx, resolve.obj)
        {
            return;
        }
        let cb = unwrap_ts(ast, cb);
        let candidate = match *ast.node(cb) {
            Node::Arrow { .. }
            | Node::FunctionExpr { .. }
            | Node::Member { .. }
            | Node::Index { .. } => true,
            Node::Ident { .. } => ident_name(ast, cb) != Some("undefined"),
            _ => false,
        };
        if candidate {
            let span = cx.span(node);
            cx.report(span, MESSAGE);
        }
    }
}

impl Rule for PreferPromiseTry {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::New, NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match cx.node(node) {
            Node::New { .. } => self.check_new(node, cx),
            _ => self.check_call(node, cx),
        }
    }
}
