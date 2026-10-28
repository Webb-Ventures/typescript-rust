//! `unicorn/no-unreadable-for-of-expression`: the iterable of a `for…of` / `for await…of`
//! must be simple (TS wrappers and parentheses skipped): an identifier, `this`, `super`, an
//! array literal, a literal or static template, a non-optional property chain (`a.b.#c`) on
//! a simple object, or a non-optional call of an identifier or such a chain whose arguments
//! are simple arguments or array literals of them. A simple argument is an identifier,
//! `this`, `super`, a literal, any template, a property chain, or an argument-less call of an
//! identifier or chain.
//!
//! `x.map(..)` / `x.filter(..)` are left to `no-duplicate-loops` unless `x` is an iterator,
//! and a lazy iterator helper call (`iter.map(..)`) is always reported. Reported at the
//! iterable.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_iter::{
    is_iterator_expression, is_lazy_iterator_helper_call, unwrap_expression,
};
use super::unicorn_text::static_string_value;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unreadable-for-of-expression",
        "Disallow unreadable iterable expressions in `for…of` and `for await…of` loop headers.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoUnreadableForOfExpression>,
};

#[derive(Debug, Default)]
struct NoUnreadableForOfExpression;

const DEPTH: u32 = 500;

fn is_literal(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::RegexLit { .. }
            | Node::BigIntLit { .. }
    )
}

fn is_simple_member(ast: &Ast, id: NodeId, depth: u32) -> bool {
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(id)
    else {
        return false;
    };
    matches!(
        ast.node(prop),
        Node::Ident { .. } | Node::PrivateName { .. }
    ) && is_simple_iterable(ast, obj, depth + 1)
}

fn is_simple_callee(ast: &Ast, callee: NodeId, depth: u32) -> bool {
    let callee = skip_parens(ast, callee);
    matches!(ast.node(callee), Node::Ident { .. }) || is_simple_member(ast, callee, depth + 1)
}

fn is_simple_argument(ast: &Ast, id: NodeId, depth: u32) -> bool {
    if depth > DEPTH {
        return false;
    }
    let id = unwrap_expression(ast, id);
    match *ast.node(id) {
        Node::Ident { .. } | Node::This {} | Node::Super {} | Node::TemplateLit { .. } => true,
        Node::Call {
            callee,
            optional: false,
            args,
            ..
        } => ast.list(args).is_empty() && is_simple_callee(ast, callee, depth),
        _ => is_literal(ast, id) || is_simple_member(ast, id, depth),
    }
}

/// `x.map(cb)` / `x.filter(cb)`: `no-duplicate-loops`' shape.
fn duplicate_loop_call(ast: &Ast, id: NodeId) -> Option<NodeId> {
    let call = method_call(ast, id)?;
    (matches!(call.name, "filter" | "map") && !call.args.is_empty() && call.plain())
        .then_some(call.obj)
}

fn is_simple_call(ast: &Ast, id: NodeId, depth: u32) -> bool {
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(id)
    else {
        return false;
    };
    duplicate_loop_call(ast, id).is_none()
        && is_simple_callee(ast, callee, depth)
        && ast.list(args).iter().all(|&a| {
            let a = unwrap_expression(ast, a);
            is_simple_argument(ast, a, depth + 1)
                || matches!(*ast.node(a), Node::ArrayLit { elements } if ast.list(elements).iter().all(|&e| {
                    !matches!(ast.node(e), Node::Hole {}) && is_simple_argument(ast, e, depth + 1)
                }))
        })
}

fn is_simple_iterable(ast: &Ast, id: NodeId, depth: u32) -> bool {
    if depth > DEPTH {
        return false;
    }
    let id = unwrap_expression(ast, id);
    matches!(
        ast.node(id),
        Node::Ident { .. } | Node::This {} | Node::Super {} | Node::ArrayLit { .. }
    ) || is_literal(ast, id)
        || static_string_value(ast, id).is_some()
        || is_simple_member(ast, id, depth)
        || is_simple_call(ast, id, depth)
}

impl Rule for NoUnreadableForOfExpression {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ForOf]
    }
    fn enter(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::ForOf {
            is_await, right, ..
        } = *ast.node(id)
        else {
            return;
        };
        let iterable = unwrap_expression(ast, right);
        if !is_lazy_iterator_helper_call(cx, iterable, &mut Vec::new())
            && (duplicate_loop_call(ast, iterable)
                .is_some_and(|obj| !is_iterator_expression(cx, obj))
                || is_simple_iterable(ast, iterable, 0))
        {
            return;
        }
        let kind = if is_await {
            "for await…of"
        } else {
            "for…of"
        };
        cx.report(
            cx.span(skip_parens(ast, right)),
            format!("Move the complex iterable expression out of the `{kind}` loop header."),
        )
        .id("no-unreadable-for-of-expression");
    }
}
