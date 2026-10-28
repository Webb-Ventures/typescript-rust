//! `unicorn/switch-case-break-position`: a `case` holding exactly one non-empty block
//! followed by `break` / `continue` / `return` / `throw` should have that statement inside
//! the block. The statement is reported; `break` and `continue` get a fix that moves the
//! statement's text to a new line before the block's `}` (after any trailing comment there,
//! at the indentation of the block's last statement) and deletes it after the `}`. No fix
//! when a comment sits between `}` and the statement, the block is on one line, or the
//! statement has a trailing comment on its line.

use super::prelude::*;
use super::unicorn_ast::linebreak;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/switch-case-break-position",
        "Enforce consistent `break`/`return`/`continue`/`throw` position in `case` clauses.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<SwitchCaseBreakPosition>,
};

#[derive(Debug, Default)]
struct SwitchCaseBreakPosition;

impl Rule for SwitchCaseBreakPosition {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::SwitchCase]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::SwitchCase { body, .. } = *cx.node(node) else {
            return;
        };
        let &[block, last] = ast.list(body) else {
            return;
        };
        let keyword = match ast.node(last) {
            Node::Break { .. } => "break",
            Node::Continue { .. } => "continue",
            Node::Return { .. } => "return",
            Node::Throw { .. } => "throw",
            _ => return,
        };
        let Node::Block { stmts } = *ast.node(block) else {
            return;
        };
        let Some(&last_body) = ast.list(stmts).last() else {
            return;
        };
        let fix = matches!(keyword, "break" | "continue")
            .then(|| self.fix(cx, block, last, last_body))
            .flatten();
        cx.report_node(
            last,
            format!("Move `{keyword}` inside the block statement."),
        )
        .id("switch-case-break-position")
        .fix_opt(fix);
    }
}

impl SwitchCaseBreakPosition {
    fn fix(&self, cx: &Ctx<'_, '_>, block: NodeId, stmt: NodeId, last_body: NodeId) -> Option<Fix> {
        let ast = cx.ast();
        let src = cx.src();
        let lines = cx.file.lines;
        let (b, s) = (cx.span(block), cx.span(stmt));
        let comments = &ast.comments;
        if comments
            .iter()
            .any(|c| c.span.start >= b.end && c.span.end <= s.start)
        {
            return None;
        }
        if lines.line_of(b.start) == lines.line_of(b.end.saturating_sub(1)) {
            return None;
        }
        // A comment right after the statement (before the next token) on its last line.
        let next = skip_trivia_forward(src, s.end as usize) as u32;
        let end_line = lines.line_of(s.end.saturating_sub(1).max(s.start));
        if comments.iter().any(|c| {
            c.span.start >= s.end && c.span.end <= next && lines.line_of(c.span.start) == end_line
        }) {
            return None;
        }
        let start = cx.span(last_body).start;
        let line_start = lines.line_start(lines.line_of(start)).unwrap_or(start) as usize;
        let before = &src[line_start..start as usize];
        let indent = &before[before.trim_end().len()..];
        let brace = b.end - 1;
        let insert_at = src[..brace as usize].trim_end().len() as u32;
        let text = format!("{}{indent}{}", linebreak(src), s.text(src));
        Some(Fix::insert(insert_at, text).and(Span::new(b.end, s.end), ""))
    }
}
