//! `unicorn/no-invalid-style-set-property`: bad arguments to
//! `CSSStyleDeclaration#setProperty(name, value[, priority])`.
//!
//! The call is `<style>.setProperty(...)` (non-computed, `?.` allowed) with at least two
//! arguments and no spread among the first three. The receiver is a style declaration: the
//! builtin type checker for `CSSStyleDeclaration`/`CSSStyleProperties` (mixed unions are
//! not, nullish members don't count, a statically known value, an object, an array or
//! `undefined` is not), else (when not known otherwise) a binding named `style` or any
//! `.style` member.
//!
//! - `property`: a statically known camelCase name (`[A-Za-z][A-Za-z0-9]*`) that is not a
//!   CSS property but whose dashed form is (`cssFloat` is `float`, `webkitX` is
//!   `-webkit-x`; the property list is `unicorn_css_properties` plus the WHATWG compat
//!   `-webkit-` aliases). Suggested for a direct string.
//! - `value`: a static string with a top-level `!important` (CSS tokens: `!`, then an ident
//!   that decodes to `important`, outside `()`/`[]`/`{}`/functions; comments between are
//!   allowed). Suggested (moving it to the priority) for a direct string with one marker that
//!   ends the value, no comment after the `!`, something before it, and a priority that is
//!   absent or a static valid, editable one (a literal, direct string or `undefined`).
//! - `priority`: a static primitive priority other than `""`, `important`
//!   (case-insensitive), `null` or `undefined`, with a static non-empty, non-null value.
//!   Suggested as `'important'` for a direct string like `!important`.

use super::prelude::*;
use super::unicorn_css_properties::is_css_property;
use super::unicorn_static::{Static, static_value_control_flow};
use super::unicorn_types::{Checker, Guess};
use super::unicorn_util::escape_string;
use super::unicorn_util::skip_wrappers;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-invalid-style-set-property",
        "Disallow invalid arguments to `CSSStyleDeclaration#setProperty()`.",
        NO_PRESET,
        Category::Correctness,
    )
    .suggestions(),
    create: create_default::<NoInvalidStyleSetProperty>,
};

#[derive(Debug, Default)]
struct NoInvalidStyleSetProperty;

fn non_target_node(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    match *ast.node(id) {
        Node::ObjectLit { .. } | Node::ArrayLit { .. } => true,
        Node::Ident { .. } => ident_name(ast, id) == Some("undefined"),
        _ => false,
    }
}

static STYLE: Checker = Checker {
    target_names: &["CSSStyleDeclaration", "CSSStyleProperties"],
    nullish_in_mixed_union: true,
    mixed_union_non_target: true,
    static_type: Some(|_, _, _| Guess::NonTarget),
    non_target_node: Some(non_target_node),
    ..Checker::DEFAULT
};

/// The standard `-webkit-` aliases (WHATWG Compatibility) missing from the property list.
const WEBKIT_ALIASES: &[&str] = &[
    "-webkit-align-content",
    "-webkit-align-items",
    "-webkit-align-self",
    "-webkit-animation",
    "-webkit-animation-delay",
    "-webkit-animation-direction",
    "-webkit-animation-duration",
    "-webkit-animation-fill-mode",
    "-webkit-animation-iteration-count",
    "-webkit-animation-name",
    "-webkit-animation-play-state",
    "-webkit-animation-timing-function",
    "-webkit-backface-visibility",
    "-webkit-background-origin",
    "-webkit-background-size",
    "-webkit-border-bottom-left-radius",
    "-webkit-border-bottom-right-radius",
    "-webkit-border-radius",
    "-webkit-border-top-left-radius",
    "-webkit-border-top-right-radius",
    "-webkit-box-align",
    "-webkit-box-flex",
    "-webkit-box-ordinal-group",
    "-webkit-box-orient",
    "-webkit-box-pack",
    "-webkit-box-shadow",
    "-webkit-box-sizing",
    "-webkit-filter",
    "-webkit-flex",
    "-webkit-flex-basis",
    "-webkit-flex-direction",
    "-webkit-flex-flow",
    "-webkit-flex-grow",
    "-webkit-flex-shrink",
    "-webkit-flex-wrap",
    "-webkit-justify-content",
    "-webkit-mask-box-image-outset",
    "-webkit-mask-box-image-repeat",
    "-webkit-mask-box-image-slice",
    "-webkit-mask-box-image-source",
    "-webkit-mask-box-image-width",
    "-webkit-order",
    "-webkit-perspective",
    "-webkit-perspective-origin",
    "-webkit-text-size-adjust",
    "-webkit-transform",
    "-webkit-transform-origin",
    "-webkit-transform-style",
    "-webkit-transition",
    "-webkit-transition-delay",
    "-webkit-transition-duration",
    "-webkit-transition-property",
    "-webkit-transition-timing-function",
];

