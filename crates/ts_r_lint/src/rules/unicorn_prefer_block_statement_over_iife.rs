//! `unicorn/prefer-block-statement-over-iife`: an expression statement that only calls an
//! anonymous, non-async, non-generator function or arrow with no parameters, no arguments
//! and a block body is a block statement in disguise. Kept when the body has a directive, a
//! `return`, a `var` or a direct `eval` outside nested functions, a function declaration
//! (scripts only: there block-level functions are not block scoped), or, for a `function`,
//! `this` / `new.target` / `arguments` outside nested non-arrow functions. The fix replaces
//! the statement with the body; it is withheld when a comment outside the body would go.

use super::prelude::*;
use super::unicorn_text::would_remove_comments;
use crate::env::SourceType;
use ts_r_ast::Ast;
use ts_r_ast::visit::{Visitor, Walk, walk};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-block-statement-over-iife",
        "Prefer block statements over IIFEs used only for scoping.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferBlockStatementOverIife>,
};

#[derive(Debug, Default)]
struct PreferBlockStatementOverIife;

/// Some node under `root` satisfies `pred`, not looking inside nodes (other than `root`)
/// that satisfy `skip` (they are still tested with `pred`).
fn contains(
    ast: &Ast,
    root: NodeId,
    pred: impl Fn(&Ast, NodeId) -> bool,
    skip: impl Fn(&Node) -> bool,
) -> bool {
    struct Find<P, S> {
        root: NodeId,
        pred: P,
        skip: S,
        found: bool,
    }
    impl<P: Fn(&Ast, NodeId) -> bool, S: Fn(&Node) -> bool> Visitor for Find<P, S> {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
            if (self.pred)(ast, id) {
                self.found = true;
                return Walk::Stop;
            }
            if id != self.root && (self.skip)(ast.node(id)) {
                return Walk::SkipChildren;
            }
            Walk::Continue
        }
    }
    let mut f = Find {
        root,
        pred,
        skip,
        found: false,
    };
    walk(ast, root, &mut f);
    f.found
}

fn is_function(n: &Node) -> bool {
    matches!(
        n,
        Node::FunctionExpr { .. } | Node::FunctionDecl { .. } | Node::Arrow { .. }
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

impl Rule for PreferBlockStatementOverIife {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ExprStmt]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::ExprStmt { expr } = *cx.node(node) else {
            return;
        };
        let call = skip_parens(ast, expr);
        let Node::Call {
            callee,
            optional: false,
            args,
            ..
        } = *ast.node(call)
        else {
            return;
        };
        if !ast.list(args).is_empty() {
            return;
        }
        let callee = skip_parens(ast, callee);
        let (flags, params, body, is_fn) = match *ast.node(callee) {
            Node::Arrow {
                flags,
                params,
                body,
                ..
            } => (flags, params, Some(body), false),
            Node::FunctionExpr {
                flags,
                name: None,
                params,
                body,
                ..
            } => (flags, params, body, true),
            _ => return,
        };
        let Some(body) = body else {
            return;
        };
        let Node::Block { stmts } = *ast.node(body) else {
            return;
        };
        if flags.contains(FnFlags::ASYNC)
            || flags.contains(FnFlags::GENERATOR)
            || !ast.list(params).is_empty()
            || ast
                .list(stmts)
                .iter()
                .any(|&s| matches!(ast.node(s), Node::Directive { .. }))
        {
            return;
        }
        let function_only = |ast: &Ast, id: NodeId| match *ast.node(id) {
            Node::Return { .. } => true,
            Node::VarDecl {
                kind: VarKind::Var, ..
            } => true,
            Node::Call {
                callee,
                optional: false,
                ..
            } => {
                let c = unwrap_ts(ast, callee);
                matches!(ast.node(c), Node::Ident { .. }) && ident_name(ast, c) == Some("eval")
            }
            _ => false,
        };
        if contains(ast, body, function_only, is_function) {
            return;
        }
        let env = cx.env();
        if matches!(env.source_type, SourceType::Script | SourceType::CommonJs)
            && contains(
                ast,
                body,
                |ast, id| matches!(ast.node(id), Node::FunctionDecl { .. }),
                is_function,
            )
        {
            return;
        }
        if is_fn {
            let context_ref = |ast: &Ast, id: NodeId| match *ast.node(id) {
                Node::This {} => true,
                Node::MetaProperty {
                    kind: ts_r_ast::MetaPropKind::NewTarget,
                } => true,
                Node::Ident { .. } => ident_name(ast, id) == Some("arguments"),
                _ => false,
            };
            if contains(ast, body, context_ref, |n| {
                is_function(n) && !matches!(n, Node::Arrow { .. })
            }) {
                return;
            }
        }
        let stmt_span = cx.span(node);
        let body_span = cx.span(body);
        let fix = (!would_remove_comments(cx, stmt_span, &[body_span]))
            .then(|| Fix::replace(stmt_span, body_span.text(cx.src()).to_owned()));
        cx.report(
            cx.span(call),
            "Prefer a block statement over an IIFE used only for scoping.",
        )
        .fix_opt(fix);
    }
}
