//! `unicorn/prefer-iterator-to-array-at-end`: `x.toArray().map/filter/flatMap(cb)` (no
//! arguments to `toArray`, exactly one non-spread argument to the helper, no optional
//! chaining, no computed names), unless `x` is known not to be an iterator or `cb` could
//! see the array argument: anything but an inline arrow/function, more than two parameters,
//! a rest parameter, or a `function` mentioning `arguments`. Reported at `toArray`.
//!
//! The suggestion rebuilds the helper call as `x.method<between>(args).toArray()`, with `x`
//! as a member object (parenthesized as needed). None when the helper's callee is
//! parenthesized or a comment outside the method name..`)` range would be lost.

use super::prelude::*;
use super::unicorn_ast::{call_tokens, method_call, no_spread_in};
use super::unicorn_iter::is_known_non_iterator;
use super::unicorn_text::{is_parenthesized, member_object_text, would_remove_comments};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-iterator-to-array-at-end",
        "Prefer moving `.toArray()` to the end of iterator helper chains.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<PreferIteratorToArrayAtEnd>,
};

#[derive(Debug, Default)]
struct PreferIteratorToArrayAtEnd;

/// `arguments` appears anywhere inside `root`.
fn mentions_arguments(ast: &Ast, root: NodeId) -> bool {
    let mut stack = vec![root];
    let mut guard = 0u32;
    while let Some(n) = stack.pop() {
        guard += 1;
        if guard > 1_000_000 {
            return true;
        }
        if matches!(ast.node(n), Node::Ident { .. }) && ident_name(ast, n) == Some("arguments") {
            return true;
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    false
}

/// The callback might read the third (array) argument.
pub(crate) fn can_observe_array_argument(ast: &Ast, callback: NodeId) -> bool {
    let f = skip_parens(ast, callback);
    let params = match *ast.node(f) {
        Node::Arrow { params, .. } | Node::FunctionExpr { params, .. } => ast.list(params),
        _ => return true,
    };
    params.len() > 2
        || params
            .iter()
            .any(|&p| matches!(ast.node(p), Node::Param { rest: true, .. }))
        || (matches!(ast.node(f), Node::FunctionExpr { .. }) && mentions_arguments(ast, f))
}

impl Rule for PreferIteratorToArrayAtEnd {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, id) else {
            return;
        };
        if !matches!(call.name, "filter" | "flatMap" | "map")
            || call.args.len() != 1
            || !no_spread_in(ast, call.args, 1)
            || !call.plain()
        {
            return;
        }
        let to_array_id = skip_parens(ast, call.obj);
        let Some(to_array) = method_call(ast, to_array_id) else {
            return;
        };
        if to_array.name != "toArray"
            || !to_array.args.is_empty()
            || !to_array.plain()
            || is_known_non_iterator(cx, to_array.obj)
            || can_observe_array_argument(ast, call.args[0])
        {
            return;
        }
        let fix = (|| {
            let callee = cx.parent(call.prop)?;
            if is_parenthesized(cx, callee) {
                return None;
            }
            let (open, close, _) = call_tokens(cx, id)?;
            let name_end = cx.span(call.prop).end;
            if would_remove_comments(cx, cx.span(id), &[Span::new(name_end, close)]) {
                return None;
            }
            let src = cx.src();
            let text = format!(
                "{}.{}{}({}).toArray()",
                member_object_text(cx, to_array.obj, id),
                call.name,
                &src[name_end as usize..open as usize],
                &src[open as usize + 1..close as usize],
            );
            Some(Fix::replace(cx.span(id), text))
        })();
        let message = format!("Move `.toArray()` after `.{}(…)`.", call.name);
        let r = cx
            .report(cx.span(to_array.prop), message.clone())
            .id("prefer-iterator-to-array-at-end");
        if let Some(f) = fix {
            r.suggest("prefer-iterator-to-array-at-end", message, f);
        }
    }
}
