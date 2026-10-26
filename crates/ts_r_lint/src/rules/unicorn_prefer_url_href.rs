//! `unicorn/prefer-url-href`: `url.href` over `url.toString()` and `String(url)` for a
//! native `URL` (see `unicorn_url_type`).
//!
//! `<url>.toString()` is a plain, non-computed call without arguments; `String(<url>)` calls
//! the global `String` with one argument. Both are reported (on the method name, on
//! `String`). The fix needs the URL to be freshly constructed here (`new URL(...)`, so no
//! own `toString` can have been set): it is withheld when a comment would be lost (between
//! the method name and the end of the call; outside the argument of `String(...)`), and for
//! a parenthesized `(<url>.toString)()`. It writes `.href` after the object (parenthesized
//! when needed), adds a `;` when the line would join the previous one, and keeps spaces
//! around adjacent keywords.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_text::{
    is_parenthesized, member_object_text, needs_semicolon, paren_span, would_remove_comments,
};
use super::unicorn_url_type::{is_fresh_url, is_url};
use super::unicorn_util::space_around_keyword;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-url-href",
        "Prefer `URL#href` over stringifying a `URL`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferUrlHref>,
};

#[derive(Debug, Default)]
struct PreferUrlHref;

const MESSAGE: &str = "Prefer `URL#href` over stringifying a `URL`.";

impl Rule for PreferUrlHref {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call {
            callee,
            optional,
            args,
            ..
        } = *ast.node(node)
        else {
            return;
        };
        if let Some(m) = method_call(ast, node)
            && m.plain()
            && m.name == "toString"
            && m.args.is_empty()
        {
            if !is_url(cx, m.obj) {
                return;
            }
            let span = cx.span(node);
            let range = Span::new(cx.span(m.prop).start, span.end);
            let fix = (!would_remove_comments(cx, range, &[])
                && !is_parenthesized(cx, skip_parens(ast, callee))
                && is_fresh_url(cx, m.obj))
            .then(|| {
                let mut fix = Fix::replace(range, "href");
                for (s, t) in space_around_keyword(cx, node) {
                    fix = fix.and(s, t);
                }
                fix
            });
            cx.report(cx.span(m.prop), MESSAGE)
                .id("prefer-url-href")
                .fix_opt(fix);
            return;
        }
        let callee = skip_parens(ast, callee);
        let &[arg] = ast.list(args) else {
            return;
        };
        if optional
            || !matches!(ast.node(callee), Node::Ident { .. })
            || ident_name(ast, callee) != Some("String")
            || matches!(ast.node(arg), Node::Spread { .. })
            || !cx.is_global_ref(callee, "String")
            || !is_url(cx, arg)
        {
            return;
        }
        let span = cx.span(node);
        let fix = (!would_remove_comments(cx, span, &[paren_span(cx, arg)])
            && is_fresh_url(cx, arg))
        .then(|| {
            let mut text = format!("{}.href", member_object_text(cx, arg, node));
            if needs_semicolon(cx, span.start, &text) {
                text.insert(0, ';');
            }
            let mut fix = Fix::replace(span, text);
            for (s, t) in space_around_keyword(cx, node) {
                fix = fix.and(s, t);
            }
            fix
        });
        cx.report(cx.span(callee), MESSAGE)
            .id("prefer-url-href")
            .fix_opt(fix);
    }
}
