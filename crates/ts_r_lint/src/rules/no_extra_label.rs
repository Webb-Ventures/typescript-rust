//! `no-extra-label`: a `break`/`continue` label that names the innermost loop or `switch`
//! anyway.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-extra-label",
        "Disallow unnecessary labels",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoExtraLabel>,
};

#[derive(Debug, Default)]
struct NoExtraLabel;

fn is_breakable(n: &Node) -> bool {
    matches!(
        n,
        Node::While { .. }
            | Node::DoWhile { .. }
            | Node::For { .. }
            | Node::ForIn { .. }
            | Node::ForOf { .. }
            | Node::Switch { .. }
    )
}

impl Rule for NoExtraLabel {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Break, NodeKind::Continue]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (Node::Break { label: Some(label) } | Node::Continue { label: Some(label) }) =
            *cx.node(node)
        else {
            return;
        };
        let name = ident_name(ast, label);
        let label_of = |s: NodeId| match cx.parent(s).map(|p| ast.node(p)) {
            Some(&Node::Labeled { label, .. }) => ident_name(ast, label),
            _ => None,
        };
        let mut cur = node;
        while let Some(p) = cx.parent(cur) {
            let n = ast.node(p);
            if matches!(
                n,
                Node::FunctionDecl { .. }
                    | Node::FunctionExpr { .. }
                    | Node::Arrow { .. }
                    | Node::StaticBlock { .. }
            ) {
                return;
            }
            if is_breakable(n) {
                if label_of(p).is_some() && label_of(p) == name {
                    let start = cx.span(node).start;
                    let kw = if matches!(cx.node(node), Node::Break { .. }) {
                        "break"
                    } else {
                        "continue"
                    };
                    let kw_end = start + kw.len() as u32;
                    let lspan = cx.span(label);
                    let fix = ast
                        .comments_in(Span::new(kw_end, lspan.start))
                        .is_empty()
                        .then(|| Fix::delete(Span::new(kw_end, lspan.end)));
                    let text = cx.text(label).to_owned();
                    cx.report(lspan, format!("This label '{text}' is unnecessary."))
                        .id("unexpected")
                        .fix_opt(fix);
                }
                return;
            }
            if let Node::Labeled { label: l, body } = *n
                && !is_breakable(ast.node(body))
                && ident_name(ast, l) == name
            {
                return;
            }
            cur = p;
        }
    }
}
