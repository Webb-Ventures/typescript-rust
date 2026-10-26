//! `unicorn/no-await-expression-member`: a member read straight off `(await x)`. The
//! property is reported. `const a = (await x)[0]` / `[1]` (also `-0`, `+0`, `0 as const`)
//! is fixed to `const [a]` / `const [, a] = await x` unless the awaited value is known not to
//! be iterable (object literal, operator expression, non-string literal), and
//! `const a = (await x).a` to `const {a} = await x`; both only for a plain identifier
//! binding without a type annotation, and only when no comment sits outside the
//! parenthesized `await`.

use super::prelude::*;
use super::unicorn_util::{parenthesized_span, skip_wrappers};
use super::unicorn_util_h4::fix_of;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-await-expression-member",
        "Disallow member access from await expression.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoAwaitExpressionMember>,
};

#[derive(Debug, Default)]
struct NoAwaitExpressionMember;

/// A number literal, possibly signed with `+`/`-` and wrapped in TS expressions.
fn static_number(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> Option<f64> {
    if depth > 64 {
        return None;
    }
    match *cx.node(skip_wrappers(cx.ast(), id)) {
        Node::NumberLit { value } => Some(value.get()),
        Node::Unary {
            op: op @ (UnaryOp::Minus | UnaryOp::Plus),
            arg,
        } => static_number(cx, arg, depth + 1).map(|v| if op == UnaryOp::Minus { -v } else { v }),
        _ => None,
    }
}

/// Syntax that can't be iterated by a destructuring pattern.
fn known_non_iterable(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    match cx.node(skip_wrappers(cx.ast(), id)) {
        Node::Binary { op, .. } => !matches!(
            op,
            BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce
        ),
        n => matches!(
            n.kind(),
            NodeKind::ObjectLit
                | NodeKind::Unary
                | NodeKind::Update
                | NodeKind::NumberLit
                | NodeKind::BoolLit
                | NodeKind::NullLit
                | NodeKind::RegexLit
                | NodeKind::BigIntLit
        ),
    }
}

impl Rule for NoAwaitExpressionMember {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Member, NodeKind::Index]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (obj, optional, prop, computed) = match *cx.node(node) {
            Node::Member {
                obj,
                optional,
                prop,
            } => (obj, optional, prop, false),
            Node::Index {
                obj,
                optional,
                index,
            } => (obj, optional, skip_parens(ast, index), true),
            _ => return,
        };
        let awaited = skip_parens(ast, obj);
        let Node::Await { arg } = *ast.node(awaited) else {
            return;
        };
        let prop_span = cx.span(prop);
        let fix = (|| {
            if optional {
                return None;
            }
            let decl = parent_skip_parens(ast, cx.file.parents, node)?;
            let Node::VarDeclarator {
                name,
                type_ann: None,
                init: Some(init),
                ..
            } = *ast.node(decl)
            else {
                return None;
            };
            let binding = ident_name(ast, name)?;
            if skip_parens(ast, init) != node {
                return None;
            }
            let (open, close) = if computed {
                let index = static_number(cx, prop, 0)?;
                let open = if index == 0.0 {
                    "["
                } else if index == 1.0 {
                    "[, "
                } else {
                    return None;
                };
                if known_non_iterable(cx, arg) {
                    return None;
                }
                (open, "]")
            } else {
                if ident_name(ast, prop) != Some(binding) {
                    return None;
                }
                ("{", "}")
            };
            let member = cx.span(node);
            let kept = parenthesized_span(cx, awaited);
            if ast
                .comments_in(member)
                .iter()
                .any(|c| !(c.span.start >= kept.start && c.span.end <= kept.end))
            {
                return None;
            }
            let name_span = cx.span(name);
            let mut edits = vec![
                (Span::at(name_span.start), open.to_owned()),
                (Span::at(name_span.end), close.to_owned()),
                (Span::new(kept.end, member.end), String::new()),
            ];
            let mut cur = awaited;
            while let Some(p) = cx.parent(cur) {
                if !matches!(cx.node(p), Node::Paren { .. }) {
                    break;
                }
                let s = cx.span(p);
                edits.push((Span::new(s.start, s.start + 1), String::new()));
                edits.push((Span::new(s.end - 1, s.end), String::new()));
                cur = p;
            }
            Some(fix_of(edits))
        })();
        cx.report(
            prop_span,
            "Do not access a member directly from an await expression.",
        )
        .id("no-await-expression-member")
        .fix_opt(fix);
    }
}
