//! `import/newline-after-import`: `count` (default 1) empty lines after the last of a
//! run of imports (`import` declarations and `import x = ...`) and after top-level
//! statements holding a static `require("x")` outside functions, blocks, object literals
//! and decorators. `exactCount` also reports more lines than `count` (without a fix);
//! `considerComments` measures to a comment within `count + 1` lines of the import
//! instead of the next statement (top-level imports only). A decorated class is measured
//! from its first decorator. The report sits at the import's last line, at its column when
//! it is one line and at column 1 otherwise; the fix adds the missing line breaks after it.

use super::import_util::string_literal;
use super::prelude::*;
use crate::diagnostic::Loc;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/newline-after-import",
        "Enforce a newline after import statements.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<NewlineAfterImport>,
};

#[derive(Debug)]
struct NewlineAfterImport {
    count: u32,
    exact_count: bool,
    consider_comments: bool,
}

impl FromOptions for NewlineAfterImport {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NewlineAfterImport {
            count: o
                .get("count")
                .and_then(Value::as_u64)
                .map_or(1, |c| c.min(10_000) as u32),
            exact_count: bool_field(o, "exactCount", false),
            consider_comments: bool_field(o, "considerComments", false),
        })
    }
}

fn is_import(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    match ast.node(id) {
        Node::ImportDecl { .. } => true,
        Node::ImportEquals { modifiers, .. } => !modifiers.contains(Modifiers::EXPORT),
        _ => false,
    }
}

fn is_static_require(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    let Node::Call { callee, args, .. } = *ast.node(id) else {
        return false;
    };
    ident_name(ast, callee) == Some("require")
        && matches!(ast.list(args), &[a] if ast.kind(a) == NodeKind::StringLit && string_literal(ast, a).is_some())
}

impl NewlineAfterImport {
    fn message(&self, kind: &str) -> String {
        let s = if self.count > 1 { "s" } else { "" };
        format!(
            "Expected {} empty line{s} after {kind} statement not followed by another {kind}.",
            self.count
        )
    }

    /// Reports `node` when the line gap to `next_start` is wrong.
    fn check_gap(
        &self,
        cx: &mut Ctx<'_, '_>,
        node: NodeId,
        next_start: u32,
        kind: &str,
        exact: bool,
    ) {
        let lines = cx.file.lines;
        let span = cx.span(node);
        let end_line = lines.line_of(span.end);
        let diff = i64::from(lines.line_of(next_start)) - i64::from(end_line);
        let expected = i64::from(self.count) + 1;
        if !(diff < expected || exact && self.exact_count && diff != expected) {
            return;
        }
        let start = lines.line_col_utf16(cx.src(), span.start);
        let column = if start.line == end_line { start.col } else { 0 };
        let fix = (!(self.exact_count && expected < diff)).then(|| {
            let n = usize::try_from(expected - diff).unwrap_or(0);
            Fix::insert(span.end, "\n".repeat(n))
        });
        let message = self.message(kind);
        cx.report(span, message)
            .loc(Loc::at(end_line + 1, column + 1))
            .fix_opt(fix);
    }

    /// The first comment starting within `count + 1` lines after line `from_line`.
    fn next_comment(&self, cx: &Ctx<'_, '_>, from_line: u32) -> Option<u32> {
        if !self.consider_comments {
            return None;
        }
        let lines = cx.file.lines;
        cx.ast().comments.iter().map(|c| c.span.start).find(|&s| {
            let l = lines.line_of(s);
            l >= from_line && l <= from_line + self.count + 1
        })
    }

    fn check_next(&self, cx: &mut Ctx<'_, '_>, node: NodeId, next: NodeId, kind: &str) {
        let ast = cx.ast();
        let mut start = cx.span(next).start;
        if let Node::ClassDecl { decorators, .. } = *ast.node(next)
            && let Some(&d) = ast.list(decorators).first()
        {
            start = cx.span(d).start;
        }
        self.check_gap(cx, node, start, kind, true);
    }
}

impl Rule for NewlineAfterImport {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ImportDecl, NodeKind::ImportEquals]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        if !is_import(ast, node) {
            return;
        }
        let Some(parent) = cx.parent(node) else {
            return;
        };
        let Some(body) = statement_list(ast, parent) else {
            return;
        };
        let Some(i) = body.iter().position(|&s| s == node) else {
            return;
        };
        let next = body.get(i + 1).copied();
        let comment = if ast.kind(parent) == NodeKind::Program {
            let end_line = cx.file.lines.line_of(cx.span(node).end);
            self.next_comment(cx, end_line)
        } else {
            None
        };
        if let Some(c) = comment {
            self.check_gap(cx, node, c, "import", false);
        } else if let Some(next) = next
            && !is_import(ast, next)
        {
            self.check_next(cx, node, next, "import");
        }
    }

    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Program { stmts } = *ast.node(ast.root) else {
            return;
        };
        let body = ast.list(stmts);
        // Static requires outside nested functions, blocks, objects and decorators, in
        // source order.
        let mut calls = Vec::new();
        let mut stack = vec![(ast.root, 0u32)];
        let mut kids = Vec::new();
        while let Some((id, level)) = stack.pop() {
            if level == 0 && is_static_require(ast, id) {
                calls.push(id);
            }
            let nested = matches!(
                ast.kind(id),
                NodeKind::FunctionDecl
                    | NodeKind::FunctionExpr
                    | NodeKind::Arrow
                    | NodeKind::Block
                    | NodeKind::ObjectLit
                    | NodeKind::Decorator
            );
            let level = level + u32::from(nested);
            kids.clear();
            ast.for_each_child(id, |c| kids.push(c));
            stack.extend(kids.iter().rev().map(|&c| (c, level)));
        }
        for (i, &call) in calls.iter().enumerate() {
            let at = cx.span(call);
            let Some(pos) = body.iter().position(|&s| cx.span(s).contains_span(at)) else {
                continue;
            };
            let stmt = body[pos];
            let next_call = calls.get(i + 1).map(|&c| cx.span(c));
            if next_call.is_some_and(|n| cx.span(stmt).contains_span(n)) {
                continue;
            }
            let Some(&next) = body.get(pos + 1) else {
                continue;
            };
            if next_call.is_some_and(|n| cx.span(next).contains_span(n)) {
                continue;
            }
            let end_line = cx.file.lines.line_of(at.end);
            if let Some(c) = self.next_comment(cx, end_line) {
                self.check_gap(cx, stmt, c, "require", false);
            } else {
                self.check_next(cx, stmt, next, "require");
            }
        }
    }
}
