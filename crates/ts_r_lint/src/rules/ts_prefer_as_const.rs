//! `@typescript-eslint/prefer-as-const`: enforce `as const` over literal types.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/prefer-as-const",
        "Enforce the use of `as const` over literal type",
        TS_REC,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferAsConst>,
};

#[derive(Debug, Default)]
struct PreferAsConst;

/// A literal value and a literal type with the same value.
fn same_literal(cx: &Ctx<'_, '_>, value: NodeId, ty: NodeId) -> bool {
    let ast = cx.ast();
    match (*cx.node(value), *cx.node(ty)) {
        (Node::StringLit { value: a }, Node::StringLit { value: b }) => ast.atom(a) == ast.atom(b),
        (Node::NumberLit { value: a }, Node::NumberLit { value: b }) => a.get() == b.get(),
        _ => false,
    }
}

impl Rule for PreferAsConst {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::As,
            NodeKind::TypeAssertion,
            NodeKind::VarDeclarator,
            NodeKind::PropertyDef,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::As { expr, ty } | Node::TypeAssertion { ty, expr } => {
                if same_literal(cx, expr, ty) {
                    let span = cx.span(ty);
                    cx.report(
                        span,
                        "Expected a `const` instead of a literal type assertion.",
                    )
                    .id("preferConstAssertion")
                    .fix(Fix::replace(span, "const"));
                }
            }
            Node::VarDeclarator {
                name,
                type_ann: Some(ty),
                init: Some(init),
                ..
            }
            | Node::PropertyDef {
                key: name,
                type_ann: Some(ty),
                value: Some(init),
                ..
            } => {
                if !same_literal(cx, init, ty) {
                    return;
                }
                let span = cx.span(ty);
                let name_end = cx.span(name).end;
                let init_end = cx.span(init).end;
                cx.report(
                    span,
                    "Expected a `const` assertion instead of a literal type annotation.",
                )
                .id("variableConstAssertion")
                .suggest(
                    "variableSuggest",
                    "You should use `as const` instead of type annotation.",
                    Fix::delete(Span::new(name_end, span.end))
                        .and(Span::new(init_end, init_end), " as const"),
                );
            }
            _ => {}
        }
    }
}
