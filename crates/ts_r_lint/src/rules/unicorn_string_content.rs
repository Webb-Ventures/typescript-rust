//! `unicorn/string-content`: configured patterns that string content should not contain.
//!
//! Each `patterns` key is a regular expression (flags `gu`, plus `i` with `caseSensitive:
//! false`); its value is the replacement (`suggest`), optionally with a `message`, `fix: false`
//! (a suggestion instead of a fix) and `caseSensitive`. The first pattern (in option order)
//! that matches a string is reported, once per string, on the whole node:
//!
//! - string literals (`Literal` with a string value, JSX attribute values with their HTML
//!   entities decoded, directives) match their cooked value; the fix replaces every match
//!   and writes the literal again with its own quote (JSX attributes keep their text and
//!   spell the delimiter quote as an entity);
//! - template elements match their raw text, skipping a match that overlaps an escape
//!   sequence; the replacement goes into the raw text with backslashes doubled and `\r`
//!   escaped, then `` ` `` and `${` that are not already escaped get a backslash. Elements of
//!   templates tagged `gql`, `html`, `sql`, `styled` or `svg` (also through member accesses
//!   and calls: `styled.div`, `styled(Button).attrs({})`) are skipped.
//!
//! With `selectors`, only string nodes matching one of them (esquery, as ESLint listener
//! keys) are checked.

use super::prelude::*;
use super::unicorn_util::{decode_jsx_entities, escape_string};
use crate::esquery::{self, Selector};
use crate::estree::EsTree;
use crate::regex_exec::match_spans;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/string-content",
        "Enforce better string content.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create::<StringContent>,
};

const DEFAULT_MESSAGE: &str = "Prefer `{{suggest}}` over `{{match}}`.";
const SUGGESTION: &str = "Replace `{{match}}` with `{{suggest}}`.";
const IGNORED_TAGS: &[&str] = &["gql", "html", "sql", "styled", "svg"];

#[derive(Debug)]
struct Pattern {
    source: String,
    flags: &'static str,
    suggest: String,
    message: Option<String>,
    fix: bool,
}

#[derive(Debug)]
struct StringContent {
    patterns: Vec<Pattern>,
    selectors: Vec<Selector>,
}

impl FromOptions for StringContent {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let mut patterns = Vec::new();
        if let Some(map) = o.get("patterns").and_then(Value::as_object) {
            for (key, v) in map {
                let (suggest, message, fix, case_sensitive) = match v {
                    Value::String(s) => (s.clone(), None, true, true),
                    Value::Object(p) => (
                        p.get("suggest")
                            .and_then(Value::as_str)
                            .ok_or("`suggest` must be a string")?
                            .to_owned(),
                        p.get("message").and_then(Value::as_str).map(str::to_owned),
                        p.get("fix").and_then(Value::as_bool).unwrap_or(true),
                        p.get("caseSensitive")
                            .and_then(Value::as_bool)
                            .unwrap_or(true),
                    ),
                    _ => return Err(format!("invalid pattern `{key}`")),
                };
                let flags = if case_sensitive { "gu" } else { "giu" };
                if match_spans(key, flags, "").is_none() {
                    return Err(format!("invalid regular expression `{key}`"));
                }
                patterns.push(Pattern {
                    source: key.clone(),
                    flags,
                    suggest,
                    message,
                    fix,
                });
            }
        }
        let mut selectors = Vec::new();
        for s in o
            .get("selectors")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            selectors.push(esquery::parse(s).map_err(|e| format!("invalid selector `{s}`: {e}"))?);
        }
        Ok(StringContent {
            patterns,
            selectors,
        })
    }
}

