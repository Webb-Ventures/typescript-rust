//! `unicorn/prefer-escaped-irregular-whitespace`: irregular whitespace (`\v`, `\f`, NEL,
//! NBSP, the U+2000 block, the line and paragraph separators, BOM, ...) written raw in a
//! string, regex or untagged template element is replaced by `\u{XXXX}`. A character
//! already behind a backslash (a line continuation or identity escape) loses that
//! backslash, except a backslash-escaped U+2028/U+2029, which is a line continuation and
//! stays. Regexes are only fixed with the `u` or `v` flag; JSX attribute strings are skipped
//! (they don't read escapes).

use super::prelude::*;
use crate::ast_util::{string_literal_span, template_element_span};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-escaped-irregular-whitespace",
        "Prefer escape sequences for irregular whitespace characters.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferEscapedIrregularWhitespace>,
};

#[derive(Debug, Default)]
struct PreferEscapedIrregularWhitespace;

fn is_irregular(c: char) -> bool {
    matches!(
        c,
        '\u{b}' | '\u{c}' | '\u{85}' | '\u{a0}' | '\u{1680}' | '\u{180e}' | '\u{2000}'
            ..='\u{200b}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// `raw` with its irregular whitespace escaped, or `None` when nothing changes.
fn escaped(raw: &str) -> Option<String> {
    if !raw.chars().any(is_irregular) {
        return None;
    }
    let b = raw.as_bytes();
    let mut out = String::with_capacity(raw.len() + 8);
    for (i, c) in raw.char_indices() {
        if !is_irregular(c) {
            out.push(c);
            continue;
        }
        let behind_backslash = b[..i].iter().rev().take_while(|&&x| x == b'\\').count() % 2 == 1;
        if behind_backslash && matches!(c, '\u{2028}' | '\u{2029}') {
            out.push(c);
            continue;
        }
        if behind_backslash {
            out.pop();
        }
        out.push_str(&format!("\\u{{{:X}}}", c as u32));
    }
    (out != raw).then_some(out)
}

impl Rule for PreferEscapedIrregularWhitespace {
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
        let (report, range, fixable) = match *cx.node(node) {
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
            Node::RegexLit { flags, .. } => {
                let f = ast.atom(flags);
                (
                    cx.span(node),
                    cx.span(node),
                    f.contains('u') || f.contains('v'),
                )
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
                (template_element_span(src, raw), raw, true)
            }
            _ => return,
        };
        let Some(fixed) = escaped(range.text(src)) else {
            return;
        };
        let fix = fixable.then(|| Fix::replace(range, fixed));
        cx.report(
            report,
            "Prefer escape sequences for irregular whitespace characters.",
        )
        .id("prefer-escaped-irregular-whitespace")
        .fix_opt(fix);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes() {
        assert_eq!(escaped("'a\u{a0}'").as_deref(), Some("'a\\u{A0}'"));
        assert_eq!(escaped("'\\\u{2028}'"), None);
        assert_eq!(escaped("'\\\u{a0}'").as_deref(), Some("'\\u{A0}'"));
        assert_eq!(escaped("'abc'"), None);
    }
}
