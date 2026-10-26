//! `unicorn/no-asterisk-prefix-in-documentation-comments`: a multi-line `/** ... */` comment
//! that starts its line should not repeat ` * ` on every line. The fix drops the prefixes and
//! moves the text left by the indentation the content lines share, down to the comment's own
//! indentation. Comments where only some lines carry an asterisk are left alone: those
//! asterisks may be content (Markdown list markers).

use super::prelude::*;
use ts_r_ast::CommentKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-asterisk-prefix-in-documentation-comments",
        "Disallow asterisk prefixes and shared indentation in multiline comments.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoAsteriskPrefix>,
};

#[derive(Debug, Default)]
struct NoAsteriskPrefix;

fn is_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// Whitespace as JavaScript's `String.prototype.trim` sees it (line breaks never reach here).
fn is_js_space(c: char) -> bool {
    (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}'
}

/// The text split at every single line-break character (so `\r\n` leaves an empty piece).
fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.split(is_break)
}

/// Only spaces and tabs, optionally followed by the closing `*/`.
fn is_blank(line: &str) -> bool {
    let rest = line.trim_start_matches([' ', '\t']);
    rest.is_empty() || rest == "*/"
}

/// After `prefix`, one or more spaces or tabs and then `*`: the length up to and including
/// the `*`.
fn asterisk_prefix(line: &str, prefix: &str) -> Option<usize> {
    let rest = line.strip_prefix(prefix)?;
    let after = rest.trim_start_matches([' ', '\t']);
    (after.len() < rest.len() && after.starts_with('*'))
        .then(|| prefix.len() + rest.len() - after.len() + 1)
}

/// Rewrites every line after a line break with `edit` (the first line is the comment start).
fn map_lines(text: &str, mut edit: impl FnMut(&str) -> String) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut first = true;
    loop {
        let end = rest.find(is_break).unwrap_or(rest.len());
        let line = &rest[..end];
        if first {
            out.push_str(line);
            first = false;
        } else {
            out.push_str(&edit(line));
        }
        if end == rest.len() {
            break;
        }
        let brk = rest[end..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&rest[end..end + brk]);
        rest = &rest[end + brk..];
    }
    out
}

fn fixed_text(text: &str, prefix: &str) -> Option<String> {
    let content: Vec<&str> = lines(text).skip(1).filter(|l| !is_blank(l)).collect();
    let prefixed = content
        .iter()
        .filter(|l| asterisk_prefix(l, prefix).is_some());
    let count = prefixed.count();
    if count > 0 && count < content.len() {
        return None;
    }
    // ` */` becomes `*/`; ` * text` loses the asterisk and one following space or tab.
    let fixed = map_lines(text, |line| match asterisk_prefix(line, prefix) {
        Some(n) if line[n..].starts_with('/') => format!("{prefix}*/"),
        Some(n) => {
            let rest = &line[n..];
            let rest = rest.strip_prefix([' ', '\t']).unwrap_or(rest);
            format!("{prefix}{rest}")
        }
        None => line.to_string(),
    });
    let mut shared: Option<&str> = None;
    for line in lines(&fixed).skip(1) {
        if is_blank(line) {
            continue;
        }
        let indent = &line[..line.len() - line.trim_start_matches([' ', '\t']).len()];
        let mut s = shared.unwrap_or(indent);
        while !indent.starts_with(s) {
            s = &s[..s.len() - 1];
        }
        shared = Some(s);
    }
    let out = match shared {
        Some(s) if s.starts_with(prefix) && s.len() > prefix.len() => map_lines(&fixed, |line| {
            if line.chars().all(is_js_space) {
                String::new()
            } else if let Some(rest) = line.strip_prefix(s) {
                format!("{prefix}{rest}")
            } else {
                line.to_string()
            }
        }),
        _ => fixed,
    };
    (out != text).then_some(out)
}

impl Rule for NoAsteriskPrefix {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Program]
    }
    fn enter(&self, _node: NodeId, cx: &mut Ctx<'_, '_>) {
        let src = cx.src();
        let mut found = Vec::new();
        for c in &cx.ast().comments {
            if !matches!(c.kind, CommentKind::Block) {
                continue;
            }
            let text = c.span.text(src);
            if !text.starts_with("/**") || text[3..].starts_with('*') || !text.contains(is_break) {
                continue;
            }
            let start = c.span.start as usize;
            let line_start = src[..start]
                .rfind(is_break)
                .map_or(0, |i| i + src[i..].chars().next().map_or(1, char::len_utf8));
            let prefix = &src[line_start..start];
            if !prefix.bytes().all(|b| b == b' ' || b == b'\t') {
                continue;
            }
            if let Some(fixed) = fixed_text(text, prefix) {
                found.push((c.span, fixed));
            }
        }
        for (span, fixed) in found {
            cx.report(
                span,
                "Remove asterisk prefixes and shared indentation from this comment.",
            )
            .id("no-asterisk-prefix-in-documentation-comments")
            .fix(Fix::replace(span, fixed));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixed_text;

    #[test]
    fn fixes() {
        assert_eq!(
            fixed_text("/**\n * a\n */", "").as_deref(),
            Some("/**\na\n*/")
        );
        assert_eq!(fixed_text("/**\nOptions:\n * a\n*/", ""), None);
        assert_eq!(
            fixed_text("/**\n  - a\n    - b\n  */", "").as_deref(),
            Some("/**\n- a\n  - b\n*/")
        );
    }
}
