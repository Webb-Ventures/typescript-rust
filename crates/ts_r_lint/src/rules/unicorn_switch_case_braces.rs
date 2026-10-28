//! `unicorn/switch-case-braces`: `case` clauses wrap their statements in one block
//! (`always`, the default), drop a block that isn't needed (`avoid`: no declarations in it),
//! or use braces only where a lexical declaration needs them (`single-statement`). An empty
//! `{}` clause is always reported.
//!
//! Fixes: adding braces puts ` {` after the colon and a line break, the clause's indent and
//! `}` after its last statement (and that line's trailing comment); none when a declaration
//! of the clause is referenced outside it. Removing braces deletes the `{` with the spaces
//! before it and everything from the block's last token (or last comment) to the `}`; none
//! when a comment follows the `}` on its line or a `//` comment would swallow code after it.

use super::prelude::*;
use super::unicorn_util_h4::{
    indent_before, linebreak, switch_case_head, trailing_comment_same_line,
};
use ts_r_ast::{Ast, CommentKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/switch-case-braces",
        "Enforce consistent brace style for `case` clauses.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<SwitchCaseBraces>,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Always,
    Avoid,
    SingleStatement,
}

#[derive(Debug)]
struct SwitchCaseBraces {
    mode: Mode,
}

impl FromOptions for SwitchCaseBraces {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mode = match opts.first().and_then(Value::as_str) {
            None | Some("always") => Mode::Always,
            Some("avoid") => Mode::Avoid,
            Some("single-statement") => Mode::SingleStatement,
            Some(o) => return Err(format!("unknown option `{o}`")),
        };
        Ok(SwitchCaseBraces { mode })
    }
}

/// A declaration that must not sit directly in a `case` clause.
fn needs_block(ast: &Ast, stmt: NodeId) -> bool {
    match ast.node(stmt) {
        Node::VarDecl { kind, .. } => *kind != VarKind::Var,
        n => matches!(
            n.kind(),
            NodeKind::ClassDecl
                | NodeKind::FunctionDecl
                | NodeKind::Enum
                | NodeKind::Interface
                | NodeKind::Module
                | NodeKind::TypeAlias
        ),
    }
}

/// Declarations that keep a block in `avoid` mode.
fn is_declaration(ast: &Ast, stmt: NodeId) -> bool {
    matches!(ast.node(stmt), Node::VarDecl { .. }) || needs_block(ast, stmt)
}

const MISSING: &str = "Missing braces in case clause.";
const UNNECESSARY: &str = "Unnecessary braces in case clause.";
const EMPTY: &str = "Unexpected braces in empty case clause.";

impl SwitchCaseBraces {
    fn report_missing(cx: &mut Ctx<'_, '_>, case: NodeId, fix: bool) {
        let head = switch_case_head(cx, case);
        let fix = fix.then(|| Self::add_braces(cx, case, head));
        cx.report(head, MISSING)
            .id("switch-case-braces/missing")
            .fix_opt(fix);
    }

    fn add_braces(cx: &Ctx<'_, '_>, case: NodeId, head: Span) -> Fix {
        let ast = cx.ast();
        let Node::SwitchCase { body, .. } = *ast.node(case) else {
            return Fix { edits: Vec::new() };
        };
        let last = *ast.list(body).last().unwrap_or(&case);
        let end = cx.span(last).end;
        let end = trailing_comment_same_line(cx, end).map_or(end, |c| c.end);
        let indent = indent_before(cx, cx.span(case).start);
        Fix::insert(head.end, " {").and(Span::at(end), format!("{}{indent}}}", linebreak(cx.src())))
    }

