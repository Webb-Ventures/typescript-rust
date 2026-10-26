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

/// `id` without parentheses and TS expression wrappers.
pub(crate) fn unwrap_ts_and_parens(ast: &Ast, mut id: NodeId) -> NodeId {
    for _ in 0..100_000 {
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

/// An optional link in the chain `id` ends (TS wrappers and parentheses looked through).
pub(crate) fn has_optional_chain_element(ast: &Ast, mut id: NodeId) -> bool {
    for _ in 0..100_000 {
        id = unwrap_ts_and_parens(ast, id);
        match *ast.node(id) {
            Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                if optional {
                    return true;
                }
                id = obj;
            }
            Node::Call {
                callee, optional, ..
            } => {
                if optional {
                    return true;
                }
                id = callee;
            }
            _ => return false,
        }
    }
    false
}

fn is_line_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// `reindentText`: every line after the first that starts with `from` gets `to` in its
/// place; blank lines become empty; other lines and the line breaks are kept.
pub(crate) fn reindent_text(text: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut first = true;
    for _ in 0..text.len() + 1 {
        let brk = rest.char_indices().find(|&(_, c)| is_line_break(c));
        let (line, next) = match brk {
            Some((i, c)) => {
                let len = if rest[i..].starts_with("\r\n") {
                    2
                } else {
                    c.len_utf8()
                };
                (&rest[..i], Some((&rest[i..i + len], &rest[i + len..])))
            }
            None => (rest, None),
        };
        if first {
            out.push_str(line);
            first = false;
        } else if line.trim().is_empty() {
        } else if let Some(stripped) = line.strip_prefix(from) {
            out.push_str(to);
            out.push_str(stripped);
        } else {
            out.push_str(line);
        }
        let Some((brk, after)) = next else {
            break;
        };
        out.push_str(brk);
        rest = after;
    }
    out
}

/// The text before `pos` on its line, when it is only whitespace (`pos` starts its line).
pub(crate) fn indent_if_line_start(src: &str, pos: usize) -> Option<&str> {
    let start = src[..pos]
        .char_indices()
        .rev()
        .find(|&(_, c)| is_line_break(c))
        .map_or(0, |(i, c)| i + c.len_utf8());
    let before = &src[start..pos];
    before.trim().is_empty().then_some(before)
}
