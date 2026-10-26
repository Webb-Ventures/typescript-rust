//! `unicorn/no-useless-else`: an `else` after a consequent that always exits (see
//! `unicorn_conditions::always_exits`) is reported on the `else` keyword, for `if`s
//! directly in a statement list that aren't dead code.
//!
//! The fix moves the alternate's statements after the `if` (unwrapped and reindented),
//! unless the alternate declares a block-scoped name, holds a multiline string, template
//! or JSX attribute, would merge with the code around it without a semicolon, is followed
//! on its line by more code or a comment, or a comment sits between the branches.

use super::prelude::*;
use super::unicorn_conditions::{
    always_exits, branch_statements, is_block_scoped_declaration, unwrapped_branch_text,
};
use super::unicorn_text::needs_semicolon;
use super::unicorn_util_h4::linebreak;
use ts_r_ast::Ast;
use ts_r_ast::visit::{Visitor, Walk, walk};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-useless-else",
        "Disallow `else` after a statement that exits.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<NoUselessElse>,
};

#[derive(Debug, Default)]
struct NoUselessElse;

impl FromOptions for NoUselessElse {
    fn from_options(_opts: &[Value]) -> Result<Self, String> {
        Ok(NoUselessElse)
    }
}

/// The statements of the list `stmt` sits in directly, if any.
fn statement_list(ast: &Ast, parent: NodeId) -> Option<&[NodeId]> {
    match *ast.node(parent) {
        Node::Program { stmts } | Node::Block { stmts } => Some(ast.list(stmts)),
        Node::StaticBlock { body } | Node::SwitchCase { body, .. } => Some(ast.list(body)),
        _ => None,
    }
}

/// A string, template piece or JSX attribute string under `root` spans lines (JSX child
/// text is fine: JSX trims line-leading whitespace).
fn has_reindent_unsafe_token(cx: &Ctx<'_, '_>, root: NodeId) -> bool {
    struct Find<'s> {
        src: &'s str,
        found: bool,
    }
    impl Visitor for Find<'_> {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
            if matches!(
                ast.node(id),
                Node::StringLit { .. } | Node::TemplateElement { .. }
            ) && ast
                .span(id)
                .text(self.src)
                .contains(['\n', '\r', '\u{2028}', '\u{2029}'])
            {
                self.found = true;
                return Walk::Stop;
            }
            Walk::Continue
        }
    }
    let mut f = Find {
        src: cx.src(),
        found: false,
    };
    walk(cx.ast(), root, &mut f);
    f.found
}

fn starts_with_asi_hazard(text: &str) -> bool {
    text.starts_with(['[', '(', '<', '/', '`', '+', '-'])
}

fn is_safe_to_move_alternate(cx: &Ctx<'_, '_>, cons: NodeId, alt: NodeId) -> bool {
    let src = cx.src();
    let ast = cx.ast();
    let alt_span = cx.span(alt);
    let alt_block = matches!(ast.node(alt), Node::Block { .. });
    let first_alt = if alt_block {
        skip_trivia_forward(src, alt_span.start as usize + 1)
    } else {
        alt_span.start as usize
    };
    let first_alt_text = &src[first_alt..];
    let cons_span = cx.span(cons);
    if !matches!(ast.node(cons), Node::Block { .. })
        && !src[..cons_span.end as usize].ends_with(';')
        && (needs_semicolon(cx, cons_span.end, first_alt_text)
            || starts_with_asi_hazard(first_alt_text))
    {
        return false;
    }
    // Code or a comment after the alternate on its last line.
    let after = &src[alt_span.end as usize..];
    let rest_of_line = after
        .find(['\n', '\r', '\u{2028}', '\u{2029}'])
        .map_or(after, |i| &after[..i]);
    if !rest_of_line.trim().is_empty() {
        return false;
    }
    let last_alt_end = if alt_block {
        skip_trivia_backward(ast, src, alt_span.end as usize - 1)
    } else {
        alt_span.end as usize
    };
    let next = skip_trivia_forward(src, alt_span.end as usize);
    if last_alt_end == 0 || src[..last_alt_end].ends_with(';') || next >= src.len() {
        return true;
    }
    let next_text = &src[next..];
    !(needs_semicolon(cx, last_alt_end as u32, next_text) || starts_with_asi_hazard(next_text))
}

fn fix(cx: &Ctx<'_, '_>, if_stmt: NodeId, cons: NodeId, alt: NodeId) -> Option<Fix> {
    let ast = cx.ast();
    let direct_declaration = is_block_scoped_declaration(ast.node(alt))
        || (matches!(ast.node(alt), Node::Block { .. })
            && branch_statements(ast, alt)
                .iter()
                .any(|&s| is_block_scoped_declaration(ast.node(s))));
    if direct_declaration
        || (matches!(ast.node(alt), Node::Block { .. }) && has_reindent_unsafe_token(cx, alt))
        || !is_safe_to_move_alternate(cx, cons, alt)
    {
        return None;
    }
    let cons_span = cx.span(cons);
    let alt_span = cx.span(alt);
    if !ast
        .comments_in(Span::new(cons_span.end, alt_span.start))
        .is_empty()
    {
        return None;
    }
    let text = unwrapped_branch_text(cx, if_stmt, alt);
    let replacement = if text.is_empty() {
        String::new()
    } else {
        format!("{}{text}", linebreak(cx.src()))
    };
    Some(
        Fix::replace(cons_span, cx.text(cons))
            .and(Span::new(cons_span.end, alt_span.end), replacement),
    )
}

impl Rule for NoUselessElse {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::If]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::If {
            cons,
            alt: Some(alt),
            ..
        } = *ast.node(node)
        else {
            return;
        };
        let Some(list) = cx.parent(node).and_then(|p| statement_list(ast, p)) else {
            return;
        };
        let dead = list
            .iter()
            .take_while(|&&s| s != node)
            .any(|&s| always_exits(cx, s));
        if dead || !always_exits(cx, cons) {
            return;
        }
        let src = cx.src();
        let else_end = skip_trivia_backward(ast, src, cx.span(alt).start as usize) as u32;
        let span = Span::new(else_end.saturating_sub(4), else_end);
        let fix = fix(cx, node, cons, alt);
        cx.report(span, "Unexpected `else` after a statement that exits.")
            .id("no-useless-else")
            .fix_opt(fix);
    }
}
