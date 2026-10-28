//! `unicorn/no-useless-coercion`: `Boolean(x)`, `String(x)`, `Number(x)`, `BigInt(x)` (the
//! globals, one non-spread argument), `+x`, `x + ''` / `'' + x` and `x.toString()` on a
//! value already guessed to be of that type (`unicorn_types::is_boolean`, `is_string`,
//! `is_number`, `is_bigint`). The fix keeps the inner expression, adding parentheses where
//! it binds too loosely for the spot and a `;` where it would continue the previous line;
//! it is withheld for `delete` operands and when a comment would go. In TypeScript,
//! `Boolean(x)` inside a function without a return annotation is left alone (it can stop
//! a type predicate from being inferred).

use super::prelude::*;
use super::unicorn_prefer_spread::is_ts_file;
use super::unicorn_text::{
    is_parenthesized, member_object_needs_parens, needs_parens_at_expression_start,
    needs_semicolon, paren_span, paren_text, static_string_value, would_remove_comments,
};
use super::unicorn_types::{is_bigint, is_boolean, is_number, is_string};
use super::unicorn_util::{space_around_keyword, with_edits};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-useless-coercion",
        "Disallow useless type coercions of values that are already of the target type.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoUselessCoercion>,
};

#[derive(Debug, Default)]
struct NoUselessCoercion;

/// Primary and left-hand-side expressions: they never need parentheses.
fn is_strong_precedence(node: &Node) -> bool {
    matches!(
        node,
        Node::ArrayLit { .. }
            | Node::Call { .. }
            | Node::Ident { .. }
            | Node::ImportCall { .. }
            | Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::RegexLit { .. }
            | Node::Member { .. }
            | Node::Index { .. }
            | Node::New { .. }
            | Node::Super {}
            | Node::TaggedTemplate { .. }
            | Node::TemplateLit { .. }
            | Node::This {}
            | Node::NonNull { .. }
            | Node::MetaProperty { .. }
    )
}

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

/// The parent with parentheses skipped, and the child on the path to it.
fn parent_np(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(NodeId, NodeId)> {
    let mut cur = id;
    loop {
        let p = cx.parent(cur)?;
        if matches!(cx.node(p), Node::Paren { .. }) {
            cur = p;
        } else {
            return Some((p, cur));
        }
    }
}

/// `replaceNodeWithExpression`: `node` replaced by its inner `expression`.
pub(crate) fn replace_with_expression(
    cx: &Ctx<'_, '_>,
    node: NodeId,
    expression: NodeId,
) -> Option<Fix> {
    let ast = cx.ast();
    let mut outer = node;
    let parent = loop {
        let (p, _) = parent_np(cx, outer)?;
        if is_ts_wrapper(cx.node(p)) {
            outer = p;
        } else {
            break p;
        }
    };
    if matches!(
        ast.node(parent),
        Node::Unary {
            op: UnaryOp::Delete,
            ..
        }
    ) {
        return None;
    }
    let expression = skip_parens(ast, expression);
    let mut text = paren_text(cx, expression).to_owned();
    let member_object = parent_np(cx, node).is_some_and(|(p, c)| {
        matches!(*ast.node(p), Node::Member { obj, .. } | Node::Index { obj, .. } if obj == c)
    });
    let needs_parens = needs_parens_at_expression_start(cx, expression, node)
        || if member_object {
            member_object_needs_parens(cx, expression, node)
        } else {
            !is_strong_precedence(ast.node(expression))
        };
    if needs_parens && !is_parenthesized(cx, expression) {
        text = format!("({text})");
    }
    let span = cx.span(node);
    if needs_semicolon(cx, span.start, &text) {
        text = format!(";{text}");
    }
    Some(Fix::replace(span, text))
}

fn report(
    cx: &mut Ctx<'_, '_>,
    node: NodeId,
    expression: NodeId,
    description: &str,
    type_name: &str,
) {
    let ast = cx.ast();
    let inner = skip_parens(ast, expression);
    let fix = if would_remove_comments(cx, cx.span(node), &[paren_span(cx, inner)]) {
        None
    } else {
        replace_with_expression(cx, node, expression)
            .map(|f| with_edits(f, space_around_keyword(cx, node)))
    };
    cx.report_node(
        node,
        format!("{description} is unnecessary because the value is already a {type_name}."),
    )
    .id("no-useless-coercion")
    .fix_opt(fix);
}

/// The nearest enclosing function has no return type annotation.
fn in_function_without_return_type(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = node;
    while let Some(p) = cx.parent(cur) {
        match *ast.node(p) {
            Node::FunctionDecl { return_type, .. }
            | Node::FunctionExpr { return_type, .. }
            | Node::Arrow { return_type, .. } => return return_type.is_none(),
            _ => cur = p,
        }
    }
    false
}

impl Rule for NoUselessCoercion {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::Unary, NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *ast.node(node) {
            Node::Call {
                callee,
                optional: false,
                args,
                ..
            } => {
                let args = ast.list(args);
                if let (Node::Ident { .. }, &[arg]) = (ast.node(callee), args)
                    && !matches!(ast.node(arg), Node::Spread { .. })
                {
                    let name = ident_name(ast, callee).unwrap_or_default();
                    let (type_name, is_type): (&str, fn(&Ctx<'_, '_>, NodeId) -> bool) = match name
                    {
                        "Boolean" => ("boolean", is_boolean),
                        "String" => ("string", is_string),
                        "BigInt" => ("bigint", is_bigint),
                        "Number" => ("number", is_number),
                        _ => return,
                    };
                    if !cx.is_global_ref(callee, name) {
                        return;
                    }
                    if name == "Boolean"
                        && (is_ts_file(cx) || cx.env().typescript)
                        && in_function_without_return_type(cx, node)
                    {
                        return;
                    }
                    if is_type(cx, arg) {
                        report(cx, node, arg, &format!("`{name}()`"), type_name);
                    }
                    return;
                }
                if !args.is_empty() {
                    return;
                }
                let Node::Member {
                    obj,
                    prop,
                    optional: false,
                } = *ast.node(skip_parens(ast, callee))
                else {
                    return;
                };
                if ident_name(ast, prop) == Some("toString")
                    && matches!(ast.node(prop), Node::Ident { .. })
                    && is_string(cx, obj)
                {
                    report(cx, node, obj, "`.toString()`", "string");
                }
            }
            Node::Unary {
                op: UnaryOp::Plus,
                arg,
            } => {
                if is_number(cx, arg) {
                    report(cx, node, arg, "`+`", "number");
                }
            }
            Node::Binary {
                left,
                op: BinaryOp::Add,
                right,
            } => {
                let empty = |id: NodeId| {
                    static_string_value(ast, skip_parens(ast, id)).as_deref() == Some("")
                };
                let value = if empty(right) && is_string(cx, left) {
                    left
                } else if empty(left) && is_string(cx, right) {
                    right
                } else {
                    return;
                };
                report(cx, node, value, "Concatenation with `''`", "string");
            }
            _ => {}
        }
    }
}
