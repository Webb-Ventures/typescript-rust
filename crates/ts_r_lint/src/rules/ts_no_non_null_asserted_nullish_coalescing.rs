//! `@typescript-eslint/no-non-null-asserted-nullish-coalescing`: disallow non-null
//! assertions in the left operand of a nullish coalescing operator.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-non-null-asserted-nullish-coalescing",
        "Disallow non-null assertions in the left operand of a nullish coalescing operator",
        TS_STRICT,
        Category::Suspicious,
    )
    .suggestions(),
    create: create_default::<NoNonNullNullish>,
};

#[derive(Debug, Default)]
struct NoNonNullNullish;

/// `let x: T;` read before any assignment: the assertion silences "used before assigned".
fn needed_for_definite_assignment(cx: &Ctx<'_, '_>, ident: NodeId) -> bool {
    let sem = cx.semantic();
    let Some(s) = sem.symbol_of_ident(ident) else {
        return false;
    };
    let s = sem.symbol(s);
    let [d] = s.decls.as_slice() else {
        return false;
    };
    let Some(vd) = cx.parent(d.name) else {
        return false;
    };
    let Node::VarDeclarator {
        name,
        definite: false,
        init: None,
        ..
    } = *cx.node(vd)
    else {
        return false;
    };
    if name != d.name {
        return false;
    }
    let at = cx.span(ident).start;
    !s.refs.iter().any(|&r| {
        let r = sem.reference(r);
        r.flags.contains(RefFlags::WRITE) && cx.span(r.node).start < at
    })
}

impl Rule for NoNonNullNullish {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Binary {
            left,
            op: BinaryOp::Coalesce,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        let Node::NonNull { expr } = *cx.node(left) else {
            return;
        };
        if matches!(cx.node(expr), Node::Ident { .. }) && needed_for_definite_assignment(cx, expr) {
            return;
        }
        let span = cx.span(left);
        let bang = Span::new(span.end - 1, span.end);
        cx.report(
            span,
            "The nullish coalescing operator is designed to handle undefined and null - using a non-null assertion is not needed.",
        )
        .id("noNonNullAssertedNullishCoalescing")
        .suggest(
            "suggestRemovingNonNull",
            "Remove the non-null assertion.",
            Fix::delete(bang),
        );
    }
}
