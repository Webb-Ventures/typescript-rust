//! `no-octal-escape`: disallow octal escape sequences (`"\251"`) in string literals.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-octal-escape",
        "Disallow octal escape sequences in string literals",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create_default::<NoOctalEscape>,
};

#[derive(Debug, Default)]
struct NoOctalEscape;

fn is_octal(b: u8) -> bool {
    (b'0'..=b'7').contains(&b)
}

/// The digits of the first octal escape in the raw text of a string literal. A lone `\0`
/// (not followed by a digit) is the NUL escape, not an octal one.
fn first_octal_escape(raw: &str) -> Option<&str> {
    let b = raw.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' {
            i += 1;
            continue;
        }
        let start = i + 1;
        let &d = b.get(start)?;
        let len = match d {
            b'0'..=b'3' => {
                let more = b[start + 1..]
                    .iter()
                    .take(2)
                    .take_while(|&&c| is_octal(c))
                    .count();
                match more {
                    0 if d == b'0' => usize::from(matches!(b.get(start + 1), Some(b'8' | b'9'))),
                    n => n + 1,
                }
            }
            b'4'..=b'7' => 1 + usize::from(b.get(start + 1).is_some_and(|&c| is_octal(c))),
            _ => 0,
        };
        if len > 0 {
            return raw.get(start..start + len);
        }
        // Skip the escaped character (which may be multi-byte).
        i = start + raw[start..].chars().next().map_or(1, char::len_utf8);
    }
    None
}

impl Rule for NoOctalEscape {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::StringLit, NodeKind::Directive]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let mut span = cx.span(node);
        let src = cx.src();
        let mut raw = span.text(src);
        // A directive's span includes its `;`.
        if let Some(t) = raw.strip_suffix(';') {
            raw = t.trim_end();
            span = Span::new(span.start, span.start + raw.len() as u32);
        }
        let Some(digits) = first_octal_escape(raw) else {
            return;
        };
        let msg = format!("Don't use octal: '\\{digits}'. Use '\\u....' instead.");
        cx.report(span, msg).id("octalEscapeSequence");
    }
}
