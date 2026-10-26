//! `unicorn/require-post-message-target-origin`: `x.postMessage(message)` with exactly one
//! non-spread argument and no `?.(` is missing its `targetOrigin`. The report covers the
//! closing parenthesis and the token before it; the suggestions append `<name>.location.origin`
//! (when the receiver is a plain name), `self.location.origin` (unless the name is already
//! `self`, `window` or `globalThis`) and `'*'`. Without type information every such call is
//! reported (the plugin skips calls whose receiver type has no `targetOrigin` parameter only
//! when it has the type checker).

use super::prelude::*;
use super::unicorn_ast::{method_call, no_spread_in};
use super::unicorn_util::prev_token_end;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/require-post-message-target-origin",
        "Enforce using the `targetOrigin` argument with `window.postMessage()`.",
        NO_PRESET,
        Category::Suspicious,
    )
    .suggestions(),
    create: create_default::<RequirePostMessageTargetOrigin>,
};

#[derive(Debug, Default)]
struct RequirePostMessageTargetOrigin;

impl Rule for RequirePostMessageTargetOrigin {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, node) else {
            return;
        };
        if call.name != "postMessage"
            || call.optional_call
            || call.args.len() != 1
            || !no_spread_in(ast, call.args, 1)
        {
            return;
        }
        let span = cx.span(node);
        let src = cx.src();
        if !src[..span.end as usize].ends_with(')') {
            return;
        }
        let close = span.end - 1;
        let before = prev_token_end(cx, close);
        let trailing_comma = src[..before as usize].ends_with(',');

        let mut replacements: Vec<String> = Vec::with_capacity(3);
        if let Some(name) = ident_name(ast, skip_parens(ast, call.obj)) {
            replacements.push(format!("{name}.location.origin"));
            if !matches!(name, "self" | "window" | "globalThis") {
                replacements.push("self.location.origin".to_owned());
            }
        } else {
            replacements.push("self.location.origin".to_owned());
        }
        replacements.push("'*'".to_owned());

        let mut report = cx
            .report(
                Span::new(before, span.end),
                "Missing the `targetOrigin` argument.",
            )
            .id("error");
        for code in replacements {
            let text = if trailing_comma {
                format!(" {code},")
            } else {
                format!(", {code}")
            };
            report = report.suggest(
                "suggestion",
                format!("Use `{code}`."),
                Fix::insert(close, text),
            );
        }
    }
}
