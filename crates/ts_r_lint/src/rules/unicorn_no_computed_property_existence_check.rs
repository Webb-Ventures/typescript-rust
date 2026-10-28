//! `unicorn/no-computed-property-existence-check`: `object[key]` used only for its
//! truthiness (`if`, `?:`, loop tests, `!`, `Boolean(..)`, through `&&`/`||`; the access may
//! sit in TS wrappers) and `key in object`, both with a non-static key, also see inherited
//! properties. Skipped: a static key (literal, expression-free template, `±` number,
//! `#private`), an access typed `boolean`, and a receiver known to be a `Map`, `Set`,
//! `WeakMap`, `WeakSet` or string.
//!
//! The suggestion `Object.hasOwn(object, key)` is withheld for `super`, an optional chain
//! anywhere in the access or its operands, an access inside a TS wrapper, an `in` whose
//! operands are not plain names or `this`, and comments outside the operands.

use super::prelude::*;
use super::unicorn_conditions::{has_optional_chain_element, unwrap_ts_and_parens};
use super::unicorn_text::{call_argument_text, paren_span, would_remove_comments};
use super::unicorn_types::{is_boolean, is_map, is_set, is_string, is_weak_map, is_weak_set};
use super::unicorn_util::is_boolean_context;
use super::unicorn_util::space_around_keyword;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-computed-property-existence-check",
        "Disallow dynamic object property existence checks.",
        NO_PRESET,
        Category::Suspicious,
    )
    .suggestions(),
    create: create_default::<NoComputedPropertyExistenceCheck>,
};

#[derive(Debug, Default)]
struct NoComputedPropertyExistenceCheck;

fn is_ts_wrapper(node: &Node) -> bool {
    matches!(
        node,
        Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
            | Node::Instantiation { .. }
    )
}

fn is_static_key(ast: &Ast, id: NodeId) -> bool {
    let id = unwrap_ts_and_parens(ast, id);
    match *ast.node(id) {
        Node::StringLit { .. }
        | Node::NumberLit { .. }
        | Node::BigIntLit { .. }
        | Node::BoolLit { .. }
        | Node::NullLit {}
        | Node::RegexLit { .. }
        | Node::PrivateName { .. } => true,
        Node::TemplateLit { parts } => ast.list(parts).len() == 1,
        Node::Unary {
            op: UnaryOp::Minus | UnaryOp::Plus,
            arg,
        } => matches!(
            ast.node(arg),
            Node::NumberLit { .. } | Node::BigIntLit { .. }
        ),
        _ => false,
    }
}

/// The outermost TS wrapper (through parentheses) around `id`, or `id`.
fn outermost_wrapper(cx: &Ctx<'_, '_>, id: NodeId) -> NodeId {
    let ast = cx.ast();
    let mut cur = id;
    for _ in 0..100_000 {
        match parent_skip_parens(ast, cx.file.parents, cur) {
            Some(p) if is_ts_wrapper(cx.node(p)) => cur = p,
            _ => break,
        }
    }
    cur
}

fn is_known_non_object(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    is_map(cx, id)
        || is_set(cx, id)
        || is_weak_map(cx, id)
        || is_weak_set(cx, id)
        || is_string(cx, id)
}

fn is_simple_in_operand(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(skip_parens(ast, id)),
        Node::Ident { .. } | Node::This {}
    )
}

fn suggestion(
    cx: &Ctx<'_, '_>,
    node: NodeId,
    object: NodeId,
    property: NodeId,
    is_in: bool,
) -> Option<Fix> {
    let ast = cx.ast();
    if matches!(ast.node(skip_parens(ast, object)), Node::Super {})
        || outermost_wrapper(cx, node) != node
        || has_optional_chain_element(ast, node)
        || has_optional_chain_element(ast, object)
        || has_optional_chain_element(ast, property)
        || (is_in && (!is_simple_in_operand(ast, object) || !is_simple_in_operand(ast, property)))
        || would_remove_comments(
            cx,
            cx.span(node),
            &[paren_span(cx, object), paren_span(cx, property)],
        )
    {
        return None;
    }
    let text = format!(
        "Object.hasOwn({}, {})",
        call_argument_text(cx, object),
        call_argument_text(cx, property)
    );
    let mut fix = Fix::replace(cx.span(node), text);
    for (s, t) in space_around_keyword(cx, node) {
        fix = fix.and(s, t);
    }
    Some(fix)
}

impl Rule for NoComputedPropertyExistenceCheck {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Index, NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (object, property, is_in) = match *cx.node(node) {
            Node::Index { obj, index, .. } => {
                if is_static_key(ast, index)
                    || !is_boolean_context(cx, outermost_wrapper(cx, node))
                    || is_boolean(cx, node)
                    || is_known_non_object(cx, obj)
                {
                    return;
                }
                (obj, index, false)
            }
            Node::Binary {
                left,
                op: BinaryOp::In,
                right,
            } => {
                if is_static_key(ast, left) || is_known_non_object(cx, right) {
                    return;
                }
                (right, left, true)
            }
            _ => return,
        };
        let fix = suggestion(cx, node, object, property, is_in);
        let report = cx
            .report(
                cx.span(node),
                "Do not use a dynamic object property existence check.",
            )
            .id("no-computed-property-existence-check/error");
        if let Some(fix) = fix {
            report.suggest(
                "no-computed-property-existence-check/suggestion",
                "Use `Object.hasOwn()`.",
                fix,
            );
        }
    }
}
