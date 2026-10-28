//! `unicorn/no-url-in-search-params`: `new URLSearchParams(x)` with a full URL, which it
//! reads as a query string.
//!
//! The call is `new URLSearchParams(arg)` (by name, one non-spread argument). Reported on the
//! argument when it is a native `URL` (the plugin's builtin type checker), a plain
//! `<receiver>.href` on a URL or on `location` (`window`/`globalThis`/`document`/`self`
//! `.location`), or a static string that, trimmed, starts with `scheme://` and parses as a
//! URL. Suggestions: a detached copy (`new URLSearchParams(<url>.search)`, or `href` renamed
//! to `search`), the URL's live `<url>.searchParams` (for URLs, not `location`), and
//! `new URL(<string>).searchParams` for strings. Replacing the whole expression is withheld
//! when a comment outside the kept argument would be lost.

use super::prelude::*;
use super::unicorn_static::{Static, static_value_control_flow};
use super::unicorn_text::{
    member_object_text, needs_semicolon, paren_span, paren_text, would_remove_comments,
};
use super::unicorn_types::Checker;
use super::unicorn_util::{in_unparenthesized_new_callee, skip_wrappers, space_around_keyword};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-url-in-search-params",
        "Disallow passing full URLs to `URLSearchParams`.",
        NO_PRESET,
        Category::Correctness,
    )
    .suggestions(),
    create: create_default::<NoUrlInSearchParams>,
};

#[derive(Debug, Default)]
struct NoUrlInSearchParams;

static URL: Checker = Checker {
    target_names: &["URL"],
    target_ctors: &["URL"],
    target_imports: &[("node:url", &["URL"]), ("url", &["URL"])],
    class_heritage: false,
    prefer_definitions: true,
    ..Checker::DEFAULT
};

const COPY: (&str, &str) = (
    "detached-copy",
    "Construct a detached copy from the URL's query string.",
);
const LIVE: (&str, &str) = (
    "live-search-params",
    "Use the URL's live `searchParams` (mutations update the URL).",
);

fn is_location(ast: &Ast, id: NodeId) -> bool {
    let id = skip_wrappers(ast, id);
    match *ast.node(id) {
        Node::Ident { .. } => ident_name(ast, id) == Some("location"),
        Node::Member {
            obj,
            optional: false,
            prop,
        } => {
            ident_name(ast, prop) == Some("location")
                && matches!(ast.node(skip_parens(ast, obj)), Node::Ident { .. })
                && matches!(
                    ident_name(ast, skip_parens(ast, obj)),
                    Some("window" | "globalThis" | "document" | "self")
                )
        }
        _ => false,
    }
}

/// A `new` callee written as a member chain on `receiver` needs parentheses when the chain
/// contains a call.
fn chain_has_call(ast: &Ast, receiver: NodeId) -> bool {
    let mut id = receiver;
    for _ in 0..10_000 {
        match *ast.node(id) {
            Node::Call { .. } => return true,
            Node::Member { obj, .. } | Node::Index { obj, .. } => id = obj,
            Node::Paren { expr } | Node::NonNull { expr } => id = expr,
            _ => return false,
        }
    }
    false
}

fn search_params_text(cx: &Ctx<'_, '_>, receiver: NodeId, node: NodeId) -> String {
    let ast = cx.ast();
    let object = member_object_text(cx, receiver, node);
    let mut text = format!("{object}.searchParams");
    if in_unparenthesized_new_callee(cx, node)
        && !object.starts_with('(')
        && chain_has_call(ast, skip_parens(ast, receiver))
    {
        text = format!("({text})");
    }
    if needs_semicolon(cx, cx.span(node).start, &text) {
        text.insert(0, ';');
    }
    text
}

fn live_fix(cx: &Ctx<'_, '_>, receiver: NodeId, node: NodeId) -> Fix {
    let mut fix = Fix::replace(cx.span(node), search_params_text(cx, receiver, node));
    for (s, t) in space_around_keyword(cx, node) {
        fix = fix.and(s, t);
    }
    fix
}

