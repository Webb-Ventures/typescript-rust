//! `unicorn/no-duplicate-if-branches`: in an `if` / `else if` / `else` chain (checked from
//! its head), a branch whose statements (empty statements ignored, at least one left) are
//! the same as the previous branch's is reported on its body. "The same" is equal tokens
//! and equal tree shape, ignoring comments, layout and a trailing `;`: a structural
//! comparison with literals compared by their raw text.

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-duplicate-if-branches",
        "Disallow duplicate adjacent branches in if chains.",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create_default::<NoDuplicateIfBranches>,
};

#[derive(Debug, Default)]
struct NoDuplicateIfBranches;

/// Upper bound on the branches of one chain.
const MAX_BRANCHES: usize = 10_000;

fn statements(ast: &Ast, body: NodeId) -> Vec<NodeId> {
    let all: &[NodeId] = match ast.node(body) {
        Node::Block { stmts } => ast.list(*stmts),
        _ => std::slice::from_ref(&body),
    };
    all.iter()
        .copied()
        .filter(|&s| !matches!(ast.node(s), Node::Empty {}))
        .collect()
}

impl Rule for NoDuplicateIfBranches {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::If]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        if let Some(p) = cx.parent(node)
            && let Node::If { alt: Some(alt), .. } = *ast.node(p)
            && alt == node
        {
            return;
        }
        let mut bodies = Vec::new();
        let mut cur = node;
        while let Node::If { cons, alt, .. } = *ast.node(cur) {
            bodies.push(cons);
            match alt {
                Some(a)
                    if matches!(ast.node(a), Node::If { .. }) && bodies.len() < MAX_BRANCHES =>
                {
                    cur = a;
                }
                Some(a) => {
                    bodies.push(a);
                    break;
                }
                None => break,
            }
        }
        let src = cx.src();
        for pair in bodies.windows(2) {
            let (prev, body) = (pair[0], pair[1]);
            let (a, b) = (statements(ast, prev), statements(ast, body));
            let same = !a.is_empty()
                && a.len() == b.len()
                && a.iter().zip(&b).all(|(&x, &y)| same_tokens(ast, src, x, y));
            if same {
                let line = cx.file.lines.line_of(cx.span(prev).start) + 1;
                cx.report_node(
                    body,
                    format!("This branch has the same body as the branch on line {line}."),
                )
                .id("no-duplicate-if-branches");
            }
        }
    }
}
