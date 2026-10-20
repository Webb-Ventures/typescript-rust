//! `no-constant-binary-expression`: disallow expressions where the operation doesn't affect
//! the value.

use super::no_constant_condition::is_constant;
use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-constant-binary-expression",
        "Disallow expressions where the operation doesn't affect the value",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create::<NoConstantBinaryExpression>,
};

#[derive(Debug, Default)]
struct NoConstantBinaryExpression {
    relational: bool,
}

impl FromOptions for NoConstantBinaryExpression {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoConstantBinaryExpression {
            relational: bool_field(obj_opt(opts, 0), "checkRelationalComparisons", false),
        })
    }
}

const CONSTRUCTORS: &[&str] = &[
    "AggregateError",
    "Array",
    "ArrayBuffer",
    "BigInt64Array",
    "BigUint64Array",
    "Boolean",
    "DataView",
    "Date",
    "Error",
    "EvalError",
    "FinalizationRegistry",
    "Float16Array",
    "Float32Array",
    "Float64Array",
    "Function",
    "Int16Array",
    "Int32Array",
    "Int8Array",
    "Map",
    "Number",
    "Object",
    "Promise",
    "Proxy",
    "RangeError",
    "ReferenceError",
    "RegExp",
    "Set",
    "SharedArrayBuffer",
    "String",
    "SyntaxError",
    "TypeError",
    "URIError",
    "Uint16Array",
    "Uint32Array",
    "Uint8Array",
    "Uint8ClampedArray",
    "WeakMap",
    "WeakRef",
    "WeakSet",
];

fn global_call(cx: &Ctx<'_, '_>, id: NodeId, names: &[&str]) -> bool {
    let ast = cx.ast();
    let Node::Call { callee, .. } = *ast.node(id) else {
        return false;
    };
    let callee = skip_parens(ast, callee);
    match ident_name(ast, callee) {
        Some(n) => names.contains(&n) && cx.is_global_ref(callee, n),
        None => false,
    }
}

fn is_null_or_undefined(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::NullLit {} => true,
        Node::Ident { .. } => cx.is_global_ref(id, "undefined"),
        Node::Unary {
            op: UnaryOp::Void, ..
        } => true,
        _ => false,
    }
}

fn is_static_boolean(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::BoolLit { .. } => true,
        Node::Unary {
            op: UnaryOp::Not,
            arg,
        } => is_constant(cx, arg, true),
        Node::Call { args, .. } => {
            global_call(cx, id, &["Boolean"])
                && match ast.list(args).first() {
                    None => true,
                    Some(&a) => {
                        !matches!(ast.node(a), Node::Spread { .. }) && is_constant(cx, a, true)
                    }
                }
        }
        _ => false,
    }
}

fn is_plain_literal(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::NumberLit { .. }
            | Node::StringLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::RegexLit { .. }
            | Node::BigIntLit { .. }
    )
}

fn has_constant_nullishness(cx: &Ctx<'_, '_>, id: NodeId, non_nullish: bool) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    if non_nullish && is_null_or_undefined(cx, id) {
        return false;
    }
    match *ast.node(id) {
        _ if is_plain_literal(ast, id) => true,
        Node::ObjectLit { .. }
        | Node::ArrayLit { .. }
        | Node::Arrow { .. }
        | Node::FunctionExpr { .. }
        | Node::ClassExpr { .. }
        | Node::New { .. }
        | Node::TemplateLit { .. }
        | Node::Update { .. }
        | Node::Unary { .. } => true,
        Node::Binary { op, right, .. } => match op {
            BinaryOp::Coalesce => has_constant_nullishness(cx, right, true),
            BinaryOp::LogicalAnd | BinaryOp::LogicalOr => false,
            _ => true,
        },
        Node::Assign { op, right, .. } => match op {
            AssignOp::Assign => has_constant_nullishness(cx, right, non_nullish),
            AssignOp::OrAssign | AssignOp::AndAssign | AssignOp::CoalesceAssign => false,
            _ => true,
        },
        Node::Seq { exprs } => ast
            .list(exprs)
            .last()
            .is_some_and(|&l| has_constant_nullishness(cx, l, non_nullish)),
        Node::Ident { .. } => cx.is_global_ref(id, "undefined"),
        Node::Call { .. } => {
            global_call(cx, id, &["Boolean", "String", "Number", "Symbol", "BigInt"])
        }
        _ => false,
    }
}

