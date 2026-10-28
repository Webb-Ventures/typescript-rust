//! `no-lonely-if`: disallow an `if` as the only statement of an `else` block.

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-lonely-if",
        "Disallow `if` statements as the only statement in `else` blocks",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoLonelyIf>,
};

#[derive(Debug, Default)]
struct NoLonelyIf;

/// Would an `else` written right after `stmt` attach to an `if` inside it?
fn has_unsafe_if(ast: &Ast, stmt: NodeId, depth: u32) -> bool {
    if depth > 500 {
        return true;
    }
    match *ast.node(stmt) {
        Node::If { alt: None, .. } => true,
        Node::If { alt: Some(a), .. } => has_unsafe_if(ast, a, depth + 1),
        Node::For { body, .. }
        | Node::ForIn { body, .. }
        | Node::ForOf { body, .. }
        | Node::Labeled { body, .. }
        | Node::With { body, .. }
        | Node::While { body, .. } => has_unsafe_if(ast, body, depth + 1),
        _ => false,
    }
}

/// Does the keyword `kw` start at `pos`?
fn keyword_at(src: &str, pos: usize, kw: &str) -> bool {
    src.get(pos..).is_some_and(|s| {
        s.starts_with(kw) && !s[kw.len()..].chars().next().is_some_and(is_ident_char)
    })
}

impl Rule for NoLonelyIf {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::If]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let Some(block) = cx.parent(node) else {
            return;
        };
        let Node::Block { stmts } = *ast.node(block) else {
            return;
        };
        if ast.list(stmts).len() != 1 {
            return;
        }
        let Some(outer) = cx.parent(block) else {
            return;
        };
        if !matches!(*ast.node(outer), Node::If { alt: Some(a), .. } if a == block) {
            return;
        }
        let bspan = cx.span(block);
        let after_block = skip_trivia_forward(src, bspan.end as usize);
        if has_unsafe_if(ast, node, 0) && keyword_at(src, after_block, "else") {
            return;
        }
        let Node::If { cons, .. } = *ast.node(node) else {
            return;
        };
        let span = cx.span(node);
        let fix = (|| {
            let open_end = bspan.start as usize + 1;
            let close = bspan.end as usize - 1;
            if !src.get(open_end..span.start as usize)?.trim().is_empty()
                || !src.get(span.end as usize..close)?.trim().is_empty()
            {
                return None;
            }
            let cspan = cx.span(cons);
            let ctext = cspan.text(src);
            if !matches!(ast.node(cons), Node::Block { .. })
                && !ctext.ends_with(';')
                && after_block < src.len()
            {
                let next = &src[after_block..];
                if !has_newline(src, cspan.end, after_block as u32)
                    || next.starts_with(['(', '[', '/', '+', '`', '-'])
                    || ctext.ends_with("++")
                    || ctext.ends_with("--")
                {
                    return None;
                }
            }
            let else_end = skip_trivia_backward(ast, src, bspan.start as usize);
            let space = if else_end == bspan.start as usize {
                " "
            } else {
                ""
            };
            Some(Fix::replace(bspan, format!("{space}{}", span.text(src))))
        })();
        cx.report(
            span,
            "Unexpected if as the only statement in an else block.",
        )
        .id("unexpectedLonelyIf")
        .fix_opt(fix);
    }
}
