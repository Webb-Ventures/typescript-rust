//! `unicorn/no-unnecessary-boolean-comparison`: `x === true`, `x !== false` (and the
//! mirrored forms) where `x` is known to be a boolean are just `x`; `x === false` and
//! `x !== true` are `!x`.
//!
//! The expression must not contain a `yield` (outside nested functions), whose value isn't
//! known. No fix when a comment sits outside the expression, or when the comparison is a
//! `delete` operand.

use super::prelude::*;
use super::unicorn_conditions::{is_boolean_expression, is_function_node, negated_expression_text};
use super::unicorn_no_useless_coercion::replace_with_expression;
use super::unicorn_text::{paren_span, would_remove_comments};
use ts_r_ast::Ast;
use ts_r_ast::visit::{Visitor, Walk, walk};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unnecessary-boolean-comparison",
        "Disallow unnecessary comparisons against boolean literals.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoUnnecessaryBooleanComparison>,
};

#[derive(Debug, Default)]
struct NoUnnecessaryBooleanComparison;

fn bool_literal(ast: &Ast, id: NodeId) -> Option<bool> {
    match *ast.node(skip_parens(ast, id)) {
        Node::BoolLit { value } => Some(value),
        _ => None,
    }
}

fn contains_yield(ast: &Ast, root: NodeId) -> bool {
    struct Find {
        found: bool,
    }
    impl Visitor for Find {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
            let node = ast.node(id);
            if matches!(node, Node::Yield { .. }) {
                self.found = true;
                return Walk::Stop;
            }
            if is_function_node(node) {
                return Walk::SkipChildren;
            }
            Walk::Continue
        }
    }
    let mut f = Find { found: false };
    walk(ast, root, &mut f);
    f.found
}

impl Rule for NoUnnecessaryBooleanComparison {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Binary { left, op, right } = *cx.node(node) else {
            return;
        };
        if !matches!(op, BinaryOp::StrictEq | BinaryOp::StrictNotEq) {
            return;
        }
        let (expression, literal) = match (bool_literal(ast, left), bool_literal(ast, right)) {
            (Some(v), None) => (skip_parens(ast, right), v),
            (None, Some(v)) => (skip_parens(ast, left), v),
            _ => return,
        };
        if contains_yield(ast, expression) || !is_boolean_expression(cx, expression) {
            return;
        }
        let span = cx.span(node);
        let fix = if would_remove_comments(cx, span, &[paren_span(cx, expression)]) {
            None
        } else if (op == BinaryOp::StrictEq) != literal {
            Some(Fix::replace(span, negated_expression_text(cx, expression)))
        } else {
            replace_with_expression(cx, node, expression)
        };
        cx.report(
            span,
            "Do not compare a boolean expression against a boolean literal.",
        )
        .id("no-unnecessary-boolean-comparison")
        .fix_opt(fix);
    }
}