fn is_style_receiver(cx: &Ctx<'_, '_>, obj: NodeId) -> bool {
    match STYLE.guess(cx, obj) {
        Guess::NonTarget => false,
        Guess::Target => true,
        _ => {
            let ast = cx.ast();
            let node = skip_wrappers(ast, obj);
            match *ast.node(node) {
                Node::Ident { .. } => ident_name(ast, node) == Some("style"),
                Node::Member { prop, .. } => ident_name(ast, prop) == Some("style"),
                _ => false,
            }
        }
    }
}

/// The dashed CSS name a camelCase `property` stands for.
fn css_property_name(property: &str) -> Option<String> {
    let mut chars = property.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        || !chars.all(|c| c.is_ascii_alphanumeric())
        || is_css_property(&property.to_ascii_lowercase())
    {
        return None;
    }
    let mut replacement = if property == "cssFloat" {
        "float".to_owned()
    } else {
        let mut out = String::new();
        for c in property.chars() {
            if c.is_ascii_uppercase() {
                out.push('-');
                out.push(c.to_ascii_lowercase());
            } else {
                out.push(c);
            }
        }
        out
    };
    if replacement.starts_with("webkit-") {
        replacement.insert(0, '-');
    }
    (is_css_property(&replacement) || WEBKIT_ALIASES.contains(&replacement.as_str()))
        .then_some(replacement)
}

#[derive(Clone, Copy, PartialEq)]
enum Tok {
    Comment,
    Space,
    Open,
    Close,
    Ident,
    Delim,
    Other,
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || !c.is_ascii()
}

fn is_name(c: char) -> bool {
    is_name_start(c) || c.is_ascii_digit() || c == '-'
}

fn is_newline(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{c}')
}

fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t') || is_newline(c)
}

/// A CSS tokenizer (css-syntax-3) reduced to what the `!important` scan needs.
struct Lexer<'s> {
    s: &'s str,
    chars: Vec<(usize, char)>,
    i: usize,
}

