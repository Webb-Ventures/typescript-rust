//! `unicorn/prefer-unicode-code-point-escapes`: legacy escapes (`\xHH`, `\uHHHH` and
//! surrogate pairs of them, octal in strings, `\cX` in regexes) become `\u{X}`. In strings
//! and templates an escape of printable ASCII is left alone, and so is `\uHHHH` for a
//! character with a short escape (`\n`, `\'`, `\\`, ...; `\u0000` only when a digit
//! follows). Templates never had octal escapes. In regexes a surrogate pair is joined only
//! outside a character class, and a lone surrogate inside a class is kept (`v` allows nested
//! classes). A regex without `u`/`v` that needs code point escapes (or already has one) gets
//! a suggestion adding `u`, or a bare report when the result wouldn't parse.

use super::prelude::*;
use crate::ast_util::{string_literal_span, template_element_span};
use crate::regex::{Flags, parse};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-unicode-code-point-escapes",
        "Prefer Unicode code point escapes over legacy escape sequences.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferUnicodeCodePointEscapes>,
};

const MESSAGE: &str = "Prefer Unicode code point escapes.";
const ID: &str = "prefer-unicode-code-point-escapes";

#[derive(Debug, Default)]
struct PreferUnicodeCodePointEscapes;

fn hex(b: &[u8], start: usize, len: usize) -> Option<u32> {
    let s = b.get(start..start + len)?;
    if !s.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    u32::from_str_radix(std::str::from_utf8(s).ok()?, 16).ok()
}

fn code_point_escape(v: u32) -> String {
    format!("\\u{{{v:X}}}")
}

fn is_high(v: u32) -> bool {
    (0xd800..=0xdbff).contains(&v)
}

fn is_low(v: u32) -> bool {
    (0xdc00..=0xdfff).contains(&v)
}

fn is_octal(b: Option<&u8>) -> bool {
    b.is_some_and(|c| (b'0'..=b'7').contains(c))
}

/// An escape already in `\u{…}` form (a valid code point).
fn is_code_point_escape(b: &[u8], i: usize) -> bool {
    if !b[i..].starts_with(b"\\u{") {
        return false;
    }
    let digits = b[i + 3..]
        .iter()
        .take_while(|c| c.is_ascii_hexdigit())
        .count();
    if digits == 0 || b.get(i + 3 + digits) != Some(&b'}') {
        return false;
    }
    let text = std::str::from_utf8(&b[i + 3..i + 3 + digits]).unwrap_or("");
    let text = text.trim_start_matches('0');
    text.len() <= 6
        && u32::from_str_radix(if text.is_empty() { "0" } else { text }, 16)
            .is_ok_and(|v| v <= 0x10ffff)
}

struct Escape {
    end: usize,
    text: String,
    value: Option<u32>,
}

/// The replacement for the escape starting at the backslash `i`.
fn escape_at(b: &[u8], i: usize, regex: bool, in_class: bool, octal: bool) -> Option<Escape> {
    let mut legacy = None;
    if b.get(i + 1) == Some(&b'x') {
        legacy = hex(b, i + 2, 2).map(|v| Escape {
            end: i + 4,
            text: code_point_escape(v),
            value: Some(v),
        });
    }
    if legacy.is_none()
        && b.get(i + 1) == Some(&b'u')
        && b.get(i + 2) != Some(&b'{')
        && let Some(v) = hex(b, i + 2, 4)
    {
        let short = !regex
            && matches!(v, 0 | 8..=13 | 34 | 39 | 47 | 92 | 96)
            && (v != 0 || !b.get(i + 6).is_some_and(u8::is_ascii_digit));
        let lone_in_class = regex && in_class && (is_high(v) || is_low(v));
        if !short && !lone_in_class {
            let mut esc = Escape {
                end: i + 6,
                text: code_point_escape(v),
                value: Some(v),
            };
            if !in_class
                && is_high(v)
                && b.get(i + 6) == Some(&b'\\')
                && b.get(i + 7) == Some(&b'u')
                && let Some(lo) = hex(b, i + 8, 4)
                && is_low(lo)
            {
                let cp = (v - 0xd800) * 0x400 + (lo - 0xdc00) + 0x10000;
                esc = Escape {
                    end: i + 12,
                    text: code_point_escape(cp),
                    value: Some(cp),
                };
            }
            legacy = Some(esc);
        }
    }
    if !regex
        && legacy
            .as_ref()
            .and_then(|e| e.value)
            .is_some_and(|v| (0x20..=0x7e).contains(&v))
    {
        return None;
    }
    if legacy.is_some() {
        return legacy;
    }
    if regex {
        let c = *b.get(i + 2)?;
        if b.get(i + 1) != Some(&b'c') || !c.is_ascii_alphabetic() {
            return None;
        }
        return Some(Escape {
            end: i + 3,
            text: code_point_escape(c.to_ascii_uppercase() as u32 % 32),
            value: None,
        });
    }
    if !octal {
        return None;
    }
    let first = b.get(i + 1);
    if first == Some(&b'0') && !is_octal(b.get(i + 2)) {
        return None;
    }
    let first = *first.filter(|c| (b'0'..=b'7').contains(c))?;
    let max = if first <= b'3' { 3 } else { 2 };
    let mut end = i + 1;
    while end < i + 1 + max && is_octal(b.get(end)) {
        end += 1;
    }
    let v = u32::from_str_radix(std::str::from_utf8(&b[i + 1..end]).ok()?, 8).ok()?;
    Some(Escape {
        end,
        text: code_point_escape(v),
        value: None,
    })
}

