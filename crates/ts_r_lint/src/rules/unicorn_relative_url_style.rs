//! `unicorn/relative-url-style`: relative URLs with (`"always"`) or without (`"never"`, the
//! default) a `./` prefix. Only JavaScript and JSX are linted here (the plugin's CSS, HTML
//! and Markdown halves need other languages).
//!
//! Checked URLs:
//! - the first argument of `new URL(a, b)`, `URL.canParse(a, b)` and `URL.parse(a, b)`
//!   (exactly two arguments, no spread, plain non-optional member), through parentheses,
//!   `as`, `<T>` and `!` (a `satisfies` stops it): a string or a template without
//!   substitutions. A static string second argument is the only base; otherwise the
//!   default bases are `https://example.com/a/b/`, `https://example.com/a/b.html` and
//!   `file:///a/b.html`. A template with substitutions whose first chunk starts with `./`
//!   gets a "Remove leading `./`" suggestion under `"never"`.
//! - JSX attributes `href`, `src`, `poster`, `srcSet`, `imageSrcSet`, `action`,
//!   `formAction`, `cite` on lowercase intrinsic elements (`img`, not `custom-image`,
//!   `Foo` or `a.b`). A `{"..."}`/`` {`...`} `` value is checked like a JS string (not for
//!   `srcSet`/`imageSrcSet`). A plain string value is read as HTML would read it: skipped
//!   when HTML and JSX decode its character references differently; HTML whitespace
//!   around it is ignored; a `srcSet` value (without references) is split into its
//!   candidates, each reported at its own range (a candidate starting `./,` is skipped).
//!
//! A change is reported only when it can't change what the URL resolves to: under
//! `"never"` the source must literally read `./` there and `new URL(rest, base)` must
//! equal `new URL("./" + rest, base)` for every base (`unicorn_url`); under `"always"` the
//! URL must not start with `/`, `.`/`..` followed by the end, `/`, `\`, `?` or `#`, and the
//! same equality must hold.

use super::prelude::*;
use super::unicorn_html::{html_attr_decode, is_html_space, srcset_candidates};
use super::unicorn_static::{Static, static_value_no_side_effects};
use super::unicorn_url::dot_slash_is_noop;
use super::unicorn_util::decode_jsx_entities;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/relative-url-style",
        "Enforce consistent relative URL style.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create::<RelativeUrlStyle>,
};

#[derive(Debug, Default)]
struct RelativeUrlStyle {
    always: bool,
}

impl FromOptions for RelativeUrlStyle {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(RelativeUrlStyle {
            always: opts.first().and_then(Value::as_str) == Some("always"),
        })
    }
}

const DEFAULT_BASES: &[&str] = &[
    "https://example.com/a/b/",
    "https://example.com/a/b.html",
    "file:///a/b.html",
];

const NEVER: &str = "Remove the `./` prefix from the relative URL.";
const ALWAYS: &str = "Add a `./` prefix to the relative URL.";

impl Rule for RelativeUrlStyle {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::New, NodeKind::Call, NodeKind::JsxAttr]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::JsxAttr { name, value } => self.jsx_attr(cx, node, name, value),
            _ => self.url_call(cx, node),
        }
    }
}

impl RelativeUrlStyle {
    /// The problem for `url` whose text starts at `start`: the fix, or `None`.
    fn problem(&self, cx: &Ctx<'_, '_>, url: &str, start: u32, bases: &[&str]) -> Option<Fix> {
        if self.always {
            let b = url.as_bytes();
            let dots = if b.starts_with(b"..") {
                2
            } else {
                usize::from(b.first() == Some(&b'.'))
            };
            let dot_ref =
                dots > 0 && (b.len() == dots || matches!(b[dots], b'#' | b'/' | b'?' | b'\\'));
            if dot_ref || url.starts_with('/') || !dot_slash_is_noop(url, bases) {
                return None;
            }
            Some(Fix::insert(start, "./"))
        } else {
            let rest = url.strip_prefix("./")?;
            if !cx.src()[start as usize..].starts_with("./") || !dot_slash_is_noop(rest, bases) {
                return None;
            }
            Some(Fix::delete(Span::new(start, start + 2)))
        }
    }

    fn message(&self) -> (&'static str, &'static str) {
        if self.always {
            ("always", ALWAYS)
        } else {
            ("never", NEVER)
        }
    }

    fn report(&self, cx: &mut Ctx<'_, '_>, span: Span, fix: Fix) {
        let (id, message) = self.message();
        cx.report(span, message).id(id).fix(fix);
    }

