//! `unicorn/prefer-short-escape-sequences`: an unescaped `\uXXXX` in a string literal
//! (not a JSX attribute value), directive or untagged template element whose character
//! has a short escape is written that way: `\b \t \n \f \r \v`, and `\0` when no digit
//! follows. The fix rewrites the raw text between the delimiters.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-short-escape-sequences",
        "Prefer shorter alternatives to Unicode escape sequences.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferShortEscapeSequences>,
};

#[derive(Debug, Default)]
struct PreferShortEscapeSequences;

fn short(code: u32, next: Option<char>) -> Option<&'static str> {
    Some(match code {
        0 if next.is_some_and(|c| c.is_ascii_digit()) => return None,
        0 => r"\0",
        0x8 => r"\b",
        0x9 => r"\t",
        0xa => r"\n",
        0xb => r"\v",
        0xc => r"\f",
        0xd => r"\r",
        _ => return None,
    })
}

/// `raw` with the replaceable `\uXXXX` escapes shortened, `None` when nothing changes.
fn fixed(raw: &str) -> Option<String> {
    let b = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    let mut changed = false;
    let mut i = 0;
    let mut copied = 0;
    while i < b.len() {
        if b[i] != b'\\' {
            i += 1;
            continue;
        }
        let run_start = i;
        while i < b.len() && b[i] == b'\\' {
            i += 1;
        }
        let odd = (i - run_start) % 2 == 1;
        let hex = raw.get(i + 1..i + 5);
        if odd
            && b.get(i) == Some(&b'u')
            && let Some(hex) = hex
            && hex.bytes().all(|c| c.is_ascii_hexdigit())
            && let Ok(code) = u32::from_str_radix(hex, 16)
            && let Some(rep) = short(code, raw[i + 5..].chars().next())
        {
            out.push_str(&raw[copied..i - 1]);
            out.push_str(rep);
            i += 5;
            copied = i;
            changed = true;
        }
    }
    out.push_str(&raw[copied..]);
    changed.then_some(out)
}

impl Rule for PreferShortEscapeSequences {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::StringLit,
            NodeKind::Directive,
            NodeKind::TemplateLit,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let span = cx.span(node);
        match *cx.node(node) {
            Node::StringLit { .. } | Node::Directive { .. } => {
                if cx
                    .parent(node)
                    .is_some_and(|p| matches!(cx.node(p), Node::JsxAttr { .. }))
                {
                    return;
                }
                let text = span.text(src);
                let Some(q) = text.chars().next().filter(|c| matches!(c, '\'' | '"')) else {
                    return;
                };
                let Some(close) = text[1..].rfind(q).map(|i| i + 1) else {
                    return;
                };
                let raw = Span::new(span.start + 1, span.start + close as u32);
                if let Some(new) = fixed(raw.text(src)) {
                    self.report(cx, Span::new(span.start, raw.end + 1), raw, new);
                }
            }
            Node::TemplateLit { parts } => {
                if cx
                    .parent(node)
                    .is_some_and(|p| matches!(cx.node(p), Node::TaggedTemplate { .. }))
                {
                    return;
                }
                let parts = ast.list(parts);
                let last = parts.len().saturating_sub(1);
                for (i, &part) in parts.iter().enumerate().step_by(2) {
                    let raw = cx.span(part);
                    if let Some(new) = fixed(raw.text(src)) {
                        let close = if i == last { 1 } else { 2 };
                        let span = Span::new(raw.start.saturating_sub(1), raw.end + close);
                        self.report(cx, span, raw, new);
                    }
                }
            }
            _ => {}
        }
    }
}

impl PreferShortEscapeSequences {
    fn report(&self, cx: &mut Ctx<'_, '_>, span: Span, raw: Span, new: String) {
        cx.report(
            span,
            "Prefer shorter alternatives to Unicode escape sequences.",
        )
        .id("prefer-short-escape-sequences")
        .fix(Fix::replace(raw, new));
    }
}

#[cfg(test)]
mod tests {
    use super::fixed;

    #[test]
    fn shortens_only_unescaped_escapes() {
        assert_eq!(fixed(r"a\u000Ab").as_deref(), Some(r"a\nb"));
        assert_eq!(fixed(r"\\u000A"), None);
        assert_eq!(fixed(r"\\\u0009").as_deref(), Some(r"\\\t"));
        assert_eq!(fixed(r"\u00001"), None);
        assert_eq!(fixed(r"\u0000").as_deref(), Some(r"\0"));
        assert_eq!(fixed(r"\u004"), None);
        assert_eq!(fixed("é\\u000b").as_deref(), Some("é\\v"));
    }
}