/// (rewritten text, whether anything was replaced, whether a `\u{…}` was already there).
fn replace_escapes(
    text: &str,
    regex: bool,
    nested_classes: bool,
    octal: bool,
) -> (String, bool, bool) {
    let b = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut depth = 0u32;
    let mut replaced = false;
    let mut has_code_point = false;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let in_class = depth > 0;
        if c == b'\\' {
            if regex && is_code_point_escape(b, i) {
                has_code_point = true;
            }
            if let Some(e) = escape_at(b, i, regex, in_class, octal) {
                out.extend_from_slice(e.text.as_bytes());
                i = e.end;
                replaced = true;
                continue;
            }
            out.push(c);
            if let Some(&n) = b.get(i + 1) {
                out.push(n);
                i += 1;
            }
            i += 1;
            continue;
        }
        if regex && c == b'[' && (!in_class || nested_classes) {
            depth += 1;
        } else if regex && c == b']' && in_class {
            depth -= 1;
        }
        out.push(c);
        i += 1;
    }
    let out = String::from_utf8(out).unwrap_or_else(|_| text.to_owned());
    (out, replaced, has_code_point)
}

impl Rule for PreferUnicodeCodePointEscapes {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::StringLit,
            NodeKind::Directive,
            NodeKind::RegexLit,
            NodeKind::TemplateElement,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let (report, range, octal) = match *cx.node(node) {
            Node::StringLit { .. } => {
                if cx
                    .parent(node)
                    .is_some_and(|p| matches!(ast.node(p), Node::JsxAttr { .. }))
                {
                    return;
                }
                (cx.span(node), cx.span(node), true)
            }
            Node::Directive { .. } => {
                let s = string_literal_span(ast, src, node);
                (s, s, true)
            }
            Node::RegexLit { .. } => {
                self.regex(node, cx);
                return;
            }
            Node::TemplateElement { .. } => {
                let Some(tpl) = cx.parent(node) else {
                    return;
                };
                if cx.parent(tpl).is_some_and(
                    |p| matches!(*ast.node(p), Node::TaggedTemplate { quasi, .. } if quasi == tpl),
                ) {
                    return;
                }
                let raw = cx.span(node);
                (template_element_span(src, raw), raw, false)
            }
            _ => return,
        };
        let (fixed, replaced, _) = replace_escapes(range.text(src), false, false, octal);
        if replaced {
            cx.report(report, MESSAGE)
                .id(ID)
                .fix(Fix::replace(range, fixed));
        }
    }
}

impl PreferUnicodeCodePointEscapes {
    fn regex(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let span = cx.span(node);
        let raw = cx.text(node);
        let Some(end) = raw.rfind('/').filter(|&e| e > 0) else {
            return;
        };
        let (pattern, flags) = (&raw[1..end], &raw[end + 1..]);
        let (fixed, replaced, has_code_point) =
            replace_escapes(pattern, true, flags.contains('v'), false);
        let unicode = flags.contains('u') || flags.contains('v');
        if !replaced && (unicode || !has_code_point) {
            return;
        }
        let pattern = if replaced { fixed.as_str() } else { pattern };
        let flags = if unicode {
            flags.to_owned()
        } else {
            format!("{flags}u")
        };
        let fix = Fix::replace(span, format!("/{pattern}/{flags}"));
        let r = cx.report(span, MESSAGE).id(ID);
        if unicode {
            r.fix(fix);
        } else if parse(pattern, Flags::new(&flags)).is_ok() {
            r.suggest(
                "prefer-unicode-code-point-escapes/add-unicode-flag",
                "Use Unicode code point escapes and add the `u` flag.",
                fix,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings() {
        let r = |s: &str| replace_escapes(s, false, false, true).0;
        assert_eq!(r("'\\xA9'"), "'\\u{A9}'");
        assert_eq!(r("'\\x41'"), "'\\x41'");
        assert_eq!(r("'\\uD83D\\uDE00'"), "'\\u{1F600}'");
        assert_eq!(r("'\\u000A'"), "'\\u000A'");
        assert_eq!(r("'\\251'"), "'\\u{A9}'");
        assert_eq!(replace_escapes("\\cA", true, false, false).0, "\\u{1}");
    }
}
