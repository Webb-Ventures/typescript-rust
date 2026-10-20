//! `@typescript-eslint/no-for-in-array`: disallow `for-in` over arrays.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-for-in-array",
        "Disallow iterating over an array with a for-in loop",
        TS_REC_TC,
        Category::Correctness,
    )
    .typed(),
    create: create_default::<NoForInArray>,
};

#[derive(Debug, Default)]
struct NoForInArray;

const MESSAGE: &str = "For-in loops over arrays skips holes, returns indices as strings, and may visit the prototype chain or other enumerable properties. Use a more robust iteration method such as for-of or array.forEach instead.";

impl Rule for NoForInArray {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ForIn]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::ForIn { right, .. } = *cx.node(node) else {
            return;
        };
        let ti = cx.types();
        let Some(t) = constrained_type_at(ti, right) else {
            return;
        };
        if t == Ty::UNRESOLVED || is_any(ti, t) {
            return;
        }
        let array_like = parts(ti, ti.non_nullable(t)).into_iter().any(|p| {
            let p = constrained(ti, p);
            let members = if ti.flags(p).contains(TypeFlags::INTERSECTION) {
                ti.intersection_members(p)
            } else {
                vec![p]
            };
            members.into_iter().any(|m| is_array_like(ti, m))
        });
        if !array_like {
            return;
        }
        // From `for` to the `)` closing the head.
        let src = cx.src();
        let start = cx.span(node).start;
        let after = skip_trivia_forward(src, cx.span(right).end as usize);
        let end = if src.as_bytes().get(after) == Some(&b')') {
            after as u32 + 1
        } else {
            cx.span(right).end
        };
        cx.report(Span::new(start, end), MESSAGE)
            .id("forInViolation");
    }
}

/// Arrays, tuples, and objects with a number index signature and a `length`.
fn is_array_like(ti: &dyn TypeInfo, t: Ty) -> bool {
    if t == Ty::UNRESOLVED {
        return false;
    }
    if ti.is_array(t) || ti.is_tuple(t) {
        return true;
    }
    ti.index_type(t, true).is_some() && ti.property_of_type(t, "length").is_some()
}