impl<'s> Lexer<'s> {
    fn new(s: &'s str) -> Self {
        Lexer {
            s,
            chars: s.char_indices().collect(),
            i: 0,
        }
    }
    fn at(&self, k: usize) -> Option<char> {
        self.chars.get(self.i + k).map(|&(_, c)| c)
    }
    fn pos(&self) -> usize {
        self.chars.get(self.i).map_or(self.s.len(), |&(p, _)| p)
    }
    fn valid_escape(&self, k: usize) -> bool {
        self.at(k) == Some('\\') && self.at(k + 1).is_some_and(|c| !is_newline(c))
    }
    fn starts_ident(&self, k: usize) -> bool {
        match self.at(k) {
            Some('-') => {
                self.at(k + 1).is_some_and(|c| is_name_start(c) || c == '-')
                    || self.valid_escape(k + 1)
            }
            Some('\\') => self.valid_escape(k),
            Some(c) => is_name_start(c),
            None => false,
        }
    }
    fn starts_number(&self) -> bool {
        match self.at(0) {
            Some('+' | '-') => {
                self.at(1).is_some_and(|c| c.is_ascii_digit())
                    || (self.at(1) == Some('.') && self.at(2).is_some_and(|c| c.is_ascii_digit()))
            }
            Some('.') => self.at(1).is_some_and(|c| c.is_ascii_digit()),
            Some(c) => c.is_ascii_digit(),
            None => false,
        }
    }
    fn escape(&mut self, out: &mut String) {
        // At `\`.
        self.i += 1;
        let Some(c) = self.at(0) else {
            out.push('\u{fffd}');
            return;
        };
        if c.is_ascii_hexdigit() {
            let mut hex = String::new();
            while hex.len() < 6
                && let Some(h) = self.at(0).filter(char::is_ascii_hexdigit)
            {
                hex.push(h);
                self.i += 1;
            }
            if self.at(0).is_some_and(is_ws) {
                if self.at(0) == Some('\r') && self.at(1) == Some('\n') {
                    self.i += 1;
                }
                self.i += 1;
            }
            let n = u32::from_str_radix(&hex, 16).unwrap_or(0xfffd);
            out.push(char::from_u32(n).filter(|_| n != 0).unwrap_or('\u{fffd}'));
        } else {
            out.push(c);
            self.i += 1;
        }
    }
    fn name(&mut self) -> String {
        let mut out = String::new();
        for _ in 0..self.chars.len() + 1 {
            match self.at(0) {
                Some(c) if is_name(c) => {
                    out.push(c);
                    self.i += 1;
                }
                Some('\\') if self.valid_escape(0) => self.escape(&mut out),
                _ => break,
            }
        }
        out
    }
    fn url_rest(&mut self) {
        // After `url(` with no quote: up to `)` (escapes skipped).
        for _ in 0..self.chars.len() + 1 {
            match self.at(0) {
                None => return,
                Some(')') => {
                    self.i += 1;
                    return;
                }
                Some('\\') if self.valid_escape(0) => {
                    let mut sink = String::new();
                    self.escape(&mut sink);
                }
                Some(_) => self.i += 1,
            }
        }
    }
    /// The next token: `(kind, start, end, decoded ident)`.
    fn next(&mut self) -> Option<(Tok, usize, usize, String)> {
        let c = self.at(0)?;
        let start = self.pos();
        let mut text = String::new();
        let kind = if c == '/' && self.at(1) == Some('*') {
            self.i += 2;
            while self.at(0).is_some() && !(self.at(0) == Some('*') && self.at(1) == Some('/')) {
                self.i += 1;
            }
            if self.at(0).is_some() {
                self.i += 2;
            }
            Tok::Comment
        } else if is_ws(c) {
            while self.at(0).is_some_and(is_ws) {
                self.i += 1;
            }
            Tok::Space
        } else if c == '"' || c == '\'' {
            self.i += 1;
            while let Some(d) = self.at(0) {
                if d == c {
                    self.i += 1;
                    break;
                }
                if is_newline(d) {
                    break;
                }
                if d == '\\' {
                    self.i += 1;
                }
                self.i += 1;
            }
            Tok::Other
        } else if matches!(c, '(' | '[' | '{') {
            self.i += 1;
            Tok::Open
        } else if matches!(c, ')' | ']' | '}') {
            self.i += 1;
            Tok::Close
        } else if self.starts_number() {
            self.i += 1;
            while self.at(0).is_some_and(|d| d.is_ascii_digit() || d == '.') {
                self.i += 1;
            }
            if matches!(self.at(0), Some('e' | 'E'))
                && (self.at(1).is_some_and(|d| d.is_ascii_digit())
                    || (matches!(self.at(1), Some('+' | '-'))
                        && self.at(2).is_some_and(|d| d.is_ascii_digit())))
            {
                self.i += 2;
                while self.at(0).is_some_and(|d| d.is_ascii_digit()) {
                    self.i += 1;
                }
            }
            if self.starts_ident(0) {
                self.name();
            } else if self.at(0) == Some('%') {
                self.i += 1;
            }
            Tok::Other
        } else if self.starts_ident(0) {
            text = self.name();
            if self.at(0) == Some('(') {
                self.i += 1;
                if text.eq_ignore_ascii_case("url") {
                    let mut k = 0;
                    while self.at(k).is_some_and(is_ws) {
                        k += 1;
                    }
                    if !matches!(self.at(k), Some('"' | '\'')) {
                        self.url_rest();
                        return Some((Tok::Other, start, self.pos(), text));
                    }
                }
                Tok::Open
            } else {
                Tok::Ident
            }
        } else if (c == '#' && (self.at(1).is_some_and(is_name) || self.valid_escape(1)))
            || (c == '@' && self.starts_ident(1))
        {
            // A hash or an at-keyword.
            self.i += 1;
            self.name();
            Tok::Other
        } else {
            self.i += 1;
            Tok::Delim
        };
        Some((kind, start, self.pos(), text))
    }
}