/// ESLint's message placeholders: `{{ name }}` for `match` and `suggest`; others stay.
fn interpolate(template: &str, p: &Pattern) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        let Some(close) = after.find("}}") else {
            out.push_str(&rest[open..]);
            return out;
        };
        let inner = &after[..close];
        let value = if inner.contains(['{', '}']) {
            None
        } else {
            match inner.trim() {
                "match" => Some(p.source.as_str()),
                "suggest" => Some(p.suggest.as_str()),
                _ => None,
            }
        };
        match value {
            Some(v) => {
                out.push_str(v);
                rest = &after[close + 2..];
            }
            None => {
                // Keep `{` and look for a placeholder starting after it.
                out.push('{');
                rest = &rest[open + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Byte ranges of the escape sequences in a template element's raw text.
fn escape_ranges(raw: &str) -> Vec<(usize, usize)> {
    let b = raw.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' {
            i += 1;
            continue;
        }
        let start = i;
        let rest = &raw[i + 1..];
        let hex =
            |s: &str, n: usize| s.len() >= n && s.as_bytes()[..n].iter().all(u8::is_ascii_hexdigit);
        let len = if let Some(r) = rest
            .strip_prefix(['u', 'U'])
            .and_then(|r| r.strip_prefix('{'))
        {
            let digits = r.bytes().take_while(u8::is_ascii_hexdigit).count();
            if digits > 0 && r.as_bytes().get(digits) == Some(&b'}') {
                Some(2 + digits + 1)
            } else {
                None
            }
        } else {
            None
        };
        let len = len.unwrap_or_else(|| {
            if let Some(r) = rest.strip_prefix(['u', 'U'])
                && hex(r, 4)
            {
                5
            } else if let Some(r) = rest.strip_prefix(['x', 'X'])
                && hex(r, 2)
            {
                3
            } else if rest.starts_with("\r\n") {
                2
            } else {
                rest.chars().next().map_or(0, char::len_utf8)
            }
        });
        out.push((start, i + 1 + len));
        i += 1 + len;
    }
    out
}

/// Matches of `p` in a template element's raw text that don't overlap an escape sequence.
fn template_matches(raw: &str, p: &Pattern) -> Vec<(usize, usize)> {
    let Some(spans) = match_spans(&p.source, p.flags, raw) else {
        return Vec::new();
    };
    let escapes = escape_ranges(raw);
    // Both lists are sorted by start; walk the escapes alongside the matches.
    let mut out = Vec::new();
    let mut e = 0;
    for (s, end) in spans {
        while e < escapes.len() && escapes[e].1 <= s {
            e += 1;
        }
        let overlaps = escapes[e..]
            .iter()
            .take_while(|&&(es, _)| es < end.max(s + 1))
            .any(|&(es, ee)| s < ee && end > es);
        if !overlaps {
            out.push((s, end));
        }
    }
    out
}

fn replace(text: &str, matches: &[(usize, usize)], with: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for &(s, e) in matches {
        out.push_str(&text[pos..s]);
        out.push_str(with);
        pos = e;
    }
    out.push_str(&text[pos..]);
    out
}

/// A backslash before each `` ` `` and `${` not already escaped (an even run of
/// backslashes before it).
fn escape_template_raw(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut backslashes = 0usize;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        let special = c == '`' || (c == '$' && chars.peek() == Some(&'{'));
        if special && backslashes.is_multiple_of(2) {
            out.push('\\');
        }
        out.push(c);
        backslashes = if c == '\\' { backslashes + 1 } else { 0 };
    }
    out
}

/// A template tagged `gql`, `html`, `sql`, `styled` or `svg`, through calls and
/// non-computed member accesses.
fn ignored_tag(ast: &ts_r_ast::Ast, cx: &Ctx<'_, '_>, element: NodeId) -> bool {
    let Some(tpl) = cx.parent(element) else {
        return false;
    };
    let Some(tagged) = cx.parent(tpl) else {
        return false;
    };
    let Node::TaggedTemplate { tag, .. } = *ast.node(tagged) else {
        return false;
    };
    let mut root = tag;
    for _ in 0..10_000 {
        root = skip_parens(ast, root);
        match *ast.node(root) {
            Node::Call { callee, .. } => root = callee,
            Node::Member { obj, .. } => root = obj,
            _ => break,
        }
    }
    matches!(ast.node(root), Node::Ident { .. })
        && ident_name(ast, root).is_some_and(|n| IGNORED_TAGS.contains(&n))
}