/// A WHATWG parse of `scheme://rest` succeeds (the parts this rule meets: the authority's
/// host and port).
fn can_parse(value: &str) -> bool {
    let value = value.trim_matches(|c: char| c <= ' ');
    let Some((scheme, rest)) = value.split_once("://") else {
        return false;
    };
    let scheme = scheme.to_ascii_lowercase();
    let special = matches!(
        scheme.as_str(),
        "http" | "https" | "ws" | "wss" | "ftp" | "file"
    );
    let rest: String = rest
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    let end = rest
        .find(|c: char| c == '/' || c == '?' || c == '#' || (special && c == '\\'))
        .unwrap_or(rest.len());
    let authority = &rest[..end];
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let (host, port) = if host_port.starts_with('[') {
        match host_port.find(']') {
            Some(i) => (&host_port[..=i], host_port[i + 1..].strip_prefix(':')),
            None => return false,
        }
    } else {
        match host_port.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (host_port, None),
        }
    };
    if let Some(p) = port
        && (!p.bytes().all(|b| b.is_ascii_digit())
            || p.len() > 10
            || p.parse::<u64>().is_ok_and(|n| n > 65535))
    {
        return false;
    }
    if let Some(inner) = host.strip_prefix('[') {
        let inner = &inner[..inner.len() - 1];
        return !inner.is_empty()
            && inner.contains(':')
            && inner
                .bytes()
                .all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.');
    }
    if host.is_empty() {
        return !special || scheme == "file";
    }
    !host.chars().any(|c| {
        matches!(
            c,
            '\0' | '\t'
                | '\n'
                | '\r'
                | ' '
                | '#'
                | '/'
                | ':'
                | '<'
                | '>'
                | '?'
                | '@'
                | '['
                | '\\'
                | ']'
                | '^'
                | '|'
        )
    })
}

fn has_scheme_prefix(value: &str) -> bool {
    let v = value.trim();
    let mut chars = v.char_indices();
    if !chars.next().is_some_and(|(_, c)| c.is_ascii_alphabetic()) {
        return false;
    }
    for (i, c) in chars {
        if c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.') {
            continue;
        }
        return v[i..].starts_with("://");
    }
    false
}

impl Rule for NoUrlInSearchParams {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::New { callee, args, .. } = *ast.node(node) else {
            return;
        };
        let &[argument] = ast.list(args) else {
            return;
        };
        if !matches!(ast.node(callee), Node::Ident { .. })
            || ident_name(ast, callee) != Some("URLSearchParams")
            || matches!(ast.node(argument), Node::Spread { .. })
        {
            return;
        }
        let argument = skip_parens(ast, argument);
        let expression = skip_wrappers(ast, argument);
        let span = cx.span(node);
        let arg_span = cx.span(argument);
        let comments_outside = would_remove_comments(cx, span, &[paren_span(cx, argument)]);
        let message = "`URLSearchParams` does not parse full URLs.";
        let id = "no-url-in-search-params";

        if URL.is_target(cx, argument) {
            let fixes = (!comments_outside).then(|| {
                let copy = Fix::replace(
                    span,
                    format!(
                        "new URLSearchParams({}.search)",
                        member_object_text(cx, argument, node)
                    ),
                );
                (copy, live_fix(cx, argument, node))
            });
            let r = cx.report(arg_span, message).id(id);
            if let Some((copy, live)) = fixes {
                r.suggest(COPY.0, COPY.1, copy)
                    .suggest(LIVE.0, LIVE.1, live);
            }
            return;
        }

        if let Node::Member {
            obj,
            optional: false,
            prop,
        } = *ast.node(expression)
            && ident_name(ast, prop) == Some("href")
        {
            let receiver_is_url = URL.is_target(cx, obj);
            if !receiver_is_url && !is_location(ast, obj) {
                return;
            }
            let copy = Fix::replace(cx.span(prop), "search");
            let live = (receiver_is_url
                && !would_remove_comments(cx, span, &[paren_span(cx, obj)]))
            .then(|| live_fix(cx, obj, node));
            let r = cx
                .report(arg_span, message)
                .id(id)
                .suggest(COPY.0, COPY.1, copy);
            if let Some(live) = live {
                r.suggest(LIVE.0, LIVE.1, live);
            }
            return;
        }

        let Some(Static::Str(value)) = static_value_control_flow(cx, argument) else {
            return;
        };
        if !has_scheme_prefix(&value) || !can_parse(&value) {
            return;
        }
        let fix = (!comments_outside).then(|| {
            let mut fix = Fix::replace(
                span,
                format!("new URL({}).searchParams", paren_text(cx, argument)),
            );
            for (s, t) in space_around_keyword(cx, node) {
                fix = fix.and(s, t);
            }
            fix
        });
        let r = cx.report(arg_span, message).id(id);
        if let Some(fix) = fix {
            r.suggest(
                "parse-url",
                "Parse the URL and use its `searchParams`.",
                fix,
            );
        }
    }
}