struct Importance {
    /// `(bang start, value end before it, ident end)`.
    markers: Vec<(usize, usize, usize)>,
    last_token_end: Option<usize>,
    last_comment_end: usize,
}

fn importance(value: &str) -> Importance {
    let mut lexer = Lexer::new(value);
    let mut out = Importance {
        markers: Vec::new(),
        last_token_end: None,
        last_comment_end: 0,
    };
    let mut depth = 0usize;
    let mut bang: Option<(usize, usize)> = None;
    for _ in 0..value.len() + 1 {
        let Some((kind, start, end, text)) = lexer.next() else {
            break;
        };
        match kind {
            Tok::Comment => {
                out.last_comment_end = end;
                continue;
            }
            Tok::Space => continue,
            Tok::Open => depth += 1,
            Tok::Close => depth = depth.saturating_sub(1),
            Tok::Ident if depth == 0 && text.to_lowercase() == "important" => {
                if let Some((s, v)) = bang {
                    out.markers.push((s, v, end));
                }
            }
            _ => {}
        }
        bang = (depth == 0 && kind == Tok::Delim && &value[start..end] == "!").then(|| {
            (
                start,
                out.last_token_end.unwrap_or(0).max(out.last_comment_end),
            )
        });
        out.last_token_end = Some(end);
    }
    out
}

