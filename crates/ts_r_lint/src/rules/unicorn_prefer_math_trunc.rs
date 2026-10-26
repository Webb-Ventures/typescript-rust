//! `unicorn/prefer-math-trunc`: `Math.trunc(x)` over bitwise tricks and `parseInt`.
//!
//! Reported:
//! - `x | 0`, `x >> 0`, `x << 0`, `x ^ 0` and their compound assignments, where the right
//!   side is a literal zero (any spelling: `0.`, `0x0`, `0b0`); the message quotes the
//!   operator and the literal as written;
//! - the innermost `~~x` (in `~~~x` the inner pair);
//! - `parseInt(String(x), 10)` and `Number.parseInt(String(x), 10)` with the global
//!   functions.
//!
//! Every fix is a suggestion (`ToInt32` wraps where `Math.trunc` doesn't). It is withheld
//! when it would drop a comment: anything outside the copied operand (the left side, the
//! `~~` operand, or the inside of `String(...)`). A compound assignment rewrites `|=` to
//! `=` and the right side to `Math.trunc(left)`, so it needs a left side without side
//! effects (and, as it copies the left side, no comment anywhere).

use super::prelude::*;
use super::unicorn_util::{
    call_argument_text, call_arguments_text, call_parens, is_number_literal, operator_after,
    would_remove_comments,
};
use super::unicorn_util::{parenthesized_span, space_around_keyword, with_edits};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-math-trunc",
        "Prefer `Math.trunc()` for truncating numbers.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<PreferMathTrunc>,
};

#[derive(Debug, Default)]
struct PreferMathTrunc;

impl Rule for PreferMathTrunc {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Call,
            NodeKind::Binary,
            NodeKind::Assign,
            NodeKind::Unary,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::Call { .. } => check_parse_int(cx, node),
            Node::Binary { left, op, right } => {
                let op = match op {
                    BinaryOp::BitOr => "|",
                    BinaryOp::Shr => ">>",
                    BinaryOp::Shl => "<<",
                    BinaryOp::BitXor => "^",
                    _ => return,
                };
                check_bitwise(cx, node, left, op, right, false);
            }
            Node::Assign { left, op, right } => {
                let op = match op {
                    AssignOp::BitOrAssign => "|=",
                    AssignOp::ShrAssign => ">>=",
                    AssignOp::ShlAssign => "<<=",
                    AssignOp::BitXorAssign => "^=",
                    _ => return,
                };
                check_bitwise(cx, node, left, op, right, true);
            }
            Node::Unary {
                op: UnaryOp::BitNot,
                arg,
            } => {
                let ast = cx.ast();
                let Some(inner) = bit_not_arg(ast, arg) else {
                    return;
                };
                let operand = skip_parens(ast, inner);
                if bit_not_arg(ast, operand).is_some() {
                    return;
                }
                let span = cx.span(node);
                let ok = !would_remove_comments(cx, span, &[parenthesized_span(cx, operand)]);
                let fix = ok.then(|| {
                    let text = format!("Math.trunc({})", call_argument_text(cx, operand));
                    with_edits(Fix::replace(span, text), space_around_keyword(cx, node))
                });
                let r = cx
                    .report(span, "Use `Math.trunc` instead of `~~`.")
                    .id("error-bitwise-not");
                if let Some(f) = fix {
                    r.suggest(
                        "suggestion-bitwise-not",
                        "Replace `~~` with `Math.trunc`.",
                        f,
                    );
                }
            }
            _ => {}
        }
    }
}

/// The operand of `~x` (through parentheses on `id`).
fn bit_not_arg(ast: &ts_r_ast::Ast, id: NodeId) -> Option<NodeId> {
    match *ast.node(skip_parens(ast, id)) {
        Node::Unary {
            op: UnaryOp::BitNot,
            arg,
        } => Some(arg),
        _ => None,
    }
}

fn check_bitwise(
    cx: &mut Ctx<'_, '_>,
    node: NodeId,
    left: NodeId,
    op: &str,
    right: NodeId,
    assignment: bool,
) {
    if !is_number_literal(cx, right, 0.0) {
        return;
    }
    let ast = cx.ast();
    let span = cx.span(node);
    let raw = cx.text(skip_parens(ast, right));
    let left = skip_parens(ast, left);
    let kept: Vec<Span> = if assignment {
        Vec::new()
    } else {
        vec![parenthesized_span(cx, left)]
    };
    let ok = (!assignment || !super::unicorn_static::has_side_effect(cx.ast(), left))
        && !would_remove_comments(cx, span, &kept);
    let fix = if !ok {
        None
    } else if assignment {
        operator_after(cx, left, op).map(|op_span| {
            Fix::replace(op_span, "=").and(
                cx.span(skip_parens(ast, right)),
                format!("Math.trunc({})", call_argument_text(cx, left)),
            )
        })
    } else {
        let text = format!("Math.trunc({})", call_argument_text(cx, left));
        Some(with_edits(
            Fix::replace(span, text),
            space_around_keyword(cx, node),
        ))
    };
    let r = cx
        .report(span, format!("Use `Math.trunc` instead of `{op} {raw}`."))
        .id("error-bitwise");
    if let Some(f) = fix {
        r.suggest(
            "suggestion-bitwise",
            format!("Replace `{op} {raw}` with `Math.trunc`."),
            f,
        );
    }
}

fn check_parse_int(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *cx.node(node)
    else {
        return;
    };
    let callee = skip_parens(ast, callee);
    let name = match *ast.node(callee) {
        Node::Ident { .. } if cx.is_global_ref(callee, "parseInt") => "parseInt",
        Node::Member {
            obj,
            optional: false,
            prop,
        } if ident_name(ast, prop) == Some("parseInt") && cx.is_global_ref(obj, "Number") => {
            "Number.parseInt"
        }
        _ => return,
    };
    let &[string_call, radix] = ast.list(args) else {
        return;
    };
    let string_call = skip_parens(ast, string_call);
    let Node::Call {
        callee: s_callee,
        optional: false,
        args: s_args,
        ..
    } = *ast.node(string_call)
    else {
        return;
    };
    if ast.list(s_args).len() != 1
        || matches!(ast.node(ast.list(s_args)[0]), Node::Spread { .. })
        || !cx.is_global_ref(s_callee, "String")
        || !is_number_literal(cx, radix, 10.0)
    {
        return;
    }
    let span = cx.span(node);
    let fix = call_parens(cx, string_call).and_then(|(open, close)| {
        if would_remove_comments(cx, span, &[Span::new(open.end, close.start)]) {
            return None;
        }
        let text = format!("Math.trunc({})", call_arguments_text(cx, string_call)?);
        Some(with_edits(
            Fix::replace(span, text),
            space_around_keyword(cx, node),
        ))
    });
    let r = cx
        .report(
            span,
            format!("Use `Math.trunc` instead of `{name}(String(...), 10)`."),
        )
        .id("error-parse-int");
    if let Some(f) = fix {
        r.suggest(
            "suggestion-parse-int",
            format!("Replace `{name}(String(...), 10)` with `Math.trunc`."),
            f,
        );
    }
}
