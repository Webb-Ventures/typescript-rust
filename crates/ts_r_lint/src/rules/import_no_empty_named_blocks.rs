//! `import/no-empty-named-blocks`: an empty `{}` in an import declaration. With a default
//! or namespace binding the block (and the `,` or `type` before it) is removed by a fix;
//! without one two suggestions are offered: remove the whole import, or keep it as a
//! side-effect import.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-empty-named-blocks",
        "Forbid empty named import blocks.",
        NO_PRESET,
        Category::Suspicious,
    )
    .fixable()
    .suggestions(),
    create: create_default::<NoEmptyNamedBlocks>,
};

#[derive(Debug, Default)]
struct NoEmptyNamedBlocks;

const MESSAGE: &str = "Unexpected empty named import block";

/// The identifier-like word ending at `end`.
fn word_before(src: &str, end: usize) -> &str {
    let start = src[..end]
        .char_indices()
        .rev()
        .take_while(|&(_, c)| is_ident_char(c))
        .last()
        .map_or(end, |(i, _)| i);
    &src[start..end]
}

impl Rule for NoEmptyNamedBlocks {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ImportDecl]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let Node::ImportDecl {
            has_clause: true,
            specifiers,
            source,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        let specifiers = ast.list(specifiers);
        if specifiers
            .iter()
            .any(|&s| ast.kind(s) == NodeKind::ImportSpecifier)
        {
            return;
        }
        let span = cx.span(node);
        let source_start = cx.span(source).start as usize;
        // The `{` of the clause, then the `}` right after it.
        let Some(open) = src[span.start as usize..source_start]
            .find('{')
            .map(|i| i + span.start as usize)
        else {
            return;
        };
        let close = skip_trivia_forward(src, open + 1);
        if src.as_bytes().get(close) != Some(&b'}') {
            return;
        }
        let close_end = close + 1;
        let prev_end = skip_trivia_backward(ast, src, open);
        let start = if src[..prev_end].ends_with(',') {
            prev_end - 1
        } else {
            match word_before(src, prev_end) {
                w @ ("type" | "typeof") => prev_end - w.len(),
                _ => open,
            }
        };
        if !specifiers.is_empty() {
            let fix = Fix::replace(Span::new(start as u32, close_end as u32), String::new());
            cx.report(span, MESSAGE).fix(fix);
            return;
        }
        // Keep `import 'x'`: drop everything from the block through `from` (and one space
        // after it), leaving one space when there was none after `import`.
        let from_start = skip_trivia_forward(src, close_end);
        if !src[from_start..].starts_with("from") {
            return;
        }
        let mut end = from_start + "from".len();
        let space_after_from = src[end..source_start]
            .chars()
            .next()
            .is_some_and(char::is_whitespace);
        if space_after_from {
            end += 1;
        }
        let import_end = span.start as usize + "import".len();
        let space_after_import = src[import_end..source_start]
            .chars()
            .any(char::is_whitespace);
        let text = if space_after_import { "" } else { " " };
        let keep = Fix::replace(Span::new(start as u32, end as u32), text.to_owned());
        cx.report(span, MESSAGE)
            .suggest(
                "remove",
                "Remove unused import",
                Fix::replace(span, String::new()),
            )
            .suggest("removeBlock", "Remove empty import block", keep);
    }
}
