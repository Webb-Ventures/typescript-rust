//! `unicorn/no-incorrect-template-string-interpolation`: in an untagged template literal,
//! raw text that looks like a botched `${...}` around a name or dotted name:
//! - `{name}` missing its `$` (not after `$`, `\`, `{` or `\u`, not followed by another
//!   `}`, and not where its line so far is `import` / `export` / `const` / `let` / `var`
//!   followed by no `;`, `=`, `{` or `}`, i.e. a specifier list or destructuring);
//! - `$name}` missing its `{` (not after `\` or `{`, not followed by another `}`).
//!
//! Matches inside a closed `/* ... */` comment of the template text (with its `${}`
//! substitutions blanked out) are skipped. Each match is reported at its own range, in text
//! order, with a suggestion to write `${name}`.

use super::prelude::*;
use ts_r_span::Span;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-incorrect-template-string-interpolation",
        "Disallow incorrect template literal interpolation syntax.",
        NO_PRESET,
        Category::Suspicious,
    )
    .suggestions(),
    create: create_default::<NoIncorrectTemplateStringInterpolation>,
};

#[derive(Debug, Default)]
struct NoIncorrectTemplateStringInterpolation;

fn is_ident_start(b: u8) -> bool {
    b == b'$' || b == b'_' || b.is_ascii_alphabetic()
}

fn is_ident_part(b: u8) -> bool {
    b == b'$' || b == b'_' || b.is_ascii_alphanumeric()
}

/// The end of a dotted name starting at `at`, if one starts there.
fn dotted_name_end(b: &[u8], at: usize) -> Option<usize> {
    let mut i = at;
    if !b.get(i).copied().is_some_and(is_ident_start) {
        return None;
    }
    loop {
        i += 1;
        while b.get(i).copied().is_some_and(is_ident_part) {
            i += 1;
        }
        if b.get(i) == Some(&b'.') && b.get(i + 1).copied().is_some_and(is_ident_start) {
            i += 1;
            continue;
        }
        return Some(i);
    }
}

/// A match: start, end (after the `}`) and the name's range.
struct Hit {
    start: usize,
    end: usize,
    name: (usize, usize),
}

/// The `{` / `$` at `i` opens `name}` with no further `}` after it.
fn closes(b: &[u8], i: usize) -> Option<(usize, usize)> {
    let end = dotted_name_end(b, i + 1)?;
    (b.get(end) == Some(&b'}') && b.get(end + 1) != Some(&b'}')).then_some((i + 1, end))
}

fn is_binding_declaration(raw: &str, index: usize) -> bool {
    let line_start = raw[..index].rfind('\n').map_or(0, |i| i + 1);
    let line = raw[line_start..index].trim_start_matches(|c: char| c.is_whitespace());
    let Some(rest) = ["import", "export", "const", "let", "var"]
        .iter()
        .find_map(|k| line.strip_prefix(k))
    else {
        return false;
    };
    let boundary = !rest.bytes().next().is_some_and(is_word);
    boundary && !rest.contains([';', '=', '{', '}'])
}

fn is_word(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphanumeric()
}

fn hits(raw: &str) -> Vec<Hit> {
    let b = raw.as_bytes();
    let mut out = Vec::new();
    let prev = |i: usize, k: usize| if i >= k { Some(b[i - k]) } else { None };
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'{'
            && !matches!(prev(i, 1), Some(b'$' | b'\\' | b'{'))
            && !(prev(i, 1) == Some(b'u') && prev(i, 2) == Some(b'\\'))
            && let Some(name) = closes(b, i)
            && !is_binding_declaration(raw, i)
        {
            out.push(Hit {
                start: i,
                end: name.1 + 1,
                name,
            });
            i = name.1 + 1;
            continue;
        }
        i += 1;
    }
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'$'
            && !matches!(prev(i, 1), Some(b'\\' | b'{'))
            && let Some(name) = closes(b, i)
        {
            out.push(Hit {
                start: i,
                end: name.1 + 1,
                name,
            });
            i = name.1 + 1;
            continue;
        }
        i += 1;
    }
    out
}

/// Closed `/* ... */` ranges in `text`.
fn block_comments(text: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < text.len() {
        if text[i] == b'/' && text[i + 1] == b'*' {
            let Some(close) = text[i + 2..].windows(2).position(|w| w == b"*/") else {
                break;
            };
            let end = i + 2 + close + 2;
            out.push((i, end));
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

impl Rule for NoIncorrectTemplateStringInterpolation {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::TemplateLit]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::TemplateLit { parts } = *cx.node(node) else {
            return;
        };
        if cx
            .parent(node)
            .is_some_and(|p| matches!(ast.node(p), Node::TaggedTemplate { .. }))
        {
            return;
        }
        let src = cx.src();
        let span = cx.span(node);
        let base = span.start as usize + 1;
        let end = (span.end as usize).saturating_sub(1).max(base);
        let quasis: Vec<Span> = ast
            .list(parts)
            .iter()
            .filter(|&&p| matches!(ast.node(p), Node::TemplateElement { .. }))
            .map(|&p| cx.span(p))
            .collect();
        let mut masked = src.as_bytes()[base..end].to_vec();
        for w in quasis.windows(2) {
            let (from, to) = (w[0].end as usize - base, w[1].start as usize - base);
            for b in masked.get_mut(from..to).into_iter().flatten() {
                *b = b' ';
            }
        }
        let comments = block_comments(&masked);
        let mut reports = Vec::new();
        for q in &quasis {
            let (qs, qe) = (q.start as usize, q.end as usize);
            let raw = &src[qs..qe];
            let offset = qs - base;
            let mut found: Vec<Hit> = hits(raw)
                .into_iter()
                .filter(|h| {
                    !comments
                        .iter()
                        .any(|&(s, e)| offset + h.start >= s && offset + h.start < e)
                })
                .collect();
            found.sort_by_key(|h| h.start);
            for h in found {
                let name = &raw[h.name.0..h.name.1];
                let incorrect = &raw[h.start..h.end];
                let correct = format!("${{{name}}}");
                reports.push((
                    Span::new((qs + h.start) as u32, (qs + h.end) as u32),
                    incorrect.to_owned(),
                    correct,
                ));
            }
        }
        for (span, incorrect, correct) in reports {
            cx.report(
                span,
                format!("Use `{correct}` for template literal interpolation."),
            )
            .id("no-incorrect-template-string-interpolation")
            .suggest(
                "no-incorrect-template-string-interpolation/suggestion",
                format!("Replace `{incorrect}` with `{correct}`."),
                Fix::replace(span, correct),
            );
        }
    }
}
