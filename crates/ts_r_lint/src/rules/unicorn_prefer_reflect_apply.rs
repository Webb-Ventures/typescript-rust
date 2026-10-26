//! `unicorn/prefer-reflect-apply`: `Reflect.apply(fn, this, args)` over `fn.apply(...)`.
//!
//! Two shapes, both calls of a non-optional member (no optional call):
//! - `fn.apply(null | this, [..] | arguments)` (also `fn["apply"]`), where `fn` is not a
//!   literal, a static string, an array or object literal, `super`, or a chain with an
//!   optional link outside parentheses;
//! - `Function.prototype.apply.call(fn, null | this, [..] | arguments)`.
//!
//! The report is the whole call. The fix rebuilds the call from the target (a sequence
//! keeps its parentheses), the receiver and the list, so it is dropped when a comment lies
//! outside those three.

use super::prelude::*;
use super::unicorn_util::{parenthesized_span, space_around_keyword, with_edits};
use super::unicorn_util_h5::{
    has_unparenthesized_optional_chain, member_object, static_member_name, static_string,
    would_remove_comments,
};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-reflect-apply",
        "Prefer `Reflect.apply()` over `Function#apply()`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferReflectApply>,
};

#[derive(Debug, Default)]
struct PreferReflectApply;

impl Rule for PreferReflectApply {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
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
        let obj = match *ast.node(callee) {
            Node::Member {
                obj,
                optional: false,
                ..
            }
            | Node::Index {
                obj,
                optional: false,
                ..
            } => obj,
            _ => return,
        };
        let inner = skip_parens(ast, obj);
        if matches!(
            ast.node(inner),
            Node::StringLit { .. }
                | Node::NumberLit { .. }
                | Node::BigIntLit { .. }
                | Node::BoolLit { .. }
                | Node::NullLit {}
                | Node::RegexLit { .. }
                | Node::ArrayLit { .. }
                | Node::ObjectLit { .. }
        ) || static_string(ast, inner).is_some()
        {
            return;
        }
        let args = ast.list(args);
        let method = static_member_name(ast, callee);
        let target = if method == Some("apply")
            && let [recv, list] = args
            && !matches!(ast.node(inner), Node::Super {})
            && apply_signature(cx, *recv, *list)
            && !has_unparenthesized_optional_chain(ast, obj)
        {
            inner
        } else if method == Some("call")
            && let [target, recv, list] = args
            && static_member_name(ast, obj) == Some("apply")
            && let Some(proto) = member_object(ast, obj)
            && static_member_name(ast, proto) == Some("prototype")
            && let Some(func) = member_object(ast, proto)
            && ident_name(ast, skip_parens(ast, func)) == Some("Function")
            && apply_signature(cx, *recv, *list)
        {
            skip_parens(ast, *target)
        } else {
            return;
        };
        let (recv, list) = (args[args.len() - 2], args[args.len() - 1]);
        let (recv, list) = (skip_parens(ast, recv), skip_parens(ast, list));
        let seq = matches!(ast.node(target), Node::Seq { .. });
        let target_span = if seq {
            parenthesized_span(cx, target)
        } else {
            cx.span(target)
        };
        let span = cx.span(node);
        let kept = [target_span, cx.span(recv), cx.span(list)];
        let fix = (!would_remove_comments(cx, span, &kept)).then(|| {
            let text = format!(
                "Reflect.apply({}, {}, {})",
                target_span.text(cx.src()),
                cx.text(recv),
                cx.text(list)
            );
            with_edits(Fix::replace(span, text), space_around_keyword(cx, node))
        });
        cx.report(span, "Prefer `Reflect.apply()` over `Function#apply()`.")
            .id("prefer-reflect-apply")
            .fix_opt(fix);
    }
}

/// `null`/`this` and an array literal or `arguments`.
fn apply_signature(cx: &Ctx<'_, '_>, recv: NodeId, list: NodeId) -> bool {
    let ast = cx.ast();
    matches!(
        ast.node(skip_parens(ast, recv)),
        Node::NullLit {} | Node::This {}
    ) && match ast.node(skip_parens(ast, list)) {
        Node::ArrayLit { .. } => true,
        Node::Ident { .. } => ident_name(ast, skip_parens(ast, list)) == Some("arguments"),
        _ => false,
    }
}
