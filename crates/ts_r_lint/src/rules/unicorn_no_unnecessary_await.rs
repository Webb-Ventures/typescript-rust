//! `unicorn/no-unnecessary-await`: `await` on a value that can't be a promise (array, arrow,
//! class or function expression, JSX, literal, template, binary/unary/update expression,
//! `await`, or a sequence ending in one; TS wrappers looked through) is reported on the
//! keyword.
//!
//! The fix removes `await` (and the whitespace after it) only when the `await` is the last
//! thing its function or module evaluates, since dropping it otherwise moves later code into
//! the same tick; never for function or class expressions (they would become
//! declarations). A `return`/`throw` argument that starts on a later line is parenthesized,
//! and a `;` is added when the remaining code could continue the previous statement.

use super::prelude::*;
use super::unicorn_ast::{needs_semicolon_before, paren_return_or_throw, skip_spaces};
use super::unicorn_util::with_edits;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unnecessary-await",
        "Disallow awaiting non-promise values.",
        NO_PRESET,
        Category::Correctness,
    )
    .fixable(),
    create: create_default::<NoUnnecessaryAwait>,
};

#[derive(Debug, Default)]
struct NoUnnecessaryAwait;

fn not_promise(ast: &Ast, id: NodeId, depth: u32) -> bool {
    match *ast.node(id) {
        Node::ArrayLit { .. }
        | Node::Arrow { .. }
        | Node::Await { .. }
        | Node::Binary { .. }
        | Node::ClassExpr { .. }
        | Node::FunctionExpr { .. }
        | Node::JsxElement { .. }
        | Node::JsxFragment { .. }
        | Node::StringLit { .. }
        | Node::NumberLit { .. }
        | Node::BigIntLit { .. }
        | Node::BoolLit { .. }
        | Node::NullLit {}
        | Node::RegexLit { .. }
        | Node::TemplateLit { .. }
        | Node::Unary { .. }
        | Node::Update { .. } => true,
        Node::Seq { exprs } => {
            depth < 64
                && ast
                    .list(exprs)
                    .last()
                    .is_some_and(|&e| not_promise(ast, skip_parens(ast, e), depth + 1))
        }
        _ => false,
    }
}

/// The parent of `id`, looking through parentheses.
fn parent_of(cx: &Ctx<'_, '_>, mut id: NodeId) -> Option<(NodeId, NodeId)> {
    loop {
        let p = cx.parent(id)?;
        if !matches!(cx.node(p), Node::Paren { .. }) {
            return Some((id, p));
        }
        id = p;
    }
}

/// Nothing of the enclosing function (or module) runs after `id`.
fn is_last_evaluated(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> bool {
    if depth > 256 {
        return false;
    }
    let ast = cx.ast();
    let Some((id, parent)) = parent_of(cx, id) else {
        return false;
    };
    let last = |list: &[NodeId]| list.last() == Some(&id);
    match *ast.node(parent) {
        Node::Arrow { body, .. } => body == id,
        Node::Program { stmts } => last(ast.list(stmts)),
        Node::Block { stmts } => {
            last(ast.list(stmts))
                && (cx.parent(parent).is_some_and(|f| is_function(ast.kind(f)))
                    || is_last_evaluated(cx, parent, depth + 1))
        }
        Node::If { test, .. } => test != id && is_last_evaluated(cx, parent, depth + 1),
        Node::VarDecl { decls, .. } => {
            last(ast.list(decls)) && is_last_evaluated(cx, parent, depth + 1)
        }
        Node::ExprStmt { .. }
        | Node::Return { .. }
        | Node::Throw { .. }
        | Node::VarDeclarator { .. } => is_last_evaluated(cx, parent, depth + 1),
        _ => false,
    }
}

impl Rule for NoUnnecessaryAwait {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Await]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Await { arg } = *cx.node(node) else {
            return;
        };
        let value = skip_ts_wrappers(ast, arg);
        if !not_promise(ast, value, 0) {
            return;
        }
        let start = cx.span(node).start;
        let kw_end = start + "await".len() as u32;
        let fix = (!matches!(
            ast.node(value),
            Node::FunctionExpr { .. } | Node::ClassExpr { .. }
        ) && is_last_evaluated(cx, node, 0))
        .then(|| {
            let src = cx.src();
            let mut edits = Vec::new();
            let parent = cx.parent(node);
            let parenthesized = parent.is_some_and(|p| matches!(cx.node(p), Node::Paren { .. }));
            if has_newline(src, kw_end, cx.span(skip_parens(ast, arg)).start)
                && !parenthesized
                && let Some(p) = parent
            {
                edits.extend(paren_return_or_throw(cx, p));
            }
            let end = skip_spaces(src, kw_end as usize) as u32;
            let next = skip_trivia_forward(src, kw_end as usize);
            if let Some(stmt) = parent
                && matches!(cx.node(stmt), Node::ExprStmt { .. })
                && cx.span(stmt).start == start
                && needs_semicolon_before(cx, stmt, src[next..].chars().next())
            {
                edits.push((Span::at(next as u32), ";".to_owned()));
            }
            with_edits(Fix::delete(Span::new(start, end)), edits)
        });
        cx.report(
            Span::new(start, kw_end),
            "Do not `await` non-promise value.",
        )
        .id("no-unnecessary-await")
        .fix_opt(fix);
    }
}
