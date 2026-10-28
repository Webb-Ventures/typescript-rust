//! The check behind `unicorn/no-array-reverse` and `unicorn/no-array-sort`: a plain call of
//! `x.reverse()` (no arguments) or `x.sort(cmp?)` (at most one argument that could be a
//! function) is reported at the method name, with a suggestion to switch to
//! `toReversed()`/`toSorted()`. With `allowExpressionStatement` (default) a call that is a
//! whole expression statement is allowed, unless the receiver is `[...y]`. For `[...y]`
//! there are two suggestions: unwrap the spread (when no comment would be lost) and keep it.
//! Receivers known not to be arrays are skipped.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_text::{member_object_text, would_remove_comments};
use super::unicorn_types::should_skip_known_non_array_receiver;

#[derive(Debug)]
pub(crate) struct ArrayMutate {
    allow_expression_statement: bool,
}

impl ArrayMutate {
    pub(crate) fn from_options(opts: &[Value]) -> ArrayMutate {
        ArrayMutate {
            allow_expression_statement: bool_field(
                obj_opt(opts, 0),
                "allowExpressionStatement",
                true,
            ),
        }
    }
}

/// `isNodeValueNotFunction`: an argument that can't (or most likely doesn't) hold a
/// function.
pub(crate) fn is_value_not_function(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    match *ast.node(id) {
        Node::Binary { op, .. } => !matches!(
            op,
            BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce
        ),
        Node::ArrayLit { .. }
        | Node::ClassExpr { .. }
        | Node::StringLit { .. }
        | Node::NumberLit { .. }
        | Node::BigIntLit { .. }
        | Node::BoolLit { .. }
        | Node::NullLit {}
        | Node::RegexLit { .. }
        | Node::ObjectLit { .. }
        | Node::TemplateLit { .. }
        | Node::Unary { .. }
        | Node::Update { .. }
        | Node::Assign { .. }
        | Node::Await { .. }
        | Node::New { .. }
        | Node::TaggedTemplate { .. }
        | Node::This {} => true,
        Node::Ident { .. } => ident_name(ast, id) == Some("undefined"),
        Node::Call { .. } => !method_call(ast, id).is_some_and(|c| c.name == "bind" && c.plain()),
        _ => false,
    }
}

pub(crate) fn check(
    opts: &ArrayMutate,
    cx: &mut Ctx<'_, '_>,
    node: NodeId,
    method: &str,
    replacement: &str,
) {
    let ast = cx.ast();
    let Some(call) = method_call(ast, node) else {
        return;
    };
    if call.optional_call || call.name != method {
        return;
    }
    if call
        .args
        .iter()
        .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
    {
        return;
    }
    let ok = match method {
        "reverse" => call.args.is_empty(),
        _ => match call.args {
            [] => true,
            [a] => !is_value_not_function(cx, *a),
            _ => false,
        },
    };
    if !ok {
        return;
    }
    let array = skip_parens(ast, call.obj);
    let spread_arg = match *ast.node(array) {
        Node::ArrayLit { elements } => match ast.list(elements) {
            [e] => match *ast.node(*e) {
                Node::Spread { arg } => Some(arg),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    };
    if opts.allow_expression_statement && spread_arg.is_none() {
        let mut cur = node;
        while let Some(p) = cx.parent(cur) {
            if !matches!(cx.node(p), Node::Paren { .. }) {
                if matches!(cx.node(p), Node::ExprStmt { .. }) {
                    return;
                }
                break;
            }
            cur = p;
        }
    }
    if should_skip_known_non_array_receiver(cx, call.obj) {
        return;
    }
    let prop = cx.span(call.prop);
    let rename = Fix::replace(prop, replacement.to_owned());
    let unwrap = spread_arg.and_then(|arg| {
        let array_span = cx.span(array);
        (!would_remove_comments(cx, array_span, &[cx.span(arg)]))
            .then(|| (array_span, member_object_text(cx, arg, array)))
    });
    let mut report = cx
        .report(
            prop,
            format!("Use `Array#{replacement}()` instead of `Array#{method}()`."),
        )
        .id("error");
    if spread_arg.is_some() {
        if let Some((array_span, text)) = unwrap {
            report = report.suggest(
                "suggestion-spreading-array",
                "The spreading object is an array.",
                Fix::replace(array_span, text).and(prop, replacement.to_owned()),
            );
        }
        report.suggest(
            "suggestion-not-spreading-array",
            "The spreading object is NOT an array.",
            rename,
        );
    } else {
        report.suggest(
            "suggestion-apply-replacement",
            format!("Switch to `.{replacement}()`."),
            rename,
        );
    }
}
