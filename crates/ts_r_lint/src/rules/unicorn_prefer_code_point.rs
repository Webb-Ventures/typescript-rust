//! `unicorn/prefer-code-point`: `codePointAt` over `charCodeAt`, `String.fromCodePoint` over
//! `String.fromCharCode`.
//!
//! `x.charCodeAt(...)` (a non-computed member call, not an optional call) is reported at the
//! method name; the rename is suggested unless the result is used as a number: climbing
//! through parentheses, `||`/`&&`/`??`, TS wrappers and `?:` branches, the first other parent
//! is an arithmetic or bitwise binary, a unary `+`/`-`/`~`, or the right side of an
//! arithmetic compound assignment (a string hash would change at surrogate pairs).
//! `String.fromCharCode` (the identifier `String`, not optional) is always suggested.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-code-point",
        "Prefer `String#codePointAt(…)` over `String#charCodeAt(…)` and `String.fromCodePoint(…)` over `String.fromCharCode(…)`.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<PreferCodePoint>,
};

#[derive(Debug, Default)]
struct PreferCodePoint;

impl Rule for PreferCodePoint {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::Member]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::Call {
                callee,
                optional: false,
                ..
            } => {
                let Node::Member { prop, .. } = *ast.node(skip_parens(ast, callee)) else {
                    return;
                };
                if ident_name(ast, prop) != Some("charCodeAt") {
                    return;
                }
                let numeric = used_numerically(cx, node);
                let r = cx
                    .report_node(
                        prop,
                        "Prefer `String#codePointAt()` over `String#charCodeAt()`.",
                    )
                    .id("error/charCodeAt");
                if !numeric {
                    r.suggest(
                        "suggestion/codePointAt",
                        "Use `String#codePointAt()`.",
                        Fix::replace(ast.span(prop), "codePointAt"),
                    );
                }
            }
            Node::Member {
                obj,
                optional: false,
                prop,
            } => {
                if ident_name(ast, skip_parens(ast, obj)) != Some("String")
                    || ident_name(ast, prop) != Some("fromCharCode")
                {
                    return;
                }
                cx.report_node(
                    prop,
                    "Prefer `String.fromCodePoint()` over `String.fromCharCode()`.",
                )
                .id("error/fromCharCode")
                .suggest(
                    "suggestion/fromCodePoint",
                    "Use `String.fromCodePoint()`.",
                    Fix::replace(ast.span(prop), "fromCodePoint"),
                );
            }
            _ => {}
        }
    }
}

fn used_numerically(cx: &Ctx<'_, '_>, call: NodeId) -> bool {
    let mut current = call;
    let mut depth = 0;
    while let Some(parent) = cx.parent(current) {
        depth += 1;
        if depth > 1000 {
            return false;
        }
        match *cx.node(parent) {
            Node::Paren { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
            | Node::Binary {
                op: BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce,
                ..
            } => current = parent,
            Node::Conditional { cons, alt, .. } if cons == current || alt == current => {
                current = parent
            }
            Node::Binary { op, .. } => return numeric_binary(op),
            Node::Unary { op, .. } => {
                return matches!(op, UnaryOp::Plus | UnaryOp::Minus | UnaryOp::BitNot);
            }
            Node::Assign { op, right, .. } => {
                return right == current
                    && matches!(
                        op,
                        AssignOp::AddAssign
                            | AssignOp::SubAssign
                            | AssignOp::MulAssign
                            | AssignOp::DivAssign
                            | AssignOp::ModAssign
                            | AssignOp::ExpAssign
                            | AssignOp::BitAndAssign
                            | AssignOp::BitOrAssign
                            | AssignOp::BitXorAssign
                            | AssignOp::ShlAssign
                            | AssignOp::ShrAssign
                            | AssignOp::UShrAssign
                    );
            }
            _ => return false,
        }
    }
    false
}

fn numeric_binary(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Add
            | BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::Div
            | BinaryOp::Mod
            | BinaryOp::Exp
            | BinaryOp::BitAnd
            | BinaryOp::BitOr
            | BinaryOp::BitXor
            | BinaryOp::Shl
            | BinaryOp::Shr
            | BinaryOp::UShr
    )
}
