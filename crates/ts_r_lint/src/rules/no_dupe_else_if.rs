//! `no-dupe-else-if`: disallow duplicate conditions in if-else-if chains.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-dupe-else-if",
        "Disallow duplicate conditions in if-else-if chains",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoDupeElseIf>,
};

#[derive(Debug, Default)]
struct NoDupeElseIf;

/// Disjunctive normal form: `a || b` -> [[a], [b]]; `(a || b) && c` -> [[a, c], [b, c]].
fn or_of_ands(ast: &ts_r_ast::Ast, id: NodeId) -> Vec<Vec<NodeId>> {
    const LIMIT: usize = 256;
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::Binary {
            left,
            op: BinaryOp::LogicalOr,
            right,
        } => {
            let mut v = or_of_ands(ast, left);
            v.extend(or_of_ands(ast, right));
            v
        }
        Node::Binary {
            left,
            op: BinaryOp::LogicalAnd,
            right,
        } => {
            let l = or_of_ands(ast, left);
            let r = or_of_ands(ast, right);
            if l.len() * r.len() > LIMIT {
                return vec![vec![id]];
            }
            let mut out = Vec::with_capacity(l.len() * r.len());
            for a in &l {
                for b in &r {
                    let mut t = a.clone();
                    t.extend(b.iter().copied());
                    out.push(t);
                }
            }
            out
        }
        _ => vec![vec![id]],
    }
}

impl Rule for NoDupeElseIf {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::If]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let Node::If { test, .. } = *cx.node(node) else {
            return;
        };
        // Only `else if` branches: the parent is an `if` whose `alt` is this node.
        let mut prev_tests = Vec::new();
        let mut cur = node;
        while let Some(p) = cx.parent(cur) {
            match *ast.node(p) {
                Node::If {
                    test: t,
                    alt: Some(a),
                    ..
                } if a == cur => {
                    prev_tests.push(t);
                    cur = p;
                }
                _ => break,
            }
        }
        if prev_tests.is_empty() {
            return;
        }
        let eq = |a: NodeId, b: NodeId| same_tokens(ast, src, a, b);
        let subset = |a: &[NodeId], b: &[NodeId]| a.iter().all(|x| b.iter().any(|y| eq(*x, *y)));
        let mut remaining = or_of_ands(ast, test);
        for pt in prev_tests {
            let prev = or_of_ands(ast, pt);
            remaining.retain(|cur_and| !prev.iter().any(|prev_and| subset(prev_and, cur_and)));
            if remaining.is_empty() {
                cx.report_node(
                    skip_parens(ast, test),
                    "This branch can never execute. Its condition is a duplicate or covered by previous conditions in the if-else-if chain.",
                )
                .id("unexpected");
                return;
            }
        }
    }
}
