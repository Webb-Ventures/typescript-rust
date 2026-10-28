//! `unicorn/consistent-existence-index-check`: for `const i = x.indexOf(…)` (also
//! `lastIndexOf`, `findIndex`, `findLastIndex`), the checks `i < 0`, `i >= 0` and `i > -1`
//! are written `i === -1` / `i !== -1`. Reported from the operator to the end of the right
//! side. The fix swaps the operator and value, and parenthesizes the comparison where the
//! lower-precedence equality would otherwise regroup (adding a leading `;` when needed).

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_text::{is_parenthesized, needs_semicolon};
use crate::ast_util::precedence;
use ts_r_ast::{Ast, VarKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/consistent-existence-index-check",
        "Enforce consistent style for element existence checks with `indexOf()`, `lastIndexOf()`, `findIndex()`, and `findLastIndex()`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<ConsistentExistenceIndexCheck>,
};

#[derive(Debug, Default)]
struct ConsistentExistenceIndexCheck;

fn is_number(ast: &Ast, id: NodeId, n: f64) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::NumberLit { value } if value.get() == n)
}

fn is_negative_one(ast: &Ast, id: NodeId) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::Unary { op: UnaryOp::Minus, arg } if matches!(*ast.node(skip_parens(ast, arg)), Node::NumberLit { value } if value.get() == 1.0))
}

/// The parent of `id` through parentheses, and the outermost parenthesized form of `id`.
fn real_parent(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(NodeId, NodeId)> {
    let mut cur = id;
    loop {
        let p = cx.parent(cur)?;
        if matches!(cx.node(p), Node::Paren { .. }) {
            cur = p;
        } else {
            return Some((p, cur));
        }
    }
}

/// `shouldAddParenthesesToComparison` for an equality operator replacing `bin`.
fn needs_parens(cx: &Ctx<'_, '_>, bin: NodeId) -> bool {
    if is_parenthesized(cx, bin) {
        return false;
    }
    let ast = cx.ast();
    let Some(parent) = cx.parent(bin) else {
        return false;
    };
    match *ast.node(parent) {
        Node::Member { obj, .. } | Node::Index { obj, .. } => obj == bin,
        Node::Call { callee, .. } | Node::New { callee, .. } => callee == bin,
        Node::TaggedTemplate { tag, .. } => tag == bin,
        Node::Binary { op, right, .. } => {
            if matches!(
                op,
                BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce
            ) {
                return false;
            }
            let pp = precedence(ast, parent);
            pp > 9 || (pp == 9 && right == bin)
        }
        Node::As { .. } | Node::Satisfies { .. } => precedence(ast, parent) > 9,
        Node::Await { .. }
        | Node::NonNull { .. }
        | Node::TypeAssertion { .. }
        | Node::Unary { .. } => true,
        _ => false,
    }
}

impl Rule for ConsistentExistenceIndexCheck {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::VarDeclarator]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::VarDeclarator {
            name,
            init: Some(init),
            ..
        } = *cx.node(node)
        else {
            return;
        };
        if !cx.parent(node).is_some_and(|p| {
            matches!(
                *ast.node(p),
                Node::VarDecl {
                    kind: VarKind::Const,
                    ..
                }
            )
        }) || !matches!(ast.node(name), Node::Ident { .. })
        {
            return;
        }
        let Some(call) = method_call(ast, skip_parens(ast, init)) else {
            return;
        };
        if !call.plain()
            || !matches!(
                call.name,
                "indexOf" | "lastIndexOf" | "findIndex" | "findLastIndex"
            )
        {
            return;
        }
        let sem = cx.semantic();
        let Some(sym) = sem.symbol_of_decl(name) else {
            return;
        };
        if sem.symbol(sym).decls.len() != 1 {
            return;
        }
        let refs: Vec<NodeId> = sem
            .symbol(sym)
            .refs
            .iter()
            .map(|&r| sem.reference(r).node)
            .collect();
        let src = cx.src();
        for ident in refs {
            let Some((bin, child)) = real_parent(cx, ident) else {
                continue;
            };
            let Node::Binary { left, op, right } = *ast.node(bin) else {
                continue;
            };
            if left != child {
                continue;
            }
            let (new_op, new_value, old_value) = match op {
                BinaryOp::Lt if is_number(ast, right, 0.0) => ("===", "-1", "0"),
                BinaryOp::Gt if is_negative_one(ast, right) => ("!==", "-1", "-1"),
                BinaryOp::GtEq if is_number(ast, right, 0.0) => ("!==", "-1", "0"),
                _ => continue,
            };
            let old_op = match op {
                BinaryOp::Lt => "<",
                BinaryOp::Gt => ">",
                _ => ">=",
            };
            // The operator token: after the left operand and its closing parentheses.
            let mut pos = skip_trivia_forward(src, cx.span(ident).end as usize);
            while src.as_bytes().get(pos) == Some(&b')') {
                pos = skip_trivia_forward(src, pos + 1);
            }
            if !src[pos..].starts_with(old_op) {
                continue;
            }
            let op_span = Span::new(pos as u32, (pos + old_op.len()) as u32);
            let right_inner = skip_parens(ast, right);
            let mut fix = Fix::replace(op_span, new_op);
            if new_value != old_value {
                fix = fix.and(cx.span(right_inner), new_value);
            }
            if needs_parens(cx, bin) {
                let b = cx.span(bin);
                let open = if needs_semicolon(cx, b.start, "(") {
                    ";("
                } else {
                    "("
                };
                fix = fix.and(Span::at(b.start), open).and(Span::at(b.end), ")");
            }
            let existence = if new_op == "===" {
                "non-existence"
            } else {
                "existence"
            };
            cx.report(
                Span::new(op_span.start, cx.span(right_inner).end),
                format!(
                    "Prefer `{new_op} {new_value}` over `{old_op} {old_value}` to check {existence}."
                ),
            )
            .id("consistent-existence-index-check")
            .fix(fix);
        }
    }
}