    /// The fix deleting the braces of the clause's only statement, a block.
    fn remove_braces(cx: &Ctx<'_, '_>, block: NodeId) -> Option<Fix> {
        let ast = cx.ast();
        let src = cx.src();
        let Node::Block { stmts } = *ast.node(block) else {
            return None;
        };
        let span = cx.span(block);
        let close = Span::new(span.end - 1, span.end);
        if trailing_comment_same_line(cx, close.end).is_some() {
            return None;
        }
        // `{` with the whitespace before it; a line break opening that run is kept.
        let before = &src[..span.start as usize];
        let trimmed = before.trim_end_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
        let ws = &before[trimmed.len()..];
        let keep = if ws.starts_with("\r\n") {
            "\r\n"
        } else if ws.starts_with('\n') {
            "\n"
        } else if ws.starts_with('\r') {
            "\r"
        } else {
            ""
        };
        let open = Span::new(trimmed.len() as u32, span.start + 1);
        // The block's last token: its last statement (and that line's trailing comment),
        // or a later comment, or the `{` of an empty block.
        let (mut last_end, mut last_is_line) = (span.start + 1, false);
        if let Some(&stmt) = ast.list(stmts).last() {
            last_end = cx.span(stmt).end;
            if let Some(c) = trailing_comment_same_line(cx, last_end) {
                last_end = c.end;
                last_is_line = src[c.start as usize..].starts_with("//");
            }
        }
        if let Some(c) = ast.comments_in(span).last()
            && c.span.start >= last_end
        {
            last_end = c.span.end;
            last_is_line = c.kind == CommentKind::Line;
        }
        if last_is_line {
            let next = skip_trivia_forward(src, close.end as usize);
            if next < src.len()
                && cx.file.lines.line_of(next as u32) == cx.file.lines.line_of(close.end)
            {
                return None;
            }
        }
        Some(Fix::replace(open, keep).and(Span::new(last_end, close.end), ""))
    }

    fn brace_report(cx: &mut Ctx<'_, '_>, block: NodeId, message: &'static str, id: &'static str) {
        let start = cx.span(block).start;
        let fix = Self::remove_braces(cx, block);
        cx.report(Span::new(start, start + 1), message)
            .id(id)
            .fix_opt(fix);
    }

    /// A declaration of the clause is referenced before or after its statements.
    fn declaration_used_outside(cx: &Ctx<'_, '_>, body: &[NodeId]) -> bool {
        let ast = cx.ast();
        let (Some(&first), Some(&last)) = (body.first(), body.last()) else {
            return false;
        };
        let (start, end) = (cx.span(first).start, cx.span(last).end);
        let sem = cx.semantic();
        let mut names = Vec::new();
        for &stmt in body {
            if !needs_block(ast, stmt) {
                continue;
            }
            names.clear();
            match *ast.node(stmt) {
                Node::VarDecl { decls, .. } => {
                    for &d in ast.list(decls) {
                        if let Node::VarDeclarator { name, .. } = *ast.node(d) {
                            binding_idents(ast, name, &mut names);
                        }
                    }
                }
                Node::ClassDecl { name: Some(n), .. }
                | Node::FunctionDecl { name: Some(n), .. } => names.push(n),
                Node::Enum { name, .. }
                | Node::Interface { name, .. }
                | Node::Module { name, .. }
                | Node::TypeAlias { name, .. } => names.push(name),
                _ => {}
            }
            for &n in &names {
                let Some(sym) = sem.symbol_of_decl(n) else {
                    continue;
                };
                if sem.symbol(sym).refs.iter().any(|&r| {
                    let p = cx.span(sem.reference(r).node).start;
                    p < start || p >= end
                }) {
                    return true;
                }
            }
        }
        false
    }
}

impl Rule for SwitchCaseBraces {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::SwitchCase]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::SwitchCase { body, .. } = *cx.node(node) else {
            return;
        };
        let stmts = ast.list(body);
        let Some(&first) = stmts.first() else {
            return;
        };
        let only_block = match (stmts.len(), ast.node(first)) {
            (1, Node::Block { stmts }) => Some(ast.list(*stmts)),
            _ => None,
        };
        if only_block.is_some_and(<[NodeId]>::is_empty) {
            Self::brace_report(cx, first, EMPTY, "switch-case-braces/empty");
            return;
        }
        match self.mode {
            Mode::SingleStatement => {
                if stmts.len() > 1 {
                    let fix = stmts.iter().all(|&s| !needs_block(ast, s));
                    Self::report_missing(cx, node, fix);
                } else if needs_block(ast, first) {
                    Self::report_missing(cx, node, false);
                } else if let Some(inner) = only_block
                    && inner.len() == 1
                    && !needs_block(ast, inner[0])
                {
                    Self::brace_report(cx, first, UNNECESSARY, "switch-case-braces/unnecessary");
                }
            }
            Mode::Always => {
                if only_block.is_none() {
                    let fix = !Self::declaration_used_outside(cx, stmts);
                    Self::report_missing(cx, node, fix);
                }
            }
            Mode::Avoid => {
                if let Some(inner) = only_block
                    && inner.iter().all(|&s| !is_declaration(ast, s))
                {
                    Self::brace_report(cx, first, UNNECESSARY, "switch-case-braces/unnecessary");
                }
            }
        }
    }
}
