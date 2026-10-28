//! `unicorn/no-empty-file`: a file with nothing but empty statements, empty blocks and
//! directive prologues (and comments). A `///` triple-slash comment keeps the file; with
//! `allowComments`, so does a comment that isn't an `eslint-disable`/`-enable` directive
//! when the file has no code and no hashbang. The report covers the whole file, or the
//! first `eslint-*` directive comment so directives apply to it.

use super::prelude::*;
use crate::diagnostic::Loc;
use crate::directives::InlineConfig;
use ts_r_ast::{Ast, CommentKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-empty-file",
        "Disallow empty files.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create::<NoEmptyFile>,
};

#[derive(Debug, Default)]
struct NoEmptyFile {
    allow_comments: bool,
}

impl FromOptions for NoEmptyFile {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoEmptyFile {
            allow_comments: opts
                .first()
                .and_then(|o| o.get("allowComments"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}

fn is_empty(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::Empty {} | Node::Directive { .. } => true,
        Node::Block { stmts } => ast.list(stmts).iter().all(|&s| is_empty(ast, s)),
        _ => false,
    }
}

impl Rule for NoEmptyFile {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let Node::Program { stmts } = *ast.node(ast.root) else {
            return;
        };
        let stmts = ast.list(stmts);
        if !stmts.iter().all(|&s| is_empty(ast, s)) {
            return;
        }
        let comments = &ast.comments;
        if comments
            .iter()
            .any(|c| c.kind == CommentKind::Line && c.span.text(src)[2..].starts_with('/'))
        {
            return;
        }
        let inline = InlineConfig::parse(ast, src, cx.file.lines);
        if self.allow_comments
            && stmts.is_empty()
            && comments.iter().all(|c| c.kind != CommentKind::Hashbang)
            && comments
                .iter()
                .any(|c| !inline.directives.iter().any(|d| d.comment == c.span))
        {
            return;
        }
        let (span, bom) = match inline.directives.first() {
            Some(d) => (d.comment, false),
            None => (Span::new(0, src.len() as u32), src.starts_with('\u{feff}')),
        };
        let end = cx.file.lines.line_col_utf16(src, span.end);
        let r = cx
            .report(span, "Empty files are not allowed.")
            .id("no-empty-file");
        // ESLint drops a byte order mark before computing columns.
        if bom {
            let end_column = if end.line == 0 { end.col } else { end.col + 1 };
            r.loc(Loc {
                line: 1,
                column: 1,
                end_line: end.line + 1,
                end_column,
            });
        }
    }
}
