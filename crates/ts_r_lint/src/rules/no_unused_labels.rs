//! `no-unused-labels`: disallow unused labels.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-unused-labels",
        "Disallow unused labels",
        ESLINT_REC,
        Category::Correctness,
    )
    .fixable(),
    create: create_default::<NoUnusedLabels>,
};

#[derive(Debug, Default)]
struct NoUnusedLabels;

fn label_used(ast: &ts_r_ast::Ast, id: NodeId, name: &str, depth: u32) -> bool {
    if depth > 1000 {
        return true;
    }
    match *ast.node(id) {
        Node::Break { label: Some(l) } | Node::Continue { label: Some(l) } => {
            return ident_name(ast, l) == Some(name);
        }
        Node::FunctionDecl { .. }
        | Node::FunctionExpr { .. }
        | Node::Arrow { .. }
        | Node::ClassDecl { .. }
        | Node::ClassExpr { .. } => return false,
        _ => {}
    }
    let mut used = false;
    ast.for_each_child(id, |c| {
        if !used && label_used(ast, c, name, depth + 1) {
            used = true;
        }
    });
    used
}

impl Rule for NoUnusedLabels {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Labeled]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Labeled { label, body } = *cx.node(node) else {
            return;
        };
        let Some(name) = ident_name(ast, label) else {
            return;
        };
        if label_used(ast, body, name, 0) {
            return;
        }
        let span = cx.span(node);
        let body_start = ast.span(body).start;
        // Removing the label is safe unless comments sit between it and the body, or the
        // body is an expression statement that could become a directive.
        let comments = !ast
            .comments_in(Span::new(span.start, body_start))
            .is_empty();
        let directive_risk = match *ast.node(body) {
            Node::ExprStmt { expr } => {
                let first = cx
                    .src()
                    .as_bytes()
                    .get(ast.span(expr).start as usize)
                    .copied();
                matches!(first, Some(b'\'' | b'"' | b'`' | b'('))
                    && in_prologue(cx, outermost_label(cx, node))
            }
            _ => false,
        };
        let asi_risk = {
            let prev = skip_trivia_backward(ast, cx.src(), span.start as usize);
            let prev_char = cx.src()[..prev].chars().next_back();
            let first = cx.src()[body_start as usize..].chars().next();
            prev_char.is_some_and(|c| !matches!(c, ';' | '{' | ':'))
                && first.is_some_and(|c| matches!(c, '[' | '(' | '/' | '+' | '-' | '`'))
        };
        let fix = (!comments && !directive_risk && !asi_risk)
            .then(|| Fix::delete(Span::new(span.start, body_start)));
        cx.report_node(label, format!("'{name}:' is defined but never used."))
            .id("unused")
            .fix_opt(fix);
    }
}

/// True when `stmt` is in a directive prologue position (only directives before it in a
/// program or function body).
fn in_prologue(cx: &Ctx<'_, '_>, stmt: NodeId) -> bool {
    let ast = cx.ast();
    let Some(p) = cx.parent(stmt) else {
        return false;
    };
    let list = match *ast.node(p) {
        Node::Program { stmts } => ast.list(stmts),
        Node::Block { stmts } => {
            let fn_body = cx.parent(p).is_some_and(|g| is_function(ast.kind(g)));
            if !fn_body {
                return false;
            }
            ast.list(stmts)
        }
        _ => return false,
    };
    list.iter()
        .take_while(|&&s| s != stmt)
        .all(|&s| matches!(ast.node(s), Node::Directive { .. }))
}

fn outermost_label(cx: &Ctx<'_, '_>, mut stmt: NodeId) -> NodeId {
    while let Some(p) = cx.parent(stmt) {
        if cx.ast().kind(p) != NodeKind::Labeled {
            break;
        }
        stmt = p;
    }
    stmt
}
