//! `constructor-super`: require `super()` calls in constructors.

use super::prelude::*;
use crate::cfg::Cfg;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "constructor-super",
        "Require `super()` calls in constructors",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<ConstructorSuper>,
};

#[derive(Debug, Default)]
struct ConstructorSuper;

/// How a class's `extends` clause can behave.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Heritage {
    None,
    Null,
    NotConstructor,
    Constructor,
}

fn possibly_constructor(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::ClassExpr { .. }
        | Node::FunctionExpr { .. }
        | Node::Ident { .. }
        | Node::This {}
        | Node::Member { .. }
        | Node::Index { .. }
        | Node::Call { .. }
        | Node::New { .. }
        | Node::Yield { .. }
        | Node::Await { .. }
        | Node::TaggedTemplate { .. }
        | Node::MetaProperty { .. }
        | Node::NonNull { .. }
        | Node::As { .. }
        | Node::Satisfies { .. }
        | Node::TypeAssertion { .. } => true,
        Node::Assign {
            op: AssignOp::Assign | AssignOp::AndAssign,
            right,
            ..
        } => possibly_constructor(ast, right),
        Node::Assign {
            op: AssignOp::OrAssign | AssignOp::CoalesceAssign,
            left,
            right,
        } => possibly_constructor(ast, left) || possibly_constructor(ast, right),
        Node::Binary {
            op: BinaryOp::LogicalAnd,
            right,
            ..
        } => possibly_constructor(ast, right),
        Node::Binary {
            op: BinaryOp::LogicalOr | BinaryOp::Coalesce,
            left,
            right,
        } => possibly_constructor(ast, left) || possibly_constructor(ast, right),
        Node::Conditional { cons, alt, .. } => {
            possibly_constructor(ast, cons) || possibly_constructor(ast, alt)
        }
        Node::Seq { exprs } => ast
            .list(exprs)
            .last()
            .is_some_and(|&e| possibly_constructor(ast, e)),
        _ => false,
    }
}

/// The heritage of the class owning constructor `method` (a `MethodDef`).
pub(crate) fn class_heritage(cx: &Ctx<'_, '_>, method: NodeId) -> Heritage {
    let ast = cx.ast();
    let Some(class) = cx.parent(method) else {
        return Heritage::None;
    };
    let heritage = match *ast.node(class) {
        Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. } => heritage,
        _ => return Heritage::None,
    };
    for &h in ast.list(heritage) {
        if let Node::HeritageClause {
            kind: ts_r_ast::HeritageKind::Extends,
            types,
        } = *ast.node(h)
            && let Some(&t) = ast.list(types).first()
        {
            let e = match *ast.node(t) {
                Node::ExprWithTypeArgs { expr, .. } => expr,
                _ => t,
            };
            return if matches!(ast.node(skip_parens(ast, e)), Node::NullLit {}) {
                Heritage::Null
            } else if possibly_constructor(ast, e) {
                Heritage::Constructor
            } else {
                Heritage::NotConstructor
            };
        }
    }
    Heritage::None
}

pub(crate) fn is_super_call(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    matches!(*ast.node(id), Node::Call { callee, .. } if matches!(ast.node(callee), Node::Super {}))
}

const ZERO: u8 = 1;
const ONE: u8 = 2;
const MANY: u8 = 4;

impl Rule for ConstructorSuper {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::MethodDef]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::MethodDef {
            kind: MethodKind::Constructor,
            value,
            ..
        } = *ast.node(node)
        else {
            return;
        };
        if !matches!(ast.node(value), Node::FunctionExpr { body: Some(_), .. }) {
            return;
        }
        let heritage = class_heritage(cx, node);
        if heritage == Heritage::None {
            return;
        }
        let cfg = Cfg::function(ast, value);
        let step = |s: u8| -> u8 {
            let mut n = 0;
            if s & ZERO != 0 {
                n |= ONE;
            }
            if s & (ONE | MANY) != 0 {
                n |= MANY;
            }
            n
        };
        let states = cfg.forward(
            ZERO,
            |a, b| a | b,
            |s, e| if is_super_call(ast, e) { step(*s) } else { *s },
        );
        let mut duplicates: Vec<NodeId> = Vec::new();
        let mut bad: Vec<NodeId> = Vec::new();
        let mut ends: Vec<u8> = Vec::new();
        for (i, b) in cfg.blocks.iter().enumerate() {
            let Some(mut s) = states[i] else { continue };
            for &e in &b.events {
                if is_super_call(ast, e) {
                    if heritage != Heritage::Constructor {
                        if !bad.contains(&e) {
                            bad.push(e);
                        }
                    } else if s & (ONE | MANY) != 0 && !duplicates.contains(&e) {
                        duplicates.push(e);
                    }
                    s = step(s);
                } else if let Node::Return { arg: None } = *ast.node(e) {
                    ends.push(s);
                }
            }
        }
        if let Some(s) = states[cfg.exit as usize] {
            ends.push(s);
        }
        if heritage != Heritage::NotConstructor && !ends.is_empty() {
            if ends.iter().all(|s| s & (ONE | MANY) == 0) {
                cx.report_node(node, "Expected to call 'super()'.")
                    .id("missingAll");
            } else if ends.iter().any(|s| s & ZERO != 0) {
                cx.report_node(node, "Lacked a call of 'super()' in some code paths.")
                    .id("missingSome");
            }
        }
        bad.sort_by_key(|&n| cx.span(n).start);
        for n in bad {
            cx.report_node(
                n,
                "Unexpected 'super()' because 'super' is not a constructor.",
            )
            .id("badSuper");
        }
        duplicates.sort_by_key(|&n| cx.span(n).start);
        for n in duplicates {
            cx.report_node(n, "Unexpected duplicate 'super()'.")
                .id("duplicate");
        }
    }
}
