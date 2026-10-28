//! `operator-assignment`: `x += y` instead of `x = x + y` (`always`), or the reverse
//! (`never`).

use super::prefer_exponentiation_operator::joins;
use super::prefer_object_spread::op_token;
use super::prelude::*;
use super::yoda::{is_literal, same_literal};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "operator-assignment",
        "Require or disallow assignment operator shorthand where possible",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<OperatorAssignment>,
};

#[derive(Debug, Default)]
struct OperatorAssignment {
    never: bool,
}

impl FromOptions for OperatorAssignment {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(OperatorAssignment {
            never: opts.first().and_then(Value::as_str) == Some("never"),
        })
    }
}

fn commutative(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Mul | BinaryOp::BitAnd | BinaryOp::BitXor | BinaryOp::BitOr
    )
}

fn has_shorthand(op: BinaryOp) -> bool {
    use BinaryOp::*;
    commutative(op) || matches!(op, Add | Sub | Div | Mod | Shl | Shr | UShr | Exp)
}

/// ESLint's `isSameReference` with static computed keys compared as written.
fn same_ref(ast: &Ast, a: NodeId, b: NodeId, depth: u32) -> bool {
    if depth > 64 {
        return false;
    }
    let (a, b) = (skip_parens(ast, a), skip_parens(ast, b));
    match (*ast.node(a), *ast.node(b)) {
        (Node::This {}, Node::This {}) | (Node::Super {}, Node::Super {}) => true,
        (Node::Ident { name: x }, Node::Ident { name: y })
        | (Node::PrivateName { name: x }, Node::PrivateName { name: y }) => {
            ast.atom(x) == ast.atom(y)
        }
        (
            Node::Member {
                obj: o1, prop: p1, ..
            },
            Node::Member {
                obj: o2, prop: p2, ..
            },
        )
        | (
            Node::Index {
                obj: o1, index: p1, ..
            },
            Node::Index {
                obj: o2, index: p2, ..
            },
        ) => same_ref(ast, o1, o2, depth + 1) && same_ref(ast, p1, p2, depth + 1),
        _ => is_literal(ast, a) && is_literal(ast, b) && same_literal(ast, a, b),
    }
}

/// An identifier, or `x.y` / `this.y` / `x[0]` (no optional chaining).
fn can_fix(ast: &Ast, id: NodeId) -> bool {
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::Ident { .. } => true,
        Node::Member {
            obj,
            optional: false,
            ..
        } => matches!(ast.node(obj), Node::Ident { .. } | Node::This {}),
        Node::Index {
            obj,
            optional: false,
            index,
        } => matches!(ast.node(obj), Node::Ident { .. } | Node::This {}) && is_literal(ast, index),
        _ => false,
    }
}

fn comments_between(ast: &Ast, from: u32, to: u32) -> bool {
    ast.comments
        .iter()
        .any(|c| c.span.start >= from && c.span.end <= to)
}

/// Our [`precedence`] of a binary operator.
fn op_precedence(op: BinaryOp) -> u8 {
    use BinaryOp::*;
    match op {
        BitOr => 6,
        BitXor => 7,
        BitAnd => 8,
        Shl | Shr | UShr => 11,
        Add | Sub => 12,
        Exp => 14,
        _ => 13,
    }
}

fn binary_of(op: AssignOp) -> Option<BinaryOp> {
    use AssignOp::*;
    Some(match op {
        AddAssign => BinaryOp::Add,
        SubAssign => BinaryOp::Sub,
        MulAssign => BinaryOp::Mul,
        DivAssign => BinaryOp::Div,
        ModAssign => BinaryOp::Mod,
        ExpAssign => BinaryOp::Exp,
        ShlAssign => BinaryOp::Shl,
        ShrAssign => BinaryOp::Shr,
        UShrAssign => BinaryOp::UShr,
        BitOrAssign => BinaryOp::BitOr,
        BitXorAssign => BinaryOp::BitXor,
        BitAndAssign => BinaryOp::BitAnd,
        _ => return None,
    })
}

impl OperatorAssignment {
    fn verify(&self, cx: &mut Ctx<'_, '_>, node: NodeId, left: NodeId, right: NodeId) {
        let ast = cx.ast();
        let src = cx.src();
        let expr = skip_parens(ast, right);
        let Node::Binary {
            left: el,
            op,
            right: er,
        } = *ast.node(expr)
        else {
            return;
        };
        if !has_shorthand(op) {
            return;
        }
        let replacement = format!("{}=", op.as_str());
        let msg =
            format!("Assignment (=) can be replaced with operator assignment ({replacement}).");
        if same_ref(ast, left, el, 0) {
            let span = cx.span(node);
            let fix = (can_fix(ast, left) && can_fix(ast, el))
                .then(|| {
                    let eq = op_token(ast, src, cx.span(left).end, cx.span(right).start, "=")?;
                    let opt = op_token(ast, src, cx.span(el).end, cx.span(er).start, op.as_str())?;
                    if comments_between(ast, eq.end, opt.start) {
                        return None;
                    }
                    let left_text = &src[span.start as usize..eq.start as usize];
                    let right_text = &src[opt.end as usize..cx.span(expr).end as usize];
                    Some(Fix::replace(
                        span,
                        format!("{left_text}{replacement}{right_text}"),
                    ))
                })
                .flatten();
            cx.report_node(node, msg).id("replaced").fix_opt(fix);
        } else if commutative(op) && same_ref(ast, left, er, 0) {
            cx.report_node(node, msg).id("replaced");
        }
    }

    fn prohibit(
        &self,
        cx: &mut Ctx<'_, '_>,
        node: NodeId,
        aop: AssignOp,
        left: NodeId,
        right: NodeId,
    ) {
        let Some(bop) = binary_of(aop) else {
            return;
        };
        let ast = cx.ast();
        let src = cx.src();
        let span = cx.span(node);
        let fix = can_fix(ast, left)
            .then(|| {
                let opt = op_token(
                    ast,
                    src,
                    cx.span(left).end,
                    cx.span(right).start,
                    aop.as_str(),
                )?;
                if comments_between(ast, span.start, opt.start) {
                    return None;
                }
                let left_text = &src[span.start as usize..opt.start as usize];
                let new_op = bop.as_str();
                let r = cx.span(right);
                let right_text = if precedence(ast, right) <= op_precedence(bop)
                    && !matches!(ast.node(right), Node::Paren { .. })
                {
                    format!(
                        "{}({})",
                        &src[opt.end as usize..r.start as usize],
                        cx.text(right)
                    )
                } else {
                    let rest = &src[opt.end as usize..span.end as usize];
                    let sep = if joins(new_op, rest) { " " } else { "" };
                    format!("{sep}{rest}")
                };
                Some(Fix::replace(
                    span,
                    format!("{left_text}= {left_text}{new_op}{right_text}"),
                ))
            })
            .flatten();
        cx.report_node(
            node,
            format!(
                "Unexpected operator assignment ({}) shorthand.",
                aop.as_str()
            ),
        )
        .id("unexpected")
        .fix_opt(fix);
    }
}

impl Rule for OperatorAssignment {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Assign]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Assign { op, left, right } = *cx.node(node) else {
            return;
        };
        if self.never {
            self.prohibit(cx, node, op, left, right);
        } else if op == AssignOp::Assign {
            self.verify(cx, node, left, right);
        }
    }
}
