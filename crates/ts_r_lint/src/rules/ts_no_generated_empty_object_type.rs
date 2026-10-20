//! `@typescript-eslint/no-generated-empty-object-type`: generic type references (and
//! intersections) that resolve to the empty object type `{}`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-generated-empty-object-type",
        "Disallow type operations that resolve to the \"empty object\" type",
        TS_STRICT_TC,
        Category::Suspicious,
    )
    .typed(),
    create: create_default::<NoGeneratedEmptyObjectType>,
};

#[derive(Debug, Default)]
struct NoGeneratedEmptyObjectType;

impl Rule for NoGeneratedEmptyObjectType {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::TypeRef, NodeKind::IntersectionType]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if let Node::TypeRef { type_args, .. } = *cx.node(node) {
            if type_args.is_none() {
                return;
            }
            // Members of an intersection are judged by the whole intersection.
            let mut p = cx.parent(node);
            while let Some(x) = p
                && matches!(cx.node(x), Node::ParenType { .. })
            {
                p = cx.parent(x);
            }
            if p.is_some_and(|x| matches!(cx.node(x), Node::IntersectionType { .. })) {
                return;
            }
        }
        let ti = cx.types();
        let Some(t) = ti.type_of_type_node(node) else {
            return;
        };
        if t == Ty::UNRESOLVED || ti.contains_type_params(t) {
            return;
        }
        if !parts(ti, t).into_iter().any(|p| is_empty_object(ti, p)) {
            return;
        }
        cx.report_node(
            node,
            "This type resolves to `{}`, the empty object type. This was likely not intentional.",
        )
        .id("noGeneratedEmptyObjectType");
    }
}

/// An anonymous object type without members, signatures or index signatures.
fn is_empty_object(ti: &dyn TypeInfo, t: Ty) -> bool {
    if t == Ty::UNRESOLVED || !ti.flags(t).contains(TypeFlags::OBJECT) {
        return false;
    }
    if ti.object_flags(t).intersects(
        ObjectFlags::CLASS | ObjectFlags::INTERFACE | ObjectFlags::REFERENCE | ObjectFlags::TUPLE,
    ) {
        return false;
    }
    ti.properties_of(t).is_empty()
        && ti.signatures(t, false).is_empty()
        && ti.signatures(t, true).is_empty()
        && ti.index_type(t, false).is_none()
        && ti.index_type(t, true).is_none()
}
