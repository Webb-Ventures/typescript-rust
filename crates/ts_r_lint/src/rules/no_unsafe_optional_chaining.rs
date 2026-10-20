//! `no-unsafe-optional-chaining`: disallow use of optional chaining in contexts where the
//! `undefined` value is not allowed.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-unsafe-optional-chaining",
        "Disallow use of optional chaining in contexts where the `undefined` value is not allowed",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create::<NoUnsafeOptionalChaining>,
};

#[derive(Debug, Default)]
struct NoUnsafeOptionalChaining {
    arithmetic: bool,
}

impl FromOptions for NoUnsafeOptionalChaining {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoUnsafeOptionalChaining {
            arithmetic: bool_field(obj_opt(opts, 0), "disallowArithmeticOperators", false),
        })
    }
}

const UNSAFE: &str = "Unsafe usage of optional chaining. If it short-circuits with 'undefined' the evaluation will throw TypeError.";
const ARITH: &str = "Unsafe arithmetic operation on optional chaining. It can result in NaN.";

fn is_chain(k: NodeKind) -> bool {
    matches!(
        k,
        NodeKind::Member | NodeKind::Index | NodeKind::Call | NodeKind::NonNull
    )
}

/// Reports every optional chain `e` can evaluate to.
fn check(cx: &mut Ctx<'_, '_>, e: NodeId, arith: bool) {
    let ast = cx.ast();
    let e = skip_parens(ast, e);
    match *ast.node(e) {
        Node::Member { .. } | Node::Index { .. } | Node::Call { .. } => {
            if chain_has_optional(ast, e) {
                let (msg, id) = if arith {
                    (ARITH, "unsafeArithmetic")
                } else {
                    (UNSAFE, "unsafeOptionalChain")
                };
                cx.report_node(e, msg).id(id);
            }
        }
        Node::Conditional { cons, alt, .. } => {
            check(cx, cons, arith);
            check(cx, alt, arith);
        }
        Node::Binary { left, op, right } => match op {
            BinaryOp::LogicalAnd => {
                check(cx, left, arith);
                check(cx, right, arith);
            }
            BinaryOp::LogicalOr | BinaryOp::Coalesce => check(cx, right, arith),
            _ => {}
        },
        Node::Seq { exprs } => {
            if let Some(&l) = ast.list(exprs).last() {
                check(cx, l, arith);
            }
        }
        Node::Await { arg } => check(cx, arg, arith),
        _ => {}
    }
}

/// For member/call contexts a chain continues unless it is parenthesized.
fn check_object(cx: &mut Ctx<'_, '_>, obj: NodeId) {
    if is_chain(cx.ast().kind(obj)) {
        return;
    }
    check(cx, obj, false);
}

fn check_pattern_target(cx: &mut Ctx<'_, '_>, pattern: NodeId, value: NodeId) {
    let ast = cx.ast();
    if matches!(
        ast.node(skip_parens(ast, pattern)),
        Node::ObjectPattern { .. }
            | Node::ArrayPattern { .. }
            | Node::ObjectLit { .. }
            | Node::ArrayLit { .. }
    ) {
        check(cx, value, false);
    }
}

impl Rule for NoUnsafeOptionalChaining {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Call,
            NodeKind::New,
            NodeKind::Member,
            NodeKind::Index,
            NodeKind::TaggedTemplate,
            NodeKind::Binary,
            NodeKind::Unary,
            NodeKind::Assign,
            NodeKind::Spread,
            NodeKind::VarDeclarator,
            NodeKind::AssignPattern,
            NodeKind::Param,
            NodeKind::ForOf,
            NodeKind::ClassDecl,
            NodeKind::ClassExpr,
            NodeKind::With,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::Call {
                callee, optional, ..
            } => {
                if !optional {
                    check_object(cx, callee);
                }
            }
            Node::New { callee, .. } => check(cx, callee, false),
            Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                if !optional {
                    check_object(cx, obj);
                }
            }
            Node::TaggedTemplate { tag, .. } => check_object(cx, tag),
            Node::Binary { left, op, right } => match op {
                BinaryOp::In | BinaryOp::InstanceOf => check(cx, right, false),
                BinaryOp::Add
                | BinaryOp::Sub
                | BinaryOp::Mul
                | BinaryOp::Div
                | BinaryOp::Mod
                | BinaryOp::Exp
                    if self.arithmetic =>
                {
                    check(cx, left, true);
                    check(cx, right, true);
                }
                _ => {}
            },
            Node::Unary {
                op: UnaryOp::Minus | UnaryOp::Plus,
                arg,
            } if self.arithmetic => {
                check(cx, arg, true);
            }
            Node::Assign { left, op, right } => match op {
                AssignOp::Assign => check_pattern_target(cx, left, right),
                AssignOp::AddAssign
                | AssignOp::SubAssign
                | AssignOp::MulAssign
                | AssignOp::DivAssign
                | AssignOp::ModAssign
                | AssignOp::ExpAssign
                    if self.arithmetic =>
                {
                    check(cx, right, true);
                }
                _ => {}
            },
            Node::Spread { arg } => {
                // Spreading into arrays and calls throws; object spread is safe.
                let parent = cx.parent(node).map(|p| ast.kind(p));
                if matches!(
                    parent,
                    Some(NodeKind::ArrayLit | NodeKind::Call | NodeKind::New)
                ) && !is_assignment_target(ast, cx.file.parents, node)
                {
                    check(cx, arg, false);
                }
            }
            Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } => check_pattern_target(cx, name, init),
            Node::AssignPattern { left, right } => check_pattern_target(cx, left, right),
            Node::Param {
                pattern,
                init: Some(init),
                ..
            } => check_pattern_target(cx, pattern, init),
            Node::ForOf { right, .. } => check(cx, right, false),
            Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. } => {
                for &h in ast.list(heritage) {
                    if let Node::HeritageClause {
                        kind: ts_r_ast::HeritageKind::Extends,
                        types,
                    } = *ast.node(h)
                    {
                        for &t in ast.list(types) {
                            if let Node::ExprWithTypeArgs { expr, .. } = *ast.node(t) {
                                check(cx, expr, false);
                            }
                        }
                    }
                }
            }
            Node::With { obj, .. } => check(cx, obj, false),
            _ => {}
        }
    }
}
