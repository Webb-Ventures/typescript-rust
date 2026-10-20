//! `no-self-assign`: disallow assignments where both sides are exactly the same.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-self-assign",
        "Disallow assignments where both sides are exactly the same",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create::<NoSelfAssign>,
};

#[derive(Debug, Default)]
struct NoSelfAssign {
    props: bool,
}

impl FromOptions for NoSelfAssign {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoSelfAssign {
            props: bool_field(obj_opt(opts, 0), "props", true),
        })
    }
}

impl Rule for NoSelfAssign {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Assign]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Assign { left, op, right } = *cx.node(node) else {
            return;
        };
        if !matches!(
            op,
            AssignOp::Assign | AssignOp::AndAssign | AssignOp::OrAssign | AssignOp::CoalesceAssign
        ) {
            return;
        }
        let mut out = Vec::new();
        self.check(cx, left, right, &mut out);
        for r in out {
            let text = compact_text(cx.text(r));
            cx.report_node(r, format!("'{text}' is assigned to itself."))
                .id("selfAssignment");
        }
    }
}

impl NoSelfAssign {
    fn check(&self, cx: &Ctx<'_, '_>, left: NodeId, right: NodeId, out: &mut Vec<NodeId>) {
        let ast = cx.ast();
        let (l, r) = (skip_parens(ast, left), skip_parens(ast, right));
        match (ast.node(l), ast.node(r)) {
            (Node::Ident { name: a }, Node::Ident { name: b }) => {
                if a == b {
                    out.push(r);
                }
            }
            (Node::ArrayLit { elements: le }, Node::ArrayLit { elements: re }) => {
                let (le, re) = (ast.list(*le), ast.list(*re));
                for (i, &le_i) in le.iter().enumerate() {
                    let Some(&re_i) = re.get(i) else { break };
                    match (ast.node(le_i), ast.node(re_i)) {
                        (Node::Spread { arg: a }, Node::Spread { arg: b }) => {
                            if i == le.len() - 1 && i == re.len() - 1 {
                                self.check(cx, *a, *b, out);
                            }
                            break;
                        }
                        (Node::Spread { .. }, _) | (_, Node::Spread { .. }) => break,
                        (Node::Hole {}, _) | (_, Node::Hole {}) => {}
                        _ => self.check(cx, le_i, re_i, out),
                    }
                }
            }
            (Node::ObjectLit { props: lp }, Node::ObjectLit { props: rp }) => {
                let (lp, rp) = (ast.list(*lp), ast.list(*rp));
                // Only right properties after the last spread are certain.
                let start = rp
                    .iter()
                    .rposition(|&p| matches!(ast.node(p), Node::Spread { .. }))
                    .map_or(0, |i| i + 1);
                for &lprop in lp {
                    let Some((lk, lv)) = prop_parts(cx, lprop) else {
                        continue;
                    };
                    for &rprop in &rp[start..] {
                        let Some((rk, rv)) = prop_parts(cx, rprop) else {
                            continue;
                        };
                        if rk == lk {
                            if matches!(ast.node(lv), Node::Assign { .. }) {
                                continue;
                            }
                            self.check(cx, lv, rv, out);
                        }
                    }
                }
            }
            (
                Node::Member { .. } | Node::Index { .. },
                Node::Member { .. } | Node::Index { .. },
            ) => {
                if self.props && same_member(cx, l, r) {
                    out.push(r);
                }
            }
            _ => {}
        }
    }
}

/// `(static key, value node)` of an object literal property.
fn prop_parts(cx: &Ctx<'_, '_>, p: NodeId) -> Option<(String, NodeId)> {
    let ast = cx.ast();
    match *ast.node(p) {
        Node::Property { key, value, .. } => {
            if let Node::ComputedKey { expr } = *ast.node(key)
                && !matches!(
                    ast.node(skip_parens(ast, expr)),
                    Node::StringLit { .. } | Node::NumberLit { .. } | Node::TemplateLit { .. }
                )
            {
                return None;
            }
            Some((key_name(ast, cx.src(), key)?.into_owned(), value))
        }
        Node::ShorthandProp {
            name, init: None, ..
        } => Some((ident_name(ast, name)?.to_owned(), name)),
        _ => None,
    }
}

/// `a.b` vs `a.b`, `a[0]` vs `a[0]`, `this.x` vs `this.x`; never through calls.
fn same_member(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
    let ast = cx.ast();
    let src = cx.src();
    let (a, b) = (skip_parens(ast, a), skip_parens(ast, b));
    match (ast.node(a), ast.node(b)) {
        (Node::Ident { name: x }, Node::Ident { name: y }) => x == y,
        (Node::This {}, Node::This {}) | (Node::Super {}, Node::Super {}) => true,
        (
            Node::Member {
                obj: o1,
                prop: p1,
                optional: q1,
            },
            Node::Member {
                obj: o2,
                prop: p2,
                optional: q2,
            },
        ) => {
            let _ = (q1, q2);
            same_tree(ast, *p1, *p2) && same_member(cx, *o1, *o2)
        }
        (
            Node::Member { obj: o1, .. } | Node::Index { obj: o1, .. },
            Node::Member { obj: o2, .. } | Node::Index { obj: o2, .. },
        ) => {
            let private = |n: NodeId| matches!(ast.node(n), Node::Member { prop, .. } if matches!(ast.node(*prop), Node::PrivateName { .. }));
            if private(a) != private(b) {
                return false;
            }
            let (n1, n2) = (member_name(ast, src, a), member_name(ast, src, b));
            match (n1, n2) {
                (Some(x), Some(y)) => x == y && same_member(cx, *o1, *o2),
                _ => match (ast.node(a), ast.node(b)) {
                    (Node::Index { index: i1, .. }, Node::Index { index: i2, .. }) => {
                        let (i1, i2) = (skip_parens(ast, *i1), skip_parens(ast, *i2));
                        matches!(ast.node(i1), Node::Ident { .. })
                            && same_tree(ast, i1, i2)
                            && same_member(cx, *o1, *o2)
                    }
                    _ => false,
                },
            }
        }
        _ => false,
    }
}
