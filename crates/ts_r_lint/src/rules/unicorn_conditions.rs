//! Helpers for the round-6 eslint-plugin-unicorn control-flow and condition rules: negated
//! condition text, subtree searches that stop at function and loop boundaries, and edits
//! that splice a removal fix into a larger one.

#![allow(dead_code)] // A shared toolkit: each rule uses part of it.

use super::prelude::*;
use super::unicorn_text::expression_text;
use ts_r_ast::Ast;
use ts_r_ast::visit::{Visitor, Walk, walk};

/// `shouldAddParenthesesToUnaryExpressionArgument`: `expr` needs parentheses as the operand
/// of a prefix unary operator (looser precedence, or an update, `x!` or `<T>x`, which read
/// ambiguously there).
pub(crate) fn unary_argument_needs_parens(ast: &Ast, expr: NodeId) -> bool {
    precedence(ast, expr) < 16
        || matches!(
            ast.node(expr),
            Node::Update { .. } | Node::NonNull { .. } | Node::TypeAssertion { .. }
        )
}

/// `getNegatedExpressionText`: `!expr`, with parentheses when needed; redundant parentheses
/// around `expr` are dropped unless they hold comments.
pub(crate) fn negated_expression_text(cx: &Ctx<'_, '_>, expr: NodeId) -> String {
    let expr = skip_parens(cx.ast(), expr);
    format!(
        "!{}",
        expression_text(cx, expr, unary_argument_needs_parens(cx.ast(), expr))
    )
}

/// A loop statement.
pub(crate) fn is_loop(node: &Node) -> bool {
    matches!(
        node,
        Node::For { .. }
            | Node::ForIn { .. }
            | Node::ForOf { .. }
            | Node::While { .. }
            | Node::DoWhile { .. }
    )
}

/// A function of any kind.
pub(crate) fn is_function_node(node: &Node) -> bool {
    is_function(node.kind())
}

/// An unlabeled `break` under `root` (inclusive) that would leave the enclosing loop: none
/// inside a function, loop or `switch` (`root` itself included) counts.
pub(crate) fn has_unlabeled_break(ast: &Ast, root: NodeId) -> bool {
    struct Find {
        found: bool,
    }
    impl Visitor for Find {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
            let node = ast.node(id);
            if matches!(node, Node::Break { label: None }) {
                self.found = true;
                return Walk::Stop;
            }
            if is_function_node(node) || is_loop(node) || matches!(node, Node::Switch { .. }) {
                return Walk::SkipChildren;
            }
            Walk::Continue
        }
    }
    let mut f = Find { found: false };
    walk(ast, root, &mut f);
    f.found
}

/// The identifiers under `root` (inclusive) in source order, not entering type nodes.
pub(crate) fn idents_in(ast: &Ast, root: NodeId) -> Vec<NodeId> {
    struct Collect {
        out: Vec<NodeId>,
    }
    impl Visitor for Collect {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
            if matches!(ast.node(id), Node::Ident { .. }) {
                self.out.push(id);
            }
            Walk::Continue
        }
    }
    let mut c = Collect { out: Vec::new() };
    walk(ast, root, &mut c);
    c.out
}

/// `fix` followed by the edits of `more`.
pub(crate) fn join_fixes(mut fix: Fix, more: Fix) -> Fix {
    for e in more.edits {
        fix = fix.and(e.span, e.text);
    }
    fix
}

/// `isGlobalNameAvailable`: the global `name` is not shadowed by a value binding at `at`
/// (a type-only declaration such as `type Boolean = ...` doesn't hide it).
pub(crate) fn global_value_available(cx: &Ctx<'_, '_>, at: NodeId, name: &str) -> bool {
    let sem = cx.semantic();
    match sem.lookup(sem.enclosing_scope(at), name) {
        Some(s) => {
            let sym = sem.symbol(s);
            sym.flags.contains(SymbolFlags::COMMENT_GLOBAL) || !sym.is_value()
        }
        None => true,
    }
}
