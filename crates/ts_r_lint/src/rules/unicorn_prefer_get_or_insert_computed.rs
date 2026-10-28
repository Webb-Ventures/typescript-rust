//! `unicorn/prefer-get-or-insert-computed`: `x.getOrInsert(key, value)` (exactly two
//! arguments, no spread, `?.` allowed) where evaluating `value` has a side effect: an
//! assignment, `await`, call, `import()`, `new`, tagged template, update, `yield` or
//! `delete`, not counting nested function bodies or instance field initialisers (their
//! computed keys do count). The value is reported.
//!
//! The fix renames the method to `getOrInsertComputed` and turns the value into a callback:
//! `f` itself for `f(key)` when the key is that same identifier, else `() => value`
//! (wrapped when it starts with `{`, is a sequence or assignment, or holds `in` inside a
//! `for` initialiser). No fix when the value contains `await`, `yield` or a direct `eval`,
//! or a comment.

use super::prelude::*;
use super::unicorn_ast::{method_call, no_spread_in};
use super::unicorn_text::{expression_text, has_comment_in, paren_span};
use super::unicorn_util::{evaluates_node, is_direct_eval_call};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-get-or-insert-computed",
        "Prefer `.getOrInsertComputed()` when the default value has side effects.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferGetOrInsertComputed>,
};

#[derive(Debug, Default)]
struct PreferGetOrInsertComputed;

fn is_side_effect(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::Assign { .. }
            | Node::Await { .. }
            | Node::Call { .. }
            | Node::ImportCall { .. }
            | Node::New { .. }
            | Node::TaggedTemplate { .. }
            | Node::Update { .. }
            | Node::Yield { .. }
            | Node::Unary {
                op: UnaryOp::Delete,
                ..
            }
    )
}

fn is_unsafe_to_wrap(ast: &Ast, id: NodeId) -> bool {
    matches!(ast.node(id), Node::Await { .. } | Node::Yield { .. }) || is_direct_eval_call(ast, id)
}

/// `value` sits inside a `for (init; ...)` initialiser.
fn in_for_init(cx: &Ctx<'_, '_>, value: NodeId) -> bool {
    let mut cur = value;
    while let Some(p) = cx.parent(cur) {
        if let Node::For {
            init: Some(init), ..
        } = *cx.node(p)
            && init == cur
        {
            return true;
        }
        cur = p;
    }
    false
}

fn callback_text(cx: &Ctx<'_, '_>, key: NodeId, value: NodeId) -> String {
    let ast = cx.ast();
    let key = skip_parens(ast, key);
    if let Some(key_name) = ident_name(ast, key)
        && let Node::Call {
            callee,
            optional: false,
            args,
            ..
        } = *ast.node(value)
        && matches!(ast.node(callee), Node::Ident { .. })
        && let &[arg] = ast.list(args)
        && ident_name(ast, skip_parens(ast, arg)) == Some(key_name)
    {
        return cx.text(callee).to_owned();
    }
    let parens = cx.text(value).starts_with('{')
        || matches!(ast.node(value), Node::Seq { .. } | Node::Assign { .. })
        || (in_for_init(cx, value) && evaluates_in(ast, value));
    format!("() => {}", expression_text(cx, value, parens))
}

/// The expression holds an `in` operator.
fn evaluates_in(ast: &Ast, value: NodeId) -> bool {
    super::unicorn_ast::contains_node(ast, value, |n| {
        matches!(
            n,
            Node::Binary {
                op: BinaryOp::In,
                ..
            }
        )
    })
}

impl Rule for PreferGetOrInsertComputed {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, node) else {
            return;
        };
        if call.name != "getOrInsert" || call.args.len() != 2 || !no_spread_in(ast, call.args, 2) {
            return;
        }
        let (key, value) = (call.args[0], skip_parens(ast, call.args[1]));
        if !evaluates_node(ast, value, &is_side_effect) {
            return;
        }
        let fix = (!evaluates_node(ast, value, &is_unsafe_to_wrap)
            && !has_comment_in(cx, paren_span(cx, value)))
        .then(|| {
            Fix::replace(cx.span(call.prop), "getOrInsertComputed")
                .and(paren_span(cx, value), callback_text(cx, key, value))
        });
        cx.report_node(
            value,
            "Prefer `.getOrInsertComputed()` when the default value has side effects.",
        )
        .id("prefer-get-or-insert-computed")
        .fix_opt(fix);
    }
}