fn direct_string(ast: &Ast, id: NodeId) -> Option<String> {
    match *ast.node(skip_wrappers(ast, id)) {
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        Node::TemplateLit { parts } => match ast.list(parts) {
            &[quasi] => match *ast.node(quasi) {
                Node::TemplateElement {
                    cooked,
                    invalid_escape: false,
                } => Some(ast.atom(cooked).to_owned()),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

fn is_primitive(v: &Static) -> bool {
    matches!(
        v,
        Static::Undefined
            | Static::Null
            | Static::Bool(_)
            | Static::Num(_)
            | Static::Str(_)
            | Static::BigInt(_)
    )
}

fn is_valid_priority(v: &Static) -> bool {
    match v {
        Static::Null | Static::Undefined => true,
        Static::Str(s) => s.is_empty() || s.eq_ignore_ascii_case("important"),
        _ => false,
    }
}

fn is_editable_priority(ast: &Ast, id: NodeId) -> bool {
    let node = skip_wrappers(ast, id);
    matches!(
        ast.node(node),
        Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::RegexLit { .. }
    ) || direct_string(ast, node).is_some()
        || (matches!(ast.node(node), Node::Ident { .. })
            && ident_name(ast, node) == Some("undefined"))
}

fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

fn is_important_like(s: &str) -> bool {
    let s = js_trim(s);
    let s = match s.strip_prefix('!') {
        Some(rest) => rest.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{feff}'),
        None => s,
    };
    s.eq_ignore_ascii_case("important")
}

impl Rule for NoInvalidStyleSetProperty {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call { callee, args, .. } = *ast.node(node) else {
            return;
        };
        let Node::Member { obj, prop, .. } = *ast.node(skip_parens(ast, callee)) else {
            return;
        };
        let args = ast.list(args);
        if ident_name(ast, prop) != Some("setProperty")
            || !matches!(ast.node(prop), Node::Ident { .. })
            || args.len() < 2
            || args
                .iter()
                .take(3)
                .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
            || !is_style_receiver(cx, obj)
        {
            return;
        }
        let (property, value, priority) = (args[0], args[1], args.get(2).copied());
        let property_value = static_value_control_flow(cx, property);
        let value_value = static_value_control_flow(cx, value);
        let priority_value = priority.and_then(|p| static_value_control_flow(cx, p));

        if let Some(Static::Str(name)) = &property_value
            && let Some(replacement) = css_property_name(name)
        {
            let span = cx.span(skip_parens(ast, property));
            let fix = direct_string(ast, property).is_some().then(|| {
                Fix::replace(
                    cx.span(skip_wrappers(ast, property)),
                    escape_string(&replacement, None),
                )
            });
            let r = cx
                .report(
                    span,
                    format!(
                        "Use the CSS property name `{replacement}` instead of `{name}` in `setProperty()`."
                    ),
                )
                .id("no-invalid-style-set-property/property");
            if let Some(fix) = fix {
                r.suggest(
                    "no-invalid-style-set-property/replace",
                    format!("Replace with `{replacement}`."),
                    fix,
                );
            }
        }

        if let Some(Static::Str(v)) = &value_value {
            let imp = importance(v);
            if !imp.markers.is_empty() {
                let fix = self.value_fix(cx, node, value, priority, priority_value.as_ref(), &imp);
                let r = cx
                    .report(
                        cx.span(skip_parens(ast, value)),
                        "Pass `important` as the priority argument instead of including `!important` in the value.",
                    )
                    .id("no-invalid-style-set-property/value");
                if let Some(fix) = fix {
                    r.suggest(
                        "no-invalid-style-set-property/move-important",
                        "Move `!important` to the priority argument.",
                        fix,
                    );
                }
            }
        }

        let (Some(v), Some(priority), Some(p)) = (value_value, priority, priority_value) else {
            return;
        };
        let nonempty = is_primitive(&v) && v != Static::Null && v != Static::Str(String::new());
        if !nonempty || !is_primitive(&p) || is_valid_priority(&p) {
            return;
        }
        let suggest = direct_string(ast, priority).is_some()
            && matches!(&p, Static::Str(s) if is_important_like(s));
        let fix = suggest.then(|| {
            Fix::replace(
                cx.span(skip_wrappers(ast, priority)),
                escape_string("important", None),
            )
        });
        let r = cx
            .report(
                cx.span(skip_parens(ast, priority)),
                "The `setProperty()` priority must be an empty string or `important` (case-insensitive).",
            )
            .id("no-invalid-style-set-property/priority");
        if let Some(fix) = fix {
            r.suggest(
                "no-invalid-style-set-property/replace",
                "Replace with `important`.",
                fix,
            );
        }
    }
}

impl NoInvalidStyleSetProperty {
    fn value_fix(
        &self,
        cx: &Ctx<'_, '_>,
        call: NodeId,
        value: NodeId,
        priority: Option<NodeId>,
        priority_value: Option<&Static>,
        imp: &Importance,
    ) -> Option<Fix> {
        let ast = cx.ast();
        let text = direct_string(ast, value)?;
        let &[(start, value_end, end)] = &imp.markers[..] else {
            return None;
        };
        if Some(end) != imp.last_token_end || imp.last_comment_end > start {
            return None;
        }
        if let Some(p) = priority
            && !(priority_value.is_some_and(is_valid_priority) && is_editable_priority(ast, p))
        {
            return None;
        }
        let replacement = &text[..value_end];
        if replacement.is_empty() {
            return None;
        }
        let fix = Fix::replace(
            cx.span(skip_wrappers(ast, value)),
            escape_string(replacement, None),
        );
        let important = escape_string("important", None);
        Some(match priority {
            Some(p) => fix.and(cx.span(skip_wrappers(ast, p)), important),
            None => {
                let src = cx.src();
                let close = cx.span(call).end - 1;
                let before = skip_trivia_backward(ast, src, close as usize);
                if src.as_bytes().get(before.wrapping_sub(1)) == Some(&b',') {
                    fix.and(Span::at(close), format!(" {important},"))
                } else {
                    fix.and(Span::at(close), format!(", {important}"))
                }
            }
        })
    }
}
