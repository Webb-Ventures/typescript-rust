//! `unicorn/prefer-array-from-range`: `[...Array(n).keys()]` and `Array.from(Array(n).keys())`
//! (the global `Array`, `n` a valid length or not statically known; see
//! `array_range_length` in `unicorn_prefer_spread.rs`) are
//! `Array.from({length: n}, (_, index) => index)`. The fix wraps a `new` callee and is
//! withheld when a comment outside `n` would be lost.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_prefer_spread::array_range_length;
use super::unicorn_text::{paren_text, would_remove_comments};
use super::unicorn_util::in_unparenthesized_new_callee;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-array-from-range",
        "Prefer `Array.from({length}, …)` when creating range arrays.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferArrayFromRange>,
};

#[derive(Debug, Default)]
struct PreferArrayFromRange;

impl Rule for PreferArrayFromRange {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ArrayLit, NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let length = match *cx.node(node) {
            Node::ArrayLit { elements } => match *ast.list(elements) {
                [e] => match *ast.node(e) {
                    Node::Spread { arg } => array_range_length(cx, arg),
                    _ => None,
                },
                _ => None,
            },
            _ => {
                let Some(m) = method_call(ast, node) else {
                    return;
                };
                let obj = skip_parens(ast, m.obj);
                if m.name != "from"
                    || !m.plain()
                    || m.args.len() != 1
                    || matches!(ast.node(m.args[0]), Node::Spread { .. })
                    || !matches!(ast.node(obj), Node::Ident { .. })
                    || ident_name(ast, obj) != Some("Array")
                    || !cx.semantic().refers_to_global(obj)
                {
                    return;
                }
                array_range_length(cx, m.args[0])
            }
        };
        let Some(length) = length else {
            return;
        };
        let inner = skip_parens(ast, length);
        let span = cx.span(node);
        let fix = (!would_remove_comments(cx, span, &[cx.span(inner)])).then(|| {
            let prop = if matches!(ast.node(inner), Node::Ident { .. })
                && ident_name(ast, inner) == Some("length")
            {
                "length".to_owned()
            } else {
                format!("length: {}", paren_text(cx, length))
            };
            let mut text = format!("Array.from({{{prop}}}, (_, index) => index)");
            if in_unparenthesized_new_callee(cx, node) {
                text = format!("({text})");
            }
            Fix::replace(span, text)
        });
        cx.report(
            span,
            "Prefer `Array.from({length}, …)` when creating range arrays.",
        )
        .fix_opt(fix);
    }
}