fn has_constant_loose_boolean_comparison(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        _ if is_plain_literal(ast, id) => true,
        Node::ObjectLit { .. }
        | Node::Arrow { .. }
        | Node::FunctionExpr { .. }
        | Node::ClassExpr { .. } => true,
        Node::ArrayLit { elements } => {
            let els = ast.list(elements);
            if els
                .iter()
                .any(|&e| matches!(ast.node(e), Node::Spread { .. }))
            {
                return false;
            }
            els.is_empty()
                || els
                    .iter()
                    .filter(|&&e| !matches!(ast.node(e), Node::Hole {}))
                    .count()
                    > 1
        }
        Node::Unary { op, arg } => match op {
            UnaryOp::Void | UnaryOp::TypeOf => true,
            UnaryOp::Not => is_constant(cx, arg, true),
            _ => false,
        },
        Node::TemplateLit { parts } => ast.list(parts).len() == 1,
        Node::Ident { .. } => cx.is_global_ref(id, "undefined"),
        Node::Assign {
            op: AssignOp::Assign,
            right,
            ..
        } => has_constant_loose_boolean_comparison(cx, right),
        Node::Seq { exprs } => ast
            .list(exprs)
            .last()
            .is_some_and(|&l| has_constant_loose_boolean_comparison(cx, l)),
        Node::Call { .. } => is_static_boolean(cx, id),
        _ => false,
    }
}

fn has_constant_strict_boolean_comparison(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        _ if is_plain_literal(ast, id) => true,
        Node::ObjectLit { .. }
        | Node::ArrayLit { .. }
        | Node::Arrow { .. }
        | Node::FunctionExpr { .. }
        | Node::ClassExpr { .. }
        | Node::New { .. }
        | Node::TemplateLit { .. }
        | Node::Update { .. } => true,
        Node::Unary { op, arg } => match op {
            UnaryOp::Not => is_constant(cx, arg, true),
            UnaryOp::Delete => false,
            _ => true,
        },
        Node::Binary { op, .. } => matches!(
            op,
            BinaryOp::Add
                | BinaryOp::Sub
                | BinaryOp::Mul
                | BinaryOp::Div
                | BinaryOp::Mod
                | BinaryOp::Exp
                | BinaryOp::Shl
                | BinaryOp::Shr
                | BinaryOp::UShr
                | BinaryOp::BitOr
                | BinaryOp::BitXor
                | BinaryOp::BitAnd
        ),
        Node::Assign { op, right, .. } => match op {
            AssignOp::Assign => has_constant_strict_boolean_comparison(cx, right),
            AssignOp::OrAssign | AssignOp::AndAssign | AssignOp::CoalesceAssign => false,
            _ => true,
        },
        Node::Seq { exprs } => ast
            .list(exprs)
            .last()
            .is_some_and(|&l| has_constant_strict_boolean_comparison(cx, l)),
        Node::Ident { .. } => cx.is_global_ref(id, "undefined"),
        Node::Call { .. } => {
            is_static_boolean(cx, id)
                || global_call(cx, id, &["String", "Number", "Symbol", "BigInt"])
        }
        _ => false,
    }
}

