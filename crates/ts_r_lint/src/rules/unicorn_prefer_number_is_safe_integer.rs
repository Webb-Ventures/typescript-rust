//! `unicorn/prefer-number-is-safe-integer`: `Number.isSafeInteger()` over
//! `Number.isInteger()` and hand-written integer checks.
//!
//! - `Number.isInteger(...)` (global `Number`, plain member, not optional) is reported on
//!   `isInteger`, with a suggestion renaming it.
//! - `x % 1 === 0` / `0 === x % 1` (number literals `1` and `0`), `Math.floor(x) === x` /
//!   `Math.trunc(x) === x` either way round (global `Math`, one non-spread argument, the
//!   same reference on the other side) and `_`/`lodash`/`underscore` `.isInteger(x)` /
//!   `.isSafeInteger(x)` (one non-spread argument) are reported whole when `Number` is not
//!   a local binding. The suggestion replaces them by `Number.isSafeInteger(x)` (keeping a
//!   space after a keyword like `return(`), and is withheld when a comment outside `x`
//!   would be lost.

use super::prelude::*;
use super::unicorn_text::{call_argument_text, is_same_reference, would_remove_comments};
use super::unicorn_util::space_around_keyword;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-number-is-safe-integer",
        "Prefer `Number.isSafeInteger()` over integer checks.",
        NO_PRESET,
        Category::Correctness,
    )
    .suggestions(),
    create: create_default::<PreferNumberIsSafeInteger>,
};

#[derive(Debug, Default)]
struct PreferNumberIsSafeInteger;

fn is_number_lit(ast: &Ast, id: NodeId, value: f64) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::NumberLit { value: v } if v.get() == value)
}

/// `<object>.<method>(<one argument>)` with one of `objects` (an identifier) and `methods`:
/// `(object, argument)`.
fn method_call(
    ast: &Ast,
    id: NodeId,
    objects: &[&str],
    methods: &[&str],
    one_arg: bool,
) -> Option<(NodeId, NodeId, Option<NodeId>)> {
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(skip_parens(ast, id))
    else {
        return None;
    };
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(skip_parens(ast, callee))
    else {
        return None;
    };
    let obj = skip_parens(ast, obj);
    if !matches!(ast.node(obj), Node::Ident { .. })
        || !objects.contains(&ident_name(ast, obj)?)
        || !methods.contains(&ident_name(ast, prop)?)
    {
        return None;
    }
    let args = ast.list(args);
    if one_arg {
        let &[arg] = args else {
            return None;
        };
        if matches!(ast.node(arg), Node::Spread { .. }) {
            return None;
        }
        return Some((obj, prop, Some(arg)));
    }
    Some((obj, prop, None))
}

fn modulo_argument(ast: &Ast, id: NodeId) -> Option<NodeId> {
    match *ast.node(skip_parens(ast, id)) {
        Node::Binary {
            op: BinaryOp::Mod,
            left,
            right,
        } if is_number_lit(ast, right, 1.0) => Some(left),
        _ => None,
    }
}

fn math_argument(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let (obj, _, arg) = method_call(cx.ast(), id, &["Math"], &["floor", "trunc"], true)?;
    cx.is_global_ref(obj, "Math").then_some(arg?)
}

fn number_available(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let sem = cx.semantic();
    sem.lookup(sem.enclosing_scope(node), "Number").is_none()
}

fn integer_check_problem(cx: &mut Ctx<'_, '_>, node: NodeId, argument: NodeId) {
    if !number_available(cx, node) {
        return;
    }
    let span = cx.span(node);
    let fix = (!would_remove_comments(cx, span, &[cx.span(argument)])).then(|| {
        let mut fix = Fix::replace(
            span,
            format!("Number.isSafeInteger({})", call_argument_text(cx, argument)),
        );
        for (s, t) in space_around_keyword(cx, node) {
            fix = fix.and(s, t);
        }
        fix
    });
    let r = cx
        .report(
            span,
            "Prefer `Number.isSafeInteger()` over this integer check.",
        )
        .id("prefer-number-is-safe-integer/integer-check-error");
    if let Some(fix) = fix {
        r.suggest(
            "prefer-number-is-safe-integer/integer-check-suggestion",
            "Replace this integer check with `Number.isSafeInteger()`.",
            fix,
        );
    }
}

impl Rule for PreferNumberIsSafeInteger {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *ast.node(node) {
            Node::Call { .. } => {
                if let Some((obj, prop, _)) =
                    method_call(ast, node, &["Number"], &["isInteger"], false)
                    && cx.is_global_ref(obj, "Number")
                {
                    let span = cx.span(prop);
                    cx.report(
                        span,
                        "Prefer `Number.isSafeInteger()` over `Number.isInteger()`.",
                    )
                    .id("prefer-number-is-safe-integer/error")
                    .suggest(
                        "prefer-number-is-safe-integer/suggestion",
                        "Replace `Number.isInteger()` with `Number.isSafeInteger()`.",
                        Fix::replace(span, "isSafeInteger"),
                    );
                    return;
                }
                if let Some((_, _, Some(arg))) = method_call(
                    ast,
                    node,
                    &["_", "lodash", "underscore"],
                    &["isInteger", "isSafeInteger"],
                    true,
                ) {
                    integer_check_problem(cx, node, arg);
                }
            }
            Node::Binary {
                op: BinaryOp::StrictEq,
                left,
                right,
            } => {
                let modulo = if is_number_lit(ast, right, 0.0) {
                    modulo_argument(ast, left)
                } else if is_number_lit(ast, left, 0.0) {
                    modulo_argument(ast, right)
                } else {
                    None
                };
                let argument = modulo.or_else(|| {
                    if let Some(a) = math_argument(cx, left)
                        && is_same_reference(cx, a, right)
                    {
                        return Some(a);
                    }
                    math_argument(cx, right).filter(|&a| is_same_reference(cx, a, left))
                });
                if let Some(argument) = argument {
                    integer_check_problem(cx, node, argument);
                }
            }
            _ => {}
        }
    }
}
