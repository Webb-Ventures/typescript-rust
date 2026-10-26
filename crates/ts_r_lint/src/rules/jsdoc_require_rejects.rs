//! `jsdoc/require-rejects`: a function that rejects a promise documents it with `@rejects`.
//!
//! Only the function's statements are searched (blocks, loops, labels, `if`, `switch` cases,
//! expression statements, `return` arguments and the `catch`/`finally` of a `try`; never the
//! `try` block). A function rejects when it reaches `Promise.reject(..)`, a call to a bare
//! `reject(..)`, a `new Promise(executor)` whose function executor's body rejects, or, when
//! async, a `throw`. A nested function is searched too (its `throw` counts when it or an
//! outer function is async), but the functions nested in it are not. Blocks with
//! `@abstract`, `@virtual`, `@type`, an `exemptedBy` tag or replaced docs are skipped.

use super::prelude::*;
use crate::jsdoc_util::{ContextOpts, exempted_by, has_exempting_tag, replaced_docs};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-rejects",
        "Requires that Promise rejections are documented with `@rejects` tags",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<RequireRejects>,
};

#[derive(Debug)]
struct RequireRejects(ContextOpts);

impl FromOptions for RequireRejects {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        ContextOpts::parse(opts).map(RequireRejects)
    }
}

const FUNCTION_TYPES: &[&str] = &[
    "ArrowFunctionExpression",
    "FunctionDeclaration",
    "FunctionExpression",
    "MethodDefinition",
];

const MAX_DEPTH: u32 = 400;

/// Where the search is: `inner` once inside the checked function (a function met there is a
/// nested one), `in_async` when a `throw` rejects.
#[derive(Clone, Copy)]
struct Scope {
    inner: bool,
    in_async: bool,
}

fn rejects(ast: &Ast, id: NodeId, s: Scope, depth: u32) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    let d = depth + 1;
    let id = skip_parens(ast, id);
    let fn_body = |flags: FnFlags, body: Option<NodeId>| {
        let is_async = flags.contains(FnFlags::ASYNC);
        let inner = if s.inner {
            Scope {
                inner: false,
                in_async: is_async || s.in_async,
            }
        } else {
            Scope {
                inner: true,
                in_async: is_async,
            }
        };
        body.is_some_and(|b| rejects(ast, b, inner, d))
    };
    let is_name = |n: NodeId, name: &str| ident_name(ast, skip_parens(ast, n)) == Some(name);
    match *ast.node(id) {
        Node::FunctionDecl { flags, body, .. } | Node::FunctionExpr { flags, body, .. } => {
            fn_body(flags, body)
        }
        Node::Arrow { flags, body, .. } => fn_body(flags, Some(body)),
        Node::Block { stmts } => ast.list(stmts).iter().any(|&n| rejects(ast, n, s, d)),
        Node::Call { callee, .. } => {
            let callee = skip_parens(ast, callee);
            match *ast.node(callee) {
                Node::Member { obj, prop, .. } => {
                    is_name(obj, "Promise") && is_name(prop, "reject")
                }
                Node::Ident { .. } => is_name(callee, "reject"),
                _ => false,
            }
        }
        Node::DoWhile { body, .. }
        | Node::ForIn { body, .. }
        | Node::ForOf { body, .. }
        | Node::For { body, .. }
        | Node::Labeled { body, .. }
        | Node::While { body, .. }
        | Node::With { body, .. } => rejects(ast, body, s, d),
        Node::ExprStmt { expr } => rejects(ast, expr, s, d),
        Node::If { cons, alt, .. } => {
            rejects(ast, cons, s, d) || alt.is_some_and(|a| rejects(ast, a, s, d))
        }
        Node::New { callee, args, .. } => {
            if !is_name(callee, "Promise") {
                return false;
            }
            let Some(&exec) = ast.list(args).first() else {
                return false;
            };
            let top = Scope {
                inner: false,
                in_async: false,
            };
            match *ast.node(skip_parens(ast, exec)) {
                Node::Arrow { body, .. } => rejects(ast, body, top, d),
                Node::FunctionExpr { body, .. } => body.is_some_and(|b| rejects(ast, b, top, d)),
                _ => false,
            }
        }
        Node::Return { arg } => arg.is_some_and(|a| rejects(ast, a, s, d)),
        Node::Switch { cases, .. } => ast.list(cases).iter().any(|&c| match *ast.node(c) {
            Node::SwitchCase { body, .. } => ast.list(body).iter().any(|&n| rejects(ast, n, s, d)),
            _ => false,
        }),
        Node::Throw { .. } => s.in_async,
        Node::Try {
            handler, finalizer, ..
        } => {
            let caught = handler.is_some_and(|h| match *ast.node(h) {
                Node::CatchClause { body, .. } => rejects(ast, body, s, d),
                _ => false,
            });
            caught || finalizer.is_some_and(|f| rejects(ast, f, s, d))
        }
        _ => false,
    }
}

fn is_async(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::FunctionDecl { flags, .. }
        | Node::FunctionExpr { flags, .. }
        | Node::Arrow { flags, .. } => flags.contains(FnFlags::ASYNC),
        _ => false,
    }
}

impl Rule for RequireRejects {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let settings = crate::jsdoc::Settings::from_cx(cx);
        let all = self.0.contexts(&settings).is_ok_and(|c| c.all);
        let exempted = exempted_by(&self.0.opts, settings.mode == "closure");
        self.0.each_doc(cx, |cx, finder, doc| {
            if doc.block.has_any_tag(&["abstract", "virtual", "type"])
                || replaced_docs(finder, doc)
                || has_exempting_tag(&doc.block, &exempted)
            {
                return;
            }
            let Some(name) = doc.preferred(cx, finder, "rejects") else {
                return;
            };
            if doc.block.has_tag(&name) {
                return;
            }
            let Some(t) = doc.target else {
                return;
            };
            let ty = finder.es_type(t);
            let iterating = !all || FUNCTION_TYPES.contains(&ty);
            let node = match t {
                crate::jsdoc::Target::Node(id) => id,
                crate::jsdoc::Target::Export(_) => return,
            };
            if !is_async(finder.ast, node) && !iterating {
                return;
            }
            let top = Scope {
                inner: false,
                in_async: false,
            };
            if rejects(finder.ast, node, top, 0) {
                cx.report(
                    doc.comment,
                    "Promise-rejecting function requires `@rejects` tag",
                );
            }
        });
    }
}