fn is_always_new(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::ObjectLit { .. }
        | Node::ArrayLit { .. }
        | Node::Arrow { .. }
        | Node::FunctionExpr { .. }
        | Node::ClassExpr { .. }
        | Node::RegexLit { .. } => true,
        Node::New { callee, .. } => {
            let c = skip_parens(ast, callee);
            ident_name(ast, c).is_some_and(|n| CONSTRUCTORS.contains(&n) && cx.is_global_ref(c, n))
        }
        Node::Seq { exprs } => ast
            .list(exprs)
            .last()
            .is_some_and(|&l| is_always_new(cx, l)),
        Node::Assign {
            op: AssignOp::Assign,
            right,
            ..
        } => is_always_new(cx, right),
        Node::Conditional { cons, alt, .. } => is_always_new(cx, cons) && is_always_new(cx, alt),
        _ => false,
    }
}

fn is_relational_literal(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        _ if is_plain_literal(ast, id) => true,
        Node::TemplateLit { parts } => ast.list(parts).len() == 1,
        Node::Ident { .. } => cx.is_global_ref(id, "undefined"),
        Node::Unary {
            op: UnaryOp::Minus | UnaryOp::Plus | UnaryOp::BitNot,
            arg,
        } => is_relational_literal(cx, arg),
        _ => false,
    }
}

impl Rule for NoConstantBinaryExpression {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Binary { left, op, right } = *cx.node(node) else {
            return;
        };
        let ops = op.as_str();
        match op {
            BinaryOp::LogicalAnd | BinaryOp::LogicalOr => {
                if is_constant(cx, left, true) {
                    cx.report_node(
                        skip_parens(ast, left),
                        format!("Unexpected constant truthiness on the left-hand side of a `{ops}` expression."),
                    )
                    .id("constantShortCircuit");
                }
            }
            BinaryOp::Coalesce => {
                if has_constant_nullishness(cx, left, false) {
                    cx.report_node(
                        skip_parens(ast, left),
                        format!("Unexpected constant nullishness on the left-hand side of a `{ops}` expression."),
                    )
                    .id("constantShortCircuit");
                }
            }
            BinaryOp::Eq | BinaryOp::NotEq | BinaryOp::StrictEq | BinaryOp::StrictNotEq => {
                let strict = matches!(op, BinaryOp::StrictEq | BinaryOp::StrictNotEq);
                let bool_cmp = |n: NodeId| {
                    if strict {
                        has_constant_strict_boolean_comparison(cx, n)
                    } else {
                        has_constant_loose_boolean_comparison(cx, n)
                    }
                };
                // (constant side, other side described in the message)
                let hit = if (is_static_boolean(cx, left) && bool_cmp(right))
                    || (is_null_or_undefined(cx, left)
                        && has_constant_nullishness(cx, right, false))
                {
                    Some((right, "left"))
                } else if (is_static_boolean(cx, right) && bool_cmp(left))
                    || (is_null_or_undefined(cx, right)
                        && has_constant_nullishness(cx, left, false))
                {
                    Some((left, "right"))
                } else {
                    None
                };
                if let Some((n, side)) = hit {
                    cx.report_node(
                        skip_parens(ast, n),
                        format!("Unexpected constant binary expression. Compares constantly with the {side}-hand side of the `{ops}`."),
                    )
                    .id("constantBinaryOperand");
                    return;
                }
                let (l, r) = (is_always_new(cx, left), is_always_new(cx, right));
                if l && r {
                    cx.report_node(
                        skip_parens(ast, left),
                        "Unexpected comparison of two newly constructed objects. These two values can never be equal.",
                    )
                    .id("bothAlwaysNew");
                } else if strict && (l || r) {
                    cx.report_node(
                        skip_parens(ast, if l { left } else { right }),
                        "Unexpected comparison to newly constructed object. These two values can never be equal.",
                    )
                    .id("alwaysNew");
                }
            }
            BinaryOp::Lt | BinaryOp::LtEq | BinaryOp::Gt | BinaryOp::GtEq => {
                if self.relational
                    && is_relational_literal(cx, left)
                    && is_relational_literal(cx, right)
                {
                    cx.report_node(
                        node,
                        format!("Unexpected constant relational comparison. Both sides of the `{ops}` are literal values."),
                    )
                    .id("constantRelationalComparison");
                }
            }
            _ => {}
        }
    }
}