    fn url_call(&self, cx: &mut Ctx<'_, '_>, node: NodeId) {
        let ast = cx.ast();
        let args = match *ast.node(node) {
            Node::New { callee, args, .. } => {
                if ident_name(ast, skip_parens(ast, callee)) != Some("URL") {
                    return;
                }
                args
            }
            Node::Call {
                callee,
                optional: false,
                args,
                ..
            } => {
                let Node::Member {
                    obj,
                    optional: false,
                    prop,
                } = *ast.node(skip_parens(ast, callee))
                else {
                    return;
                };
                if !matches!(ident_name(ast, prop), Some("canParse" | "parse"))
                    || !matches!(ast.node(obj), Node::Ident { .. })
                    || ident_name(ast, obj) != Some("URL")
                {
                    return;
                }
                args
            }
            _ => return,
        };
        let &[first, second] = ast.list(args) else {
            return;
        };
        if [first, second]
            .iter()
            .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
        {
            return;
        }
        let mut arg = first;
        loop {
            match *ast.node(arg) {
                Node::Paren { expr }
                | Node::As { expr, .. }
                | Node::NonNull { expr }
                | Node::TypeAssertion { expr, .. } => arg = expr,
                Node::Satisfies { .. } => return,
                _ => break,
            }
        }
        let span = cx.span(arg);
        let start = span.start + 1;
        let url = match *ast.node(arg) {
            Node::StringLit { value } => ast.atom(value).to_owned(),
            Node::TemplateLit { parts } => {
                let parts = ast.list(parts);
                if parts.len() > 1 {
                    if self.always || !cx.src()[start as usize..].starts_with("./") {
                        return;
                    }
                    let (id, message) = self.message();
                    cx.report(span, message).id(id).suggest(
                        "remove",
                        "Remove leading `./`.",
                        Fix::delete(Span::new(start, start + 2)),
                    );
                    return;
                }
                let Some(&quasi) = parts.first() else {
                    return;
                };
                let Node::TemplateElement {
                    cooked,
                    invalid_escape: false,
                } = *ast.node(quasi)
                else {
                    return;
                };
                ast.atom(cooked).to_owned()
            }
            _ => return,
        };
        let base = match static_value_no_side_effects(cx, second) {
            Some(Static::Str(s)) => Some(s),
            _ => None,
        };
        let bases: Vec<&str> = match &base {
            Some(b) => vec![b.as_str()],
            None => DEFAULT_BASES.to_vec(),
        };
        if let Some(fix) = self.problem(cx, &url, start, &bases) {
            self.report(cx, span, fix);
        }
    }

    fn jsx_attr(&self, cx: &mut Ctx<'_, '_>, attr: NodeId, name: NodeId, value: Option<NodeId>) {
        let ast = cx.ast();
        let Some(parent) = cx.parent(attr) else {
            return;
        };
        let Node::JsxOpening { name: tag, .. } = *ast.node(parent) else {
            return;
        };
        let Some(tag) =
            ident_name(ast, tag).filter(|_| matches!(ast.node(tag), Node::Ident { .. }))
        else {
            return;
        };
        let mut tag_chars = tag.chars();
        if !tag_chars.next().is_some_and(|c| c.is_ascii_lowercase())
            || !tag_chars.all(|c| c.is_ascii_alphanumeric())
        {
            return;
        }
        let Some(attr_name) =
            ident_name(ast, name).filter(|_| matches!(ast.node(name), Node::Ident { .. }))
        else {
            return;
        };
        if !matches!(
            attr_name,
            "href" | "src" | "poster" | "srcSet" | "imageSrcSet" | "action" | "formAction" | "cite"
        ) {
            return;
        }
        let Some(value) = value else {
            return;
        };
        match *ast.node(value) {
            Node::JsxExprContainer { expr: Some(expr) } => {
                if matches!(attr_name, "srcSet" | "imageSrcSet") {
                    return;
                }
                let expr = skip_parens(ast, expr);
                let url = match *ast.node(expr) {
                    Node::StringLit { value } => ast.atom(value).to_owned(),
                    Node::TemplateLit { parts } => match ast.list(parts) {
                        &[quasi] => match *ast.node(quasi) {
                            Node::TemplateElement {
                                cooked,
                                invalid_escape: false,
                            } => ast.atom(cooked).to_owned(),
                            _ => return,
                        },
                        _ => return,
                    },
                    _ => return,
                };
                let span = cx.span(expr);
                if let Some(fix) = self.problem(cx, &url, span.start + 1, DEFAULT_BASES) {
                    self.report(cx, span, fix);
                }
            }
            Node::StringLit { .. } => {
                let span = cx.span(value);
                let start = span.start + 1;
                let raw = &cx.src()[start as usize..(span.end - 1) as usize];
                if html_attr_decode(raw) != Some(decode_jsx_entities(raw)) {
                    return;
                }
                self.html_attr(cx, &attr_name.to_ascii_lowercase(), raw, start);
            }
            _ => {}
        }
    }

    fn html_attr(&self, cx: &mut Ctx<'_, '_>, name: &str, raw: &str, start: u32) {
        if raw.is_empty() {
            return;
        }
        let mut problems = Vec::new();
        if name == "srcset" || name == "imagesrcset" {
            if html_attr_decode(raw).as_deref() != Some(raw) {
                return;
            }
            for (s, e) in srcset_candidates(raw) {
                let url = &raw[s..e];
                if url.starts_with("./,") {
                    continue;
                }
                let at = start + s as u32;
                if let Some(fix) = self.problem(cx, url, at, DEFAULT_BASES) {
                    problems.push((Span::new(at, start + e as u32), fix));
                }
            }
        } else {
            let value = raw.trim_matches(is_html_space);
            let offset = raw.len() - raw.trim_start_matches(is_html_space).len();
            let Some(url) = html_attr_decode(value) else {
                return;
            };
            if url != url.trim() {
                return;
            }
            let at = start + offset as u32;
            if let Some(fix) = self.problem(cx, &url, at, DEFAULT_BASES) {
                problems.push((Span::new(at, at + value.len() as u32), fix));
            }
        }
        for (span, fix) in problems {
            self.report(cx, span, fix);
        }
    }
}
