//! `unicorn/escape-case`: hexadecimal and unicode escapes (`\xa9`, `\ud834`, `\u{1d306}`, and
//! `\cj` in regular expressions) use uppercase digits (the default) or lowercase ones
//! (`"lowercase"`). The escape letter itself (`x`, `u`, `c`) is left alone.
//!
//! Strings and regular expressions are checked on their source text and reported whole;
//! each template element of an untagged template is checked and reported on its own, with
//! its delimiters (ESLint's `TemplateElement` range). An escape counts only after an odd run
//! of backslashes, so `\\xa9` (an escaped backslash) is not one.

use super::prelude::*;
use crate::ast_util::template_element_span;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/escape-case",
        "Require escape sequences to use uppercase or lowercase values.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<EscapeCase>,
};

#[derive(Debug, Default)]
struct EscapeCase {
    lowercase: bool,
}

impl FromOptions for EscapeCase {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(EscapeCase {
            lowercase: opts.first().and_then(Value::as_str) == Some("lowercase"),
        })
    }
}

impl Rule for EscapeCase {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::StringLit,
            NodeKind::Directive,
            NodeKind::RegexLit,
            NodeKind::TemplateElement,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let raw = string_literal_span(cx.ast(), cx.src(), node);
        let (report_span, regex) = match *cx.node(node) {
            Node::StringLit { .. } | Node::Directive { .. } => (raw, false),
            Node::RegexLit { .. } => (raw, true),
            Node::TemplateElement { .. } => {
                let lit = cx.parent(node);
                let tagged = lit.and_then(|t| cx.parent(t)).is_some_and(|p| {
                    matches!(*cx.node(p), Node::TaggedTemplate { quasi, .. } if Some(quasi) == lit)
                });
                if tagged {
                    return;
                }
                (template_element_span(cx.src(), raw), false)
            }
            _ => return,
        };
        let text = raw.text(cx.src());
        let Some(fixed) = fix_escapes(text, regex, self.lowercase) else {
            return;
        };
        let (id, message) = if self.lowercase {
            (
                "escape-lowercase",
                "Use lowercase characters for the value of the escape sequence.",
            )
        } else {
            (
                "escape-uppercase",
                "Use uppercase characters for the value of the escape sequence.",
            )
        };
        cx.report(report_span, message)
            .id(id)
            .fix(Fix::replace(raw, fixed));
    }
}

/// `text` with the digits of each escape recased, or `None` when nothing changes.
fn fix_escapes(text: &str, regex: bool, lowercase: bool) -> Option<String> {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut backslashes = 0usize;
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            backslashes += 1;
            i += 1;
            continue;
        }
        let odd = backslashes % 2 == 1;
        backslashes = 0;
        if odd && let Some(len) = escape_len(&b[i..], regex) {
            // The escape letter stays; the rest is ASCII and gets recased.
            out.push_str(&text[copied..=i]);
            let digits = &text[i + 1..i + len];
            if lowercase {
                out.push_str(&digits.to_ascii_lowercase());
            } else {
                out.push_str(&digits.to_ascii_uppercase());
            }
            copied = i + len;
            i += len;
            continue;
        }
        i += 1;
    }
    out.push_str(&text[copied..]);
    (out != text).then_some(out)
}

/// The length of an escape body starting at its letter: `xHH`, `uHHHH`, `u{H+}`, and in
/// regular expressions `cL`.
fn escape_len(b: &[u8], regex: bool) -> Option<usize> {
    let hex = |n: usize, from: usize| {
        b.len() >= from + n && b[from..from + n].iter().all(u8::is_ascii_hexdigit)
    };
    match b.first()? {
        b'x' if hex(2, 1) => Some(3),
        b'u' if b.get(1) == Some(&b'{') => {
            let digits = b[2..].iter().take_while(|c| c.is_ascii_hexdigit()).count();
            (digits > 0 && b.get(2 + digits) == Some(&b'}')).then_some(3 + digits)
        }
        b'u' if hex(4, 1) => Some(5),
        b'c' if regex && b.get(1).is_some_and(u8::is_ascii_alphabetic) => Some(2),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::fix_escapes;

    #[test]
    fn only_escapes_after_an_odd_run_of_backslashes() {
        assert_eq!(fix_escapes(r"'\\xab'", false, false), None);
        assert_eq!(
            fix_escapes(r"'\\\xab\u{1d306}'", false, false).as_deref(),
            Some(r"'\\\xAB\u{1D306}'")
        );
        assert_eq!(
            fix_escapes(r"/\cj/", true, false).as_deref(),
            Some(r"/\cJ/")
        );
        assert_eq!(fix_escapes(r"'\cj\u{'", false, false), None);
    }
}
