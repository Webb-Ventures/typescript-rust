//! `@typescript-eslint/prefer-literal-enum-member`: require all enum members to be
//! literal values.

use super::prelude::*;
use super::ts_no_require_imports::plain_string;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/prefer-literal-enum-member",
        "Require all enum members to be literal values",
        TS_STRICT,
        Category::Style,
    ),
    create: create::<PreferLiteralEnumMember>,
};

#[derive(Debug)]
struct PreferLiteralEnumMember {
    bitwise: bool,
}

impl FromOptions for PreferLiteralEnumMember {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(PreferLiteralEnumMember {
            bitwise: bool_field(o, "allowBitwiseExpressions", false),
        })
    }
}

fn is_literal(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    if plain_string(cx, id).is_some() {
        return true;
    }
    match *cx.node(id) {
        Node::NumberLit { .. } | Node::NullLit {} | Node::RegexLit { .. } => true,
        Node::Unary {
            op: UnaryOp::Plus | UnaryOp::Minus,
            arg,
        } => matches!(cx.node(arg), Node::NumberLit { .. }),
        _ => false,
    }
}

/// An operand of an allowed bitwise expression: a number or another member of the enum.
fn bitwise_ok(cx: &Ctx<'_, '_>, id: NodeId, enum_name: &str, members: &[String]) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    match *cx.node(id) {
        Node::NumberLit { .. } => true,
        Node::Ident { .. } => members.iter().any(|m| m == cx.text(id)),
        Node::Member { obj, .. } | Node::Index { obj, .. } => {
            ident_name(ast, obj) == Some(enum_name)
                && member_name(ast, cx.src(), id).is_some_and(|n| members.iter().any(|m| *m == n))
        }
        Node::Unary {
            op: UnaryOp::BitNot | UnaryOp::Minus | UnaryOp::Plus,
            arg,
        } => bitwise_ok(cx, arg, enum_name, members),
        Node::Binary { left, op, right } => {
            matches!(
                op,
                BinaryOp::Shl
                    | BinaryOp::Shr
                    | BinaryOp::UShr
                    | BinaryOp::BitOr
                    | BinaryOp::BitAnd
                    | BinaryOp::BitXor
            ) && bitwise_ok(cx, left, enum_name, members)
                && bitwise_ok(cx, right, enum_name, members)
        }
        _ => false,
    }
}

fn is_bitwise(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    matches!(
        *cx.node(id),
        Node::Unary {
            op: UnaryOp::BitNot,
            ..
        } | Node::Binary {
            op: BinaryOp::Shl
                | BinaryOp::Shr
                | BinaryOp::UShr
                | BinaryOp::BitOr
                | BinaryOp::BitAnd
                | BinaryOp::BitXor,
            ..
        }
    )
}

impl Rule for PreferLiteralEnumMember {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Enum]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Enum { name, members, .. } = *cx.node(node) else {
            return;
        };
        let enum_name = cx.text(name).to_owned();
        let names: Vec<String> = ast
            .list(members)
            .iter()
            .filter_map(|&m| match *cx.node(m) {
                Node::EnumMember { name, .. } => {
                    static_key_name(ast, cx.src(), name).map(|n| n.into_owned())
                }
                _ => None,
            })
            .collect();
        for &m in ast.list(members) {
            let Node::EnumMember {
                name: mname,
                init: Some(init),
            } = *cx.node(m)
            else {
                continue;
            };
            if is_literal(cx, init) {
                continue;
            }
            if self.bitwise {
                if is_bitwise(cx, init) && bitwise_ok(cx, init, &enum_name, &names) {
                    continue;
                }
                cx.report_node(
                    mname,
                    "Explicit enum value must only be a literal value (string or number) or a bitwise expression.",
                )
                .id("notLiteralOrBitwiseExpression");
            } else {
                cx.report_node(
                    mname,
                    "Explicit enum value must only be a literal value (string or number).",
                )
                .id("notLiteral");
            }
        }
    }
}
