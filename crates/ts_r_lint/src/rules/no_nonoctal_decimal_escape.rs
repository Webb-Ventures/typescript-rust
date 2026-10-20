//! `no-nonoctal-decimal-escape`: disallow `\8` and `\9` escape sequences in string literals.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-nonoctal-decimal-escape",
        "Disallow `\\8` and `\\9` escape sequences in string literals",
        ESLINT_REC,
        Category::Correctness,
    )
    .suggestions(),
    create: create_default::<NoNonoctalDecimalEscape>,
};

#[derive(Debug, Default)]
struct NoNonoctalDecimalEscape;

impl Rule for NoNonoctalDecimalEscape {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::StringLit, NodeKind::Directive]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let span = cx.span(node);
        let raw = cx.text(node);
        if !raw.contains("\\8") && !raw.contains("\\9") {
            return;
        }
        let b = raw.as_bytes();
        let mut i = 1;
        let mut prev_escape: Option<(usize, usize)> = None;
        let mut hits = Vec::new();
        while i + 1 < b.len() {
            if b[i] != b'\\' {
                prev_escape = None;
                i += raw[i..].chars().next().map_or(1, char::len_utf8);
                continue;
            }
            let next = b[i + 1];
            if next == b'8' || next == b'9' {
                hits.push((i, next, prev_escape));
            }
            // Escape length (only `\0` matters for the suggestion).
            let len = if next == b'\r' && b.get(i + 2) == Some(&b'\n') {
                3
            } else {
                1 + raw[i + 1..].chars().next().map_or(1, char::len_utf8)
            };
            prev_escape = Some((i, len));
            i += len;
        }
        for (pos, digit, prev) in hits {
            let d = digit as char;
            let start = span.start + pos as u32;
            let esc = Span::new(start, start + 2);
            let mut rep = cx
                .report(esc, format!("Don't use '\\{d}' escape sequence."))
                .id("decimalEscape");
            if let Some((ppos, plen)) = prev
                && plen == 2
                && b[ppos + 1] == b'0'
                && !b
                    .get(ppos + 2)
                    .is_some_and(|c| c.is_ascii_digit() && *c != b'8' && *c != b'9')
            {
                let both = Span::new(span.start + ppos as u32, start + 2);
                rep = rep.suggest(
                    "refactor",
                    format!("Replace '\\0\\{d}' with '\\u0000{d}'. This maintains the current functionality."),
                    Fix::replace(both, format!("\\u0000{d}")),
                );
                rep = rep.suggest(
                    "refactor",
                    format!("Replace '\\{d}' with '\\u003{d}'. This maintains the current functionality."),
                    Fix::replace(esc, format!("\\u003{d}")),
                );
            } else {
                rep = rep.suggest(
                    "refactor",
                    format!(
                        "Replace '\\{d}' with '{d}'. This maintains the current functionality."
                    ),
                    Fix::replace(esc, d.to_string()),
                );
            }
            let _ = rep.suggest(
                "escapeBackslash",
                format!(
                    "Replace '\\{d}' with '\\\\{d}' to include the actual backslash character."
                ),
                Fix::replace(esc, format!("\\\\{d}")),
            );
        }
    }
}
