//! `unicorn/consistent-export-decorator-position`: where the decorators of an exported
//! class sit relative to `export` (`export default`): on the lines `above` it (the
//! default), on the same line `before` it, or `after` it.
//!
//! A decorator that starts after `export` is `after`; otherwise it is `before` when it ends
//! on the line of `export`, else `above`. The class's style is the shared style of all its
//! decorators (`mixed` when they differ). The report goes on the first decorator out of
//! place. The fix rewrites the head (from the first of the decorators and `export` to the
//! `class` keyword) when it holds nothing but the decorators, `export` and `default`, and
//! no comment outside a decorator.

use super::prelude::*;
use super::unicorn_ast::linebreak;
use super::unicorn_util::line_indent;
use crate::ast_util::skip_trivia_forward;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/consistent-export-decorator-position",
        "Enforce consistent decorator position on exported classes.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<ConsistentExportDecoratorPosition>,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Style {
    Above,
    Before,
    After,
    Mixed,
}

impl Style {
    fn name(self) -> &'static str {
        match self {
            Style::Above => "above",
            Style::Before => "before",
            Style::After => "after",
            Style::Mixed => "mixed",
        }
    }
}

#[derive(Debug)]
struct ConsistentExportDecoratorPosition {
    expected: Style,
}

impl FromOptions for ConsistentExportDecoratorPosition {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let expected = match opts.first().and_then(Value::as_str) {
            None | Some("above") => Style::Above,
            Some("before") => Style::Before,
            Some("after") => Style::After,
            Some(other) => return Err(format!("unknown position `{other}`")),
        };
        Ok(Self { expected })
    }
}

/// The position of the `class` keyword at or after `pos` (skipping trivia and words).
fn class_keyword(src: &str, mut pos: usize, end: usize) -> Option<usize> {
    for _ in 0..16 {
        pos = skip_trivia_forward(src, pos);
        if pos >= end {
            return None;
        }
        let word_len = src[pos..]
            .char_indices()
            .find(|&(_, c)| !is_ident_char(c))
            .map_or(src.len() - pos, |(i, _)| i);
        if &src[pos..pos + word_len] == "class" {
            return Some(pos);
        }
        pos += word_len.max(src[pos..].chars().next().map_or(1, char::len_utf8));
    }
    None
}

/// Every decorator in the class head, in source order. The parser keeps only one group on
/// the class when decorators sit both before and after `export` (`@a export @b class`), so
/// they are read from the nodes allocated before `scan_from` (the class's first child other
/// than a decorator, or the class): the head's nodes, which start at or after `stmt_start`
/// and end by `head_end`. Outermost only.
fn head_decorators(
    ast: &ts_r_ast::Ast,
    scan_from: NodeId,
    stmt_start: u32,
    head_end: u32,
) -> Vec<Span> {
    let mut found: Vec<Span> = Vec::new();
    let mut raw = scan_from.index() as u32;
    while raw > 1 {
        raw -= 1;
        let Some(id) = NodeId::from_raw(raw) else {
            break;
        };
        let s = ast.span(id);
        if s.start < stmt_start {
            break;
        }
        if ast.kind(id) == NodeKind::Decorator && s.end <= head_end {
            found.push(s);
        }
    }
    found.sort_by_key(|s| (s.start, std::cmp::Reverse(s.end)));
    let mut out: Vec<Span> = Vec::new();
    for s in found {
        if out.last().is_none_or(|p| s.start >= p.end) {
            out.push(s);
        }
    }
    out
}

impl Rule for ConsistentExportDecoratorPosition {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ClassDecl]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let Node::ClassDecl {
            decorators,
            modifiers,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        let decorators = ast.list(decorators);
        if !modifiers.contains(Modifiers::EXPORT) || decorators.is_empty() {
            return;
        }
        let span = cx.span(node);
        // Modifier keywords before the class's first child other than a decorator.
        let mut head_end = span.end;
        let mut scan_from = node;
        let mut found = false;
        ast.for_each_child(node, |c| {
            if !found && ast.kind(c) != NodeKind::Decorator {
                head_end = cx.span(c).start;
                scan_from = c;
                found = true;
            }
        });
        let decorator_spans = head_decorators(ast, scan_from, span.start, head_end);
        let Some(last_decorator_end) = decorator_spans.last().map(|s| s.end) else {
            return;
        };
        let tokens = ast.modifiers_in(Span::new(span.start, head_end.max(span.start)));
        let Some(export) = tokens.iter().find(|t| t.flag == Modifiers::EXPORT) else {
            return;
        };
        let default = tokens.iter().find(|t| t.flag == Modifiers::DEFAULT);
        let lines = cx.file.lines;
        let export_line = lines.line_of(export.span.start);
        let style_of = |s: Span| {
            if s.start > export.span.start {
                Style::After
            } else if lines.line_of(s.end) == export_line {
                Style::Before
            } else {
                Style::Above
            }
        };
        let first = style_of(decorator_spans[0]);
        let actual = if decorator_spans.iter().all(|&s| style_of(s) == first) {
            first
        } else {
            Style::Mixed
        };
        let expected = self.expected;
        if actual == expected {
            return;
        }
        let Some(&report) = decorator_spans.iter().find(|&&s| style_of(s) != expected) else {
            return;
        };
        let msg = format!(
            "Expected export decorators to be positioned `{}`, but found `{}`.",
            expected.name(),
            actual.name()
        );
        let tokens_end = tokens
            .iter()
            .map(|t| t.span.end)
            .max()
            .unwrap_or(span.start);
        let fix = class_keyword(
            src,
            last_decorator_end.max(tokens_end) as usize,
            span.end as usize,
        )
        .and_then(|class_pos| {
            let head = Span::new(
                decorator_spans[0].start.min(export.span.start),
                class_pos as u32,
            );
            let inside_decorator = |s: Span| {
                decorator_spans
                    .iter()
                    .any(|d| d.start <= s.start && s.end <= d.end)
            };
            let only_export_default = ast
                .modifiers_in(head)
                .iter()
                .all(|t| t.flag == Modifiers::EXPORT || t.flag == Modifiers::DEFAULT);
            let comments_ok = ast
                .comments_in(head)
                .iter()
                .all(|c| inside_decorator(c.span));
            if !only_export_default || !comments_ok {
                return None;
            }
            let export_text = match default {
                Some(d) => &src[export.span.start as usize..d.span.end as usize],
                None => "export",
            };
            let texts = |sep: &str| {
                decorator_spans
                    .iter()
                    .map(|s| &src[s.start as usize..s.end as usize])
                    .collect::<Vec<_>>()
                    .join(sep)
            };
            let indent = line_indent(src, export.span.start as usize);
            let lb = linebreak(src);
            let text = match expected {
                Style::Above => {
                    let sep = format!("{lb}{indent}");
                    format!("{}{sep}{export_text} ", texts(&sep))
                }
                Style::Before => format!("{} {export_text} ", texts(" ")),
                Style::After | Style::Mixed => format!("{export_text} {} ", texts(" ")),
            };
            Some(Fix::replace(head, text))
        });
        cx.report(report, msg)
            .id("consistent-export-decorator-position")
            .fix_opt(fix);
    }
}

#[cfg(test)]
mod tests {
    use super::class_keyword;

    #[test]
    fn class_keyword_is_bounded() {
        assert_eq!(class_keyword("export @a class X {}", 7, 20), Some(10));
        assert_eq!(class_keyword("@a export", 2, 9), None);
        assert_eq!(class_keyword("é é é", 0, 7), None);
    }
}
