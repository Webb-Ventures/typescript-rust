//! `no-useless-escape`: disallow unnecessary escape characters.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-useless-escape",
        "Disallow unnecessary escape characters",
        ESLINT_REC,
        Category::Style,
    )
    .suggestions(),
    create: create::<NoUselessEscape>,
};

#[derive(Debug, Default)]
struct NoUselessEscape {
    allow_regex: Vec<char>,
}

impl FromOptions for NoUselessEscape {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoUselessEscape {
            allow_regex: str_list(obj_opt(opts, 0), "allowRegexCharacters")
                .iter()
                .filter_map(|s| s.chars().next())
                .collect(),
        })
    }
}

/// Escapes with a meaning everywhere in a regex.
const REGEX_GENERAL: &str = "\\bcdDfnpPrsStvwWxu0123456789]";
/// Escapes with a meaning outside character classes.
const REGEX_OUTSIDE: &str = "^/.$*+?[{}|()Bk";
/// `v`-mode class syntax characters.
const CLASS_SET_SYNTAX: &str = "()[]{}/-\\|";
/// `v`-mode reserved double punctuators.
const DOUBLE_PUNCT: &str = "&!#$%*+,.:;<=>?@^`~";

fn report(cx: &mut Ctx<'_, '_>, pos: u32, c: char, keep_semantics: bool, backslash: bool) {
    let remove = if keep_semantics {
        (
            "removeEscape",
            "Remove the `\\`. This maintains the current functionality.",
        )
    } else {
        (
            "removeEscapeDoNotKeepSemantics",
            "Remove the `\\` if it was inserted by mistake.",
        )
    };
    let r = cx
        .report(
            Span::new(pos, pos + 1),
            format!("Unnecessary escape character: \\{c}."),
        )
        .id("unnecessaryEscape")
        .suggest(remove.0, remove.1, Fix::delete(Span::new(pos, pos + 1)));
    if backslash {
        let _ = r.suggest(
            "escapeBackslash",
            "Replace the `\\` with `\\\\` to include the actual backslash character.",
            Fix::insert(pos, "\\"),
        );
    }
}

/// Does the `v` class starting at char index `start` (after its `[`) use `--` or `&&` at
/// its own level?
fn has_set_op(chars: &[(usize, char)], start: usize) -> bool {
    let mut depth = 0u32;
    let mut k = start;
    while k < chars.len() {
        match chars[k].1 {
            '\\' => k += 1,
            '[' => depth += 1,
            ']' if depth == 0 => return false,
            ']' => depth -= 1,
            c @ ('-' | '&') if depth == 0 && chars.get(k + 1).map(|x| x.1) == Some(c) => {
                return true;
            }
            _ => {}
        }
        k += 1;
    }
    false
}

/// Checks the escapes of string or template text starting at source offset `base`.
fn check_text(
    cx: &mut Ctx<'_, '_>,
    base: u32,
    text: &str,
    quote: Option<char>,
    template: bool,
    keep_semantics: bool,
) {
    let mut found = Vec::new();
    let mut it = text.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if c != '\\' {
            continue;
        }
        let Some((j, e)) = it.next() else { break };
        let next = text[j + e.len_utf8()..].chars().next();
        let valid = "\\nrvtbfux\n\r\u{2028}\u{2029}".contains(e)
            || e.is_ascii_digit()
            || Some(e) == quote
            || (template
                && (e == '`'
                    || (e == '$' && next == Some('{'))
                    || (e == '{' && text[..i].ends_with('$'))));
        if !valid {
            found.push((base + i as u32, e));
        }
    }
    for (pos, e) in found {
        report(cx, pos, e, keep_semantics, true);
    }
}

impl NoUselessEscape {
    fn check_regex(&self, cx: &mut Ctx<'_, '_>, base: u32, pattern: &str, v: bool) {
        let chars: Vec<(usize, char)> = pattern.char_indices().collect();
        let mut found = Vec::new();
        // Starts (char index after `[`) of the open classes.
        let mut classes: Vec<usize> = Vec::new();
        let mut k = 0;
        while k < chars.len() {
            let (_, c) = chars[k];
            if c == '\\' {
                let Some(&(_, e)) = chars.get(k + 1) else {
                    break;
                };
                let valid = if classes.is_empty() {
                    REGEX_GENERAL.contains(e) || REGEX_OUTSIDE.contains(e)
                } else {
                    let start = *classes.last().unwrap_or(&0);
                    let at_start = k == start;
                    let mut ok = REGEX_GENERAL.contains(e) || (e == '^' && at_start);
                    if v {
                        let negation = start > 0 && k == start + 1 && chars[start].1 == '^';
                        let prev = (k > start && !negation).then(|| chars[k - 1].1);
                        let after = chars.get(k + 2).map(|x| x.1);
                        ok = ok
                            || CLASS_SET_SYNTAX.contains(e)
                            || e == 'q'
                            || (DOUBLE_PUNCT.contains(e) && (prev == Some(e) || after == Some(e)));
                    } else if e == '-' {
                        ok = !at_start && chars.get(k + 2).map(|x| x.1) != Some(']');
                    }
                    ok
                };
                if !valid && !self.allow_regex.contains(&e) {
                    let backslash = !(v && classes.last().is_some_and(|&s| has_set_op(&chars, s)));
                    found.push((base + chars[k].0 as u32, e, backslash));
                }
                k += 2;
                continue;
            }
            if c == '[' && (classes.is_empty() || v) {
                classes.push(k + 1);
            } else if c == ']' && !classes.is_empty() {
                classes.pop();
            }
            k += 1;
        }
        for (pos, e, backslash) in found {
            report(cx, pos, e, true, backslash);
        }
    }
}

impl Rule for NoUselessEscape {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::StringLit,
            NodeKind::Directive,
            NodeKind::TemplateLit,
            NodeKind::RegexLit,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let span = cx.span(node);
        match *ast.node(node) {
            Node::StringLit { .. } | Node::Directive { .. } => {
                if cx
                    .parent(node)
                    .is_some_and(|p| matches!(ast.node(p), Node::JsxAttr { .. }))
                {
                    return;
                }
                let raw = cx.text(node);
                let Some(q) = raw.chars().next().filter(|q| *q == '"' || *q == '\'') else {
                    return;
                };
                if raw.len() < 2 {
                    return;
                }
                let directive = matches!(ast.node(node), Node::Directive { .. });
                check_text(
                    cx,
                    span.start + 1,
                    &raw[1..raw.len() - 1],
                    Some(q),
                    false,
                    !directive,
                );
            }
            Node::TemplateLit { parts } => {
                if cx.parent(node).is_some_and(
                    |p| matches!(*ast.node(p), Node::TaggedTemplate { quasi, .. } if quasi == node),
                ) {
                    return;
                }
                for &p in ast.list(parts) {
                    if matches!(ast.node(p), Node::TemplateElement { .. }) {
                        let s = cx.span(p);
                        let text = cx.text(p);
                        check_text(cx, s.start, text, None, true, true);
                    }
                }
            }
            Node::RegexLit { .. } => {
                let Some((pattern, flags)) = regex_parts(ast, node) else {
                    return;
                };
                self.check_regex(cx, span.start + 1, pattern, flags.contains('v'));
            }
            _ => {}
        }
    }
}
