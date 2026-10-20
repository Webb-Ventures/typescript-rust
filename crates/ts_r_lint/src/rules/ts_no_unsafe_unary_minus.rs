//! `@typescript-eslint/no-unsafe-unary-minus`: unary `-` only on numbers and bigints.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-unsafe-unary-minus",
        "Require unary negation to take a number",
        TS_REC_TC,
        Category::Suspicious,
    )
    .typed(),
    create: create_default::<NoUnsafeUnaryMinus>,
};

#[derive(Debug, Default)]
struct NoUnsafeUnaryMinus;

impl Rule for NoUnsafeUnaryMinus {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Unary]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Unary {
            op: UnaryOp::Minus,
            arg,
        } = *cx.node(node)
        else {
            return;
        };
        let ti = cx.types();
        let Some(t) = ti.type_at(arg) else { return };
        if t == Ty::UNRESOLVED || has_unresolved_part(ti, t) {
            return;
        }
        if numeric(ti, t, 0) != Some(false) {
            return;
        }
        let name = ti.type_to_string(t);
        cx.report_node(
            node,
            format!(
                "Argument of unary negation should be assignable to number | bigint but is {name} instead."
            ),
        )
        .id("unaryMinus");
    }
}

/// `Some(true)` when every part is number- or bigint-like (or `any`/`never`), `None` when
/// unknown to us.
fn numeric(ti: &dyn TypeInfo, t: Ty, depth: u32) -> Option<bool> {
    if depth > 16 || t == Ty::UNRESOLVED {
        return None;
    }
    let f = ti.flags(t);
    if f.intersects(TypeFlags::ANY | TypeFlags::NEVER) {
        return Some(true);
    }
    if f.intersects(TypeFlags::NUMBER_LIKE | TypeFlags::BIGINT_LIKE) {
        return Some(true);
    }
    if f.contains(TypeFlags::UNION) {
        let mut all = true;
        for p in ti.union_members(t) {
            match numeric(ti, p, depth + 1) {
                None => return None,
                Some(false) => all = false,
                Some(true) => {}
            }
        }
        return Some(all);
    }
    if f.contains(TypeFlags::INTERSECTION) {
        let mut any = false;
        for p in ti.intersection_members(t) {
            match numeric(ti, p, depth + 1) {
                None => return None,
                Some(true) => any = true,
                Some(false) => {}
            }
        }
        return Some(any);
    }
    if f.intersects(TypeFlags::INSTANTIABLE) {
        let c = constrained(ti, t);
        if c == t {
            return Some(false);
        }
        return numeric(ti, c, depth + 1);
    }
    Some(false)
}
