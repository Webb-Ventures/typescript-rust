//! `no-extra-bind`: disallow `.bind()` on a function that does not use `this`.

use super::prelude::*;
use super::this_binding::{ThisScope, this_scope};
use ts_r_ast::{Ast, Visitor, Walk};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-extra-bind",
        "Disallow unnecessary calls to `.bind()`",
        NO_PRESET,
        Category::Suspicious,
    )
    .fixable(),
    create: create_default::<NoExtraBind>,
};

#[derive(Debug, Default)]
struct NoExtraBind;

#[derive(Default)]
struct ThisNodes(Vec<NodeId>);

impl Visitor for ThisNodes {
    fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
        if matches!(ast.node(id), Node::This {}) {
            self.0.push(id);
        }
        Walk::Continue
    }
}

/// The name of a static `bind` property: `.bind`, `['bind']` or `` [`bind`] ``.
fn is_bind_prop(ast: &Ast, src: &str, member: NodeId) -> Option<NodeId> {
    match *ast.node(member) {
        Node::Member { prop, .. } => (ident_name(ast, prop) == Some("bind")).then_some(prop),
        Node::Index { index, .. } => {
            let index = skip_parens(ast, index);
            (member_name(ast, src, member).as_deref() == Some("bind")).then_some(index)
        }
        _ => None,
    }
}

fn side_effect_free(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(skip_parens(ast, id)),
        Node::NullLit {}
            | Node::BoolLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::StringLit { .. }
            | Node::RegexLit { .. }
            | Node::Ident { .. }
            | Node::This {}
            | Node::FunctionExpr { .. }
    )
}

impl Rule for NoExtraBind {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::FunctionExpr, NodeKind::Arrow]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let parents = cx.file.parents;
        let Some(member) = parent_skip_parens(ast, parents, node) else {
            return;
        };
        let (Node::Member { obj, .. } | Node::Index { obj, .. }) = *ast.node(member) else {
            return;
        };
        if skip_parens(ast, obj) != node {
            return;
        }
        let Some(prop) = is_bind_prop(ast, src, member) else {
            return;
        };
        let Some(call) = parent_skip_parens(ast, parents, member) else {
            return;
        };
        let Node::Call { callee, args, .. } = *ast.node(call) else {
            return;
        };
        if skip_parens(ast, callee) != member {
            return;
        }
        let args = ast.list(args);
        if args.len() != 1 || matches!(ast.node(args[0]), Node::Spread { .. }) {
            return;
        }
        if matches!(ast.node(node), Node::FunctionExpr { .. }) {
            let mut this = ThisNodes::default();
            ts_r_ast::walk(ast, node, &mut this);
            if this
                .0
                .iter()
                .any(|&t| this_scope(cx, t) == ThisScope::Function(node))
            {
                return;
            }
        }
        let fix = (|| {
            if !side_effect_free(ast, args[0]) {
                return None;
            }
            let first = skip_trivia_forward(src, cx.span(obj).end as usize);
            let member_end = cx.span(member).end as usize;
            let args_start = skip_trivia_forward(src, cx.span(callee).end as usize);
            let end = cx.span(call).end as usize;
            let removed = Span::new(first as u32, end as u32);
            if ast
                .comments
                .iter()
                .any(|c| c.span.start >= removed.start && c.span.end <= removed.end)
            {
                return None;
            }
            let kept = src.get(member_end..args_start)?;
            Some(Fix::replace(removed, kept.to_owned()))
        })();
        cx.report(cx.span(prop), "The function binding is unnecessary.")
            .id("unexpected")
            .fix_opt(fix);
    }
}
