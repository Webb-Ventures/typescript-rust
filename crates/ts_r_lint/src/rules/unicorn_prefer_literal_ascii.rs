//! `unicorn/prefer-literal-ascii`: `\xHH`, `\uHHHH` and `\u{H…}` escapes of printable ASCII
//! in strings and untagged template elements are written literally (a backslash or the
//! literal's own quote keeps a backslash; templates also re-escape `` ` `` and `${`). In a
//! template a run of escaped spaces (mixed with literal spaces and tabs) right before a line
//! break stays as it is, since it keeps trailing whitespace visible. There is no fix in a
//! directive, when a digit would extend a preceding `\0` or legacy octal escape, or in a
//! substitution-free template literal type whose value holds a backslash.

use super::prelude::*;
use super::unicorn_no_useless_template_literals::escape_for_template;
use crate::ast_util::{string_literal_span, template_element_span};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-literal-ascii",
        "Prefer literal printable ASCII characters over escape sequences.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferLiteralAscii>,
};

#[derive(Debug, Default)]
struct PreferLiteralAscii;

/// A numeric escape at `i` (a backslash): its end and value (saturated).
fn numeric_escape(b: &[u8], i: usize) -> Option<(usize, u64)> {
    let digits = |from: usize, len: usize| -> Option<u64> {
        let s = b.get(from..from + len)?;
        s.iter().all(u8::is_ascii_hexdigit).then(|| {
            s.iter().fold(0u64, |v, &c| {
                v.saturating_mul(16)
                    .saturating_add((c as char).to_digit(16).unwrap_or(0) as u64)
            })
        })
    };
    match b.get(i + 1)? {
        b'x' => digits(i + 2, 2).map(|v| (i + 4, v)),
        b'u' if b.get(i + 2) == Some(&b'{') => {
            let n = b[i + 3..]
                .iter()
                .take_while(|c| c.is_ascii_hexdigit())
                .count();
            if n == 0 || b.get(i + 3 + n) != Some(&b'}') {
                return None;
            }
            digits(i + 3, n).map(|v| (i + 4 + n, v))
        }
        b'u' => digits(i + 2, 4).map(|v| (i + 6, v)),
        _ => None,
    }
}

fn is_printable(v: u64) -> bool {
    (0x20..=0x7e).contains(&v)
}

/// Whether appending the digit `c` to `text` would lengthen a `\0` or legacy octal escape
/// at its end.
fn extends_legacy_escape(text: &[u8], c: u8) -> bool {
    if !c.is_ascii_digit() {
        return false;
    }
    let mut start = text.len();
    while start > 0 && (b'0'..=b'7').contains(&text[start - 1]) && text.len() - start < 3 {
        start -= 1;
    }
    if start == 0 || text[start - 1] != b'\\' {
        return false;
    }
    let backslashes = text[..start - 1]
        .iter()
        .rev()
        .take_while(|&&x| x == b'\\')
        .count();
    if backslashes % 2 == 1 {
        return false;
    }
    let digits = &text[start..];
    if digits == b"0" {
        return true;
    }
    if c > b'7' {
        return false;
    }
    let max = if digits.first().is_some_and(|&d| d <= b'3') {
        3
    } else {
        2
    };
    digits.len() < max
}

/// A run of spaces, tabs and escaped spaces from `start`: its end and literal form.
fn space_run(b: &[u8], start: usize) -> (usize, Vec<u8>) {
    let mut end = start;
    let mut out = Vec::new();
    while end < b.len() {
        if matches!(b[end], b' ' | b'\t') {
            out.push(b[end]);
            end += 1;
            continue;
        }
        match numeric_escape(b, end) {
            Some((e, 0x20)) if b[end] == b'\\' => {
                out.push(b' ');
                end = e;
            }
            _ => break,
        }
    }
    (end, out)
}

/// (rewritten text, whether it may be fixed).
fn replace_escapes(text: &str, quote: u8) -> (String, bool) {
    let b = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut can_fix = true;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c != b'\\' {
            out.push(c);
            i += 1;
            continue;
        }
        if b.get(i + 1) == Some(&b'\\') {
            out.extend_from_slice(b"\\\\");
            i += 2;
            continue;
        }
        let Some((end, value)) = numeric_escape(b, i).filter(|&(_, v)| is_printable(v)) else {
            out.push(c);
            i += 1;
            continue;
        };
        if quote == b'`' && value == 0x20 {
            let (end, spaces) = space_run(b, i);
            let rest = &text[end..];
            if rest.starts_with(['\n', '\r', '\u{2028}', '\u{2029}']) {
                out.extend_from_slice(&b[i..end]);
            } else {
                out.extend_from_slice(&spaces);
            }
            i = end;
            continue;
        }
        let ch = value as u8;
        if extends_legacy_escape(&out, ch) {
            can_fix = false;
        }
        if ch == b'\\' || ch == quote {
            out.push(b'\\');
        }
        out.push(ch);
        i = end;
    }
    (
        String::from_utf8(out).unwrap_or_else(|_| text.to_owned()),
        can_fix,
    )
}

impl Rule for PreferLiteralAscii {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::StringLit,
            NodeKind::Directive,
            NodeKind::TemplateElement,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let (report, range, fixable, template) = match *cx.node(node) {
            Node::StringLit { .. } => {
                if cx
                    .parent(node)
                    .is_some_and(|p| matches!(ast.node(p), Node::JsxAttr { .. }))
                {
                    return;
                }
                (cx.span(node), cx.span(node), true, false)
            }
            Node::Directive { .. } => {
                let s = string_literal_span(ast, src, node);
                (s, s, false, false)
            }
            Node::TemplateElement {
                cooked,
                invalid_escape,
            } => {
                let Some(tpl) = cx.parent(node) else {
                    return;
                };
                if cx.parent(tpl).is_some_and(
                    |p| matches!(*ast.node(p), Node::TaggedTemplate { quasi, .. } if quasi == tpl),
                ) {
                    return;
                }
                let literal_type = matches!(*ast.node(tpl), Node::TemplateLitType { parts } if ast.list(parts).len() == 1);
                let backslash = invalid_escape || ast.atom(cooked).contains('\\');
                let raw = cx.span(node);
                (
                    template_element_span(src, raw),
                    raw,
                    !(literal_type && backslash),
                    true,
                )
            }
            _ => return,
        };
        let text = range.text(src);
        let quote = if template {
            b'`'
        } else {
            text.as_bytes().first().copied().unwrap_or(b'"')
        };
        let (fixed, can_fix) = replace_escapes(text, quote);
        if fixed == text {
            return;
        }
        let fix = (fixable && can_fix).then(|| {
            let fixed = if template {
                escape_for_template(&fixed)
            } else {
                fixed
            };
            Fix::replace(range, fixed)
        });
        cx.report(
            report,
            "Prefer literal printable ASCII characters over escape sequences.",
        )
        .id("prefer-literal-ascii")
        .fix_opt(fix);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces() {
        assert_eq!(replace_escapes("'\\x41\\u{42}'", b'\'').0, "'AB'");
        assert_eq!(replace_escapes("'\\x27'", b'\'').0, "'\\''");
        assert!(!replace_escapes("'\\0\\x31'", b'\'').1);
        assert_eq!(replace_escapes("a\\x20\n", b'`').0, "a\\x20\n");
        assert_eq!(replace_escapes("a\\x20b", b'`').0, "a b");
    }
}
