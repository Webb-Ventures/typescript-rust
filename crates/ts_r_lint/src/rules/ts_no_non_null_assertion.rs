//! `@typescript-eslint/no-non-null-assertion`: disallow non-null assertions using the `!`
//! postfix operator.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-non-null-assertion",
        "Disallow non-null assertions using the `!` postfix operator",
        TS_STRICT,
        Category::Restriction,
    )
    .suggestions(),
    create: create_default::<NoNonNull>,
};

#[derive(Debug, Default)]
struct NoNonNull;

const SUGGEST: &str = "Consider using the optional chain operator `?.` instead. This operator includes runtime checks, so it is safer than the compile-only non-null assertion operator.";

impl Rule for NoNonNull {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::NonNull]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let span = cx.span(node);
        let bang = Span::new(span.end - 1, span.end);
        let fix = cx.parent(node).and_then(|p| {
            if is_assignment_target(ast, cx.file.parents, p) {
                return None;
            }
            match *cx.node(p) {
                Node::Member { obj, optional, .. } if obj == node => {
                    if optional {
                        return Some(Fix::delete(bang));
                    }
                    let dot = skip_trivia_forward(cx.src(), span.end as usize) as u32;
                    Some(Fix::delete(bang).and(Span::new(dot, dot), "?"))
                }
                Node::Index { obj, optional, .. } if obj == node => Some(if optional {
                    Fix::delete(bang)
                } else {
                    Fix::replace(bang, "?.")
                }),
                Node::Call {
                    callee, optional, ..
                } if callee == node => Some(if optional {
                    Fix::delete(bang)
                } else {
                    Fix::replace(bang, "?.")
                }),
                _ => None,
            }
        });
        let r = cx
            .report(span, "Forbidden non-null assertion.")
            .id("noNonNull");
        if let Some(f) = fix {
            let _ = r.suggest("suggestOptionalChain", SUGGEST, f);
        }
    }
}
