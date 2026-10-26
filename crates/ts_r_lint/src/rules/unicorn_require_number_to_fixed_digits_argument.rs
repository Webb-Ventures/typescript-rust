//! `unicorn/require-number-to-fixed-digits-argument`: `x.toFixed()` must pass its digits.
//!
//! A non-optional call of a `.toFixed` member (not computed) with no arguments is reported
//! from its `(` to its `)`, unless the receiver is a `new` expression (`new BigNumber(1)`
//! has its own `toFixed`). The fix writes `0` just before the `)`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/require-number-to-fixed-digits-argument",
        "Enforce using the digits argument with `Number#toFixed()`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<RequireToFixedDigits>,
};

#[derive(Debug, Default)]
struct RequireToFixedDigits;

impl Rule for RequireToFixedDigits {
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
        if !ast.list(args).is_empty() {
            return;
        }
        let Node::Member { obj, prop, .. } = *ast.node(skip_parens(ast, callee)) else {
            return;
        };
        if ident_name(ast, prop) != Some("toFixed")
            || matches!(ast.node(skip_parens(ast, obj)), Node::New { .. })
        {
            return;
        }
        let src = cx.src();
        let span = cx.span(node);
        if !src[..span.end as usize].ends_with(')') {
            return;
        }
        let close = span.end - 1;
        let open = skip_trivia_backward(ast, src, close as usize) as u32;
        if open == 0 || src.as_bytes()[open as usize - 1] != b'(' {
            return;
        }
        cx.report(
            Span::new(open - 1, span.end),
            "Missing the digits argument.",
        )
        .id("require-number-to-fixed-digits-argument")
        .fix(Fix::insert(close, "0"));
    }
}
