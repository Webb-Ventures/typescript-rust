//! `unicorn/consistent-optional-chaining`: both sides of `&&` / `||` are member accesses
//! (through parentheses and TS wrappers) on the same reference, built from an identifier,
//! `this` or `super` and member accesses. With `&&`, a `?.` on the right is unnecessary (the
//! left already proved the base); with `||`, the right should use `?.` exactly when the left
//! does. The right member access is reported.
//!
//! The suggestion rewrites the right access operator (from the end of its object, with
//! parentheses, to the property or past the `[`) to `.` / `[` or `?.` / `?.[`; it is
//! dropped when a comment sits in that range.

use super::prelude::*;
use super::unicorn_ast::would_remove_comments;
use super::unicorn_util::is_same_reference;
use super::unicorn_util::skip_wrappers;
use ts_r_ast::Ast;
use ts_r_span::Span;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/consistent-optional-chaining",
        "Enforce consistent optional chaining for same-base member access.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<ConsistentOptionalChaining>,
};

#[derive(Debug, Default)]
struct ConsistentOptionalChaining;

/// A member access: `(object, optional, computed)`.
fn member(ast: &Ast, id: NodeId) -> Option<(NodeId, bool, bool)> {
    match *ast.node(id) {
        Node::Member { obj, optional, .. } => Some((obj, optional, false)),
        Node::Index { obj, optional, .. } => Some((obj, optional, true)),
        _ => None,
    }
}

/// An identifier, `this` or `super`, or member accesses on one (at most 1000 deep).
fn is_supported_base(ast: &Ast, mut id: NodeId) -> bool {
    for _ in 0..1000 {
        id = skip_wrappers(ast, id);
        match *ast.node(id) {
            Node::Ident { .. } | Node::This {} | Node::Super {} => return true,
            Node::Member { obj, .. } | Node::Index { obj, .. } => id = obj,
            _ => return false,
        }
    }
    false
}

impl Rule for ConsistentOptionalChaining {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Binary { left, op, right } = *cx.node(node) else {
            return;
        };
        if !matches!(op, BinaryOp::LogicalAnd | BinaryOp::LogicalOr) {
            return;
        }
        let (left, right) = (skip_wrappers(ast, left), skip_wrappers(ast, right));
        let (Some((left_obj, left_optional, _)), Some((right_obj, right_optional, computed))) =
            (member(ast, left), member(ast, right))
        else {
            return;
        };
        if !is_supported_base(ast, left_obj)
            || !is_supported_base(ast, right_obj)
            || !is_same_reference(cx, left_obj, right_obj)
        {
            return;
        }
        let remove = if op == BinaryOp::LogicalAnd {
            if !right_optional {
                return;
            }
            true
        } else {
            if left_optional == right_optional {
                return;
            }
            right_optional
        };

        // The access operator: from the end of the object to the property or past `[`.
        let src = cx.src();
        let start = cx.span(right_obj).end;
        let end = if computed {
            let mut at = skip_trivia_forward(src, start as usize);
            if src[at..].starts_with("?.") {
                at = skip_trivia_forward(src, at + 2);
            }
            (at + 1) as u32
        } else {
            match *ast.node(right) {
                Node::Member { prop, .. } => cx.span(prop).start,
                _ => return,
            }
        };
        let range = Span::new(start, end.max(start));
        let fix = (!would_remove_comments(ast, range, &[])).then(|| {
            let text = match (remove, computed) {
                (true, false) => ".",
                (true, true) => "[",
                (false, false) => "?.",
                (false, true) => "?.[",
            };
            Fix::replace(range, text)
        });
        let (id, message, suggest_id, desc) = if remove {
            (
                "consistent-optional-chaining/remove-optional",
                "Remove unnecessary optional chaining.",
                "consistent-optional-chaining/suggest-remove-optional",
                "Remove optional chaining.",
            )
        } else {
            (
                "consistent-optional-chaining/use-optional",
                "Use optional chaining consistently.",
                "consistent-optional-chaining/suggest-use-optional",
                "Use optional chaining.",
            )
        };
        let report = cx.report_node(right, message).id(id);
        if let Some(fix) = fix {
            report.suggest(suggest_id, desc, fix);
        }
    }
}