impl StringContent {
    fn check(&self, cx: &mut Ctx<'_, '_>, id: NodeId) {
        let ast = cx.ast();
        let src = cx.src();
        match *ast.node(id) {
            Node::StringLit { value } | Node::Directive { value } => {
                let jsx = cx
                    .parent(id)
                    .is_some_and(|p| matches!(ast.node(p), Node::JsxAttr { .. }));
                let value = if jsx {
                    decode_jsx_entities(ast.atom(value))
                } else {
                    ast.atom(value).to_owned()
                };
                if value.is_empty() {
                    return;
                }
                let Some((p, spans)) = self.patterns.iter().find_map(|p| {
                    match_spans(&p.source, p.flags, &value)
                        .filter(|s| !s.is_empty())
                        .map(|s| (p, s))
                }) else {
                    return;
                };
                let fixed = replace(&value, &spans, &p.suggest);
                let span = string_literal_span(ast, src, id);
                let quote = span.text(src).chars().next();
                let text = if jsx {
                    let q = quote.unwrap_or('"');
                    let entity = if q == '"' { "&quot;" } else { "&#39;" };
                    format!("{q}{}{q}", fixed.replace(q, entity))
                } else {
                    escape_string(&fixed, quote)
                };
                self.report(cx, span, p, Fix::replace(span, text));
            }
            Node::TemplateElement { .. } => {
                if ignored_tag(ast, cx, id) {
                    return;
                }
                let raw_span = cx.span(id);
                let raw = raw_span.text(src);
                if raw.is_empty() {
                    return;
                }
                let Some((p, spans)) = self.patterns.iter().find_map(|p| {
                    let m = template_matches(raw, p);
                    (!m.is_empty()).then_some((p, m))
                }) else {
                    return;
                };
                let raw_suggest = p.suggest.replace('\\', "\\\\").replace('\r', "\\r");
                let fixed = replace(raw, &spans, &raw_suggest);
                let span = template_element_span(src, raw_span);
                self.report(
                    cx,
                    span,
                    p,
                    Fix::replace(raw_span, escape_template_raw(&fixed)),
                );
            }
            _ => {}
        }
    }

    fn report(&self, cx: &mut Ctx<'_, '_>, span: Span, p: &Pattern, fix: Fix) {
        let message = interpolate(p.message.as_deref().unwrap_or(DEFAULT_MESSAGE), p);
        let r = cx.report(span, message);
        if p.fix {
            r.fix(fix);
        } else {
            r.suggest("replace", interpolate(SUGGESTION, p), fix);
        }
    }
}

impl Rule for StringContent {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::StringLit,
            NodeKind::Directive,
            NodeKind::TemplateElement,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if self.patterns.is_empty() || !self.selectors.is_empty() || cx.ast().is_jsdoc(node) {
            return;
        }
        self.check(cx, node);
    }
    fn has_run_file(&self) -> bool {
        !self.selectors.is_empty()
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        if self.patterns.is_empty() || self.selectors.is_empty() {
            return;
        }
        let ast = cx.ast();
        let tree = EsTree::build(ast, cx.src());
        let prepared: Vec<_> = self.selectors.iter().map(Selector::prepare).collect();
        let mut targets = Vec::new();
        for n in 0..tree.len() as u32 {
            let e = &tree.nodes[n as usize];
            let is_string = match e.ty {
                "Literal" => matches!(
                    ast.node(e.ast),
                    Node::StringLit { .. } | Node::Directive { .. }
                ),
                "TemplateElement" => true,
                _ => false,
            };
            if is_string && prepared.iter().any(|p| p.matches_listener(&tree, n)) {
                targets.push(e.ast);
            }
        }
        targets.sort_by_key(|&id| (cx.span(id).start, id.index()));
        targets.dedup();
        for t in targets {
            if !ast.is_jsdoc(t) {
                self.check(cx, t);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(
            escape_ranges(r"a\nb\u{1F}\x41\u0041"),
            [(1, 3), (4, 10), (10, 14), (14, 20)]
        );
        assert_eq!(escape_template_raw("${a}`\\`"), "\\${a}\\`\\`");
        assert_eq!(escape_template_raw("\\\\${"), "\\\\\\${");
        let p = Pattern {
            source: "no".into(),
            flags: "gu",
            suggest: "yes".into(),
            message: None,
            fix: true,
        };
        assert_eq!(interpolate(DEFAULT_MESSAGE, &p), "Prefer `yes` over `no`.");
        assert_eq!(interpolate("{{x}} {{ match }} {{", &p), "{{x}} no {{");
        assert_eq!(template_matches(r"\nnn", &p), Vec::<(usize, usize)>::new());
    }
}
