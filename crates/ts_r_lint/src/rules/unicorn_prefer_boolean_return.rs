//! `unicorn/prefer-boolean-return`: `if (x) return true; else return false;` (or the `if`
//! followed by `return false;`, or the values swapped) becomes `return x;` / `return !x;`.
//! A test that isn't a runtime boolean (comparisons, `!`, `delete`, logical of booleans,
//! `Boolean(..)`, the global `Array.isArray(..)`) is wrapped in `Boolean(..)`; with a local
//! `Boolean` that one is reported without a fix. A conditional test is skipped, and so is a
//! flat `if` preceded by a flat `if` returning the same boolean (a chain of guards). No fix
//! when a comment outside the test would be lost, or one trails the last `return`.

use super::prelude::*;
use super::unicorn_conditions::{global_value_available, negated_expression_text};
use super::unicorn_text::{call_argument_text, paren_span, paren_text, would_remove_comments};
use super::unicorn_util::is_global_boolean_call;
use super::unicorn_util::skip_wrappers as unwrap_ts;
use super::unicorn_util::trailing_comment_same_line;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-boolean-return",
        "Prefer directly returning boolean expressions over `if` statements.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferBooleanReturn>,
};

#[derive(Debug, Default)]
struct PreferBooleanReturn;

/// A block with one statement (ignoring empty ones) stands for that statement.
fn node_body(cx: &Ctx<'_, '_>, mut id: NodeId) -> NodeId {
    for _ in 0..1000 {
        let Node::Block { stmts } = *cx.node(id) else {
            return id;
        };
        let mut body = cx
            .ast()
            .list(stmts)
            .iter()
            .filter(|&&s| !matches!(cx.node(s), Node::Empty {}));
        match (body.next(), body.next()) {
            (Some(&only), None) => id = only,
            _ => return id,
        }
    }
    id
}

fn boolean_return(cx: &Ctx<'_, '_>, id: NodeId) -> Option<bool> {
    let Node::Return { arg: Some(arg) } = *cx.node(id) else {
        return None;
    };
    match *cx.node(unwrap_ts(cx.ast(), arg)) {
        Node::BoolLit { value } => Some(value),
        _ => None,
    }
}

fn is_runtime_boolean(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::BoolLit { .. } => true,
        Node::Unary { op, .. } => matches!(op, UnaryOp::Not | UnaryOp::Delete),
        Node::Binary { left, op, right } => match op {
            BinaryOp::Eq
            | BinaryOp::NotEq
            | BinaryOp::StrictEq
            | BinaryOp::StrictNotEq
            | BinaryOp::Lt
            | BinaryOp::LtEq
            | BinaryOp::Gt
            | BinaryOp::GtEq
            | BinaryOp::In
            | BinaryOp::InstanceOf => true,
            BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce => {
                depth < 200
                    && is_runtime_boolean(cx, left, depth + 1)
                    && is_runtime_boolean(cx, right, depth + 1)
            }
            _ => false,
        },
        Node::Call {
            callee,
            optional: false,
            ..
        } => {
            if is_global_boolean_call(cx, id) {
                return true;
            }
            matches!(*ast.node(callee), Node::Member { obj, optional: false, prop }
                if ident_name(ast, prop) == Some("isArray")
                    && matches!(ast.node(obj), Node::Ident { .. })
                    && cx.is_global_ref(obj, "Array"))
        }
        _ => false,
    }
}

/// The statement before / after `stmt` in its statement list.
fn sibling(cx: &Ctx<'_, '_>, stmt: NodeId, offset: isize) -> Option<NodeId> {
    let list = statement_list(cx.ast(), cx.parent(stmt)?)?;
    let i = list.iter().position(|&s| s == stmt)?;
    list.get(i.checked_add_signed(offset)?).copied()
}

impl PreferBooleanReturn {
    fn problem(
        &self,
        cx: &mut Ctx<'_, '_>,
        node: NodeId,
        test: NodeId,
        cons: NodeId,
        alt: NodeId,
        range: Span,
    ) {
        let ast = cx.ast();
        if matches!(ast.node(skip_parens(ast, test)), Node::Conditional { .. }) {
            return;
        }
        let (Some(c), Some(a)) = (boolean_return(cx, cons), boolean_return(cx, alt)) else {
            return;
        };
        if c == a {
            return;
        }
        let negate = !c;
        let has_comments = would_remove_comments(cx, range, &[paren_span(cx, test)])
            || trailing_comment_same_line(cx, cx.span(alt).end).is_some();
        let runtime = is_runtime_boolean(cx, test, 0);
        let needs_global = !negate && !runtime && !global_value_available(cx, node, "Boolean");
        let span = cx.span(node);
        let msg = "Return the boolean expression directly.";
        if has_comments || needs_global {
            cx.report(span, msg).id("prefer-boolean-return");
            return;
        }
        let text = if negate {
            negated_expression_text(cx, test)
        } else if runtime {
            paren_text(cx, skip_parens(ast, test)).to_owned()
        } else {
            format!("Boolean({})", call_argument_text(cx, test))
        };
        cx.report(span, msg)
            .id("prefer-boolean-return")
            .fix(Fix::replace(range, format!("return {text};")));
    }
}

impl Rule for PreferBooleanReturn {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::If]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::If { test, cons, alt } = *cx.node(node) else {
            return;
        };
        if let Some(alt) = alt {
            let (c, a) = (node_body(cx, cons), node_body(cx, alt));
            self.problem(cx, node, test, c, a, cx.span(node));
            return;
        }
        let Some(next) = sibling(cx, node, 1) else {
            return;
        };
        if !matches!(cx.node(next), Node::Return { .. }) {
            return;
        }
        let c = node_body(cx, cons);
        if let Some(value) = boolean_return(cx, c)
            && let Some(prev) = sibling(cx, node, -1)
            && let Node::If {
                cons: pc,
                alt: None,
                ..
            } = *cx.node(prev)
            && boolean_return(cx, node_body(cx, pc)) == Some(value)
        {
            return;
        }
        let range = Span::new(cx.span(node).start, cx.span(next).end);
        self.problem(cx, node, test, c, next, range);
    }
}
