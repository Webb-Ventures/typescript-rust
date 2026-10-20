//! `@typescript-eslint/no-wrapper-object-types`: disallow wrapper object types
//! (`String`, `Number`, ...).

use super::prelude::*;
use super::ts_no_unsafe_function_type::global_type_name;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-wrapper-object-types",
        "Disallow using confusing built-in primitive class wrappers",
        TS_REC,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoWrapperObjectTypes>,
};

#[derive(Debug, Default)]
struct NoWrapperObjectTypes;

impl Rule for NoWrapperObjectTypes {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::TypeRef, NodeKind::ExprWithTypeArgs]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Some((name, is_ref)) = global_type_name(cx, node) else {
            return;
        };
        let text = cx.text(name);
        let prim = match text {
            "BigInt" => "bigint",
            "Boolean" => "boolean",
            "Number" => "number",
            "Object" => "object",
            "String" => "string",
            "Symbol" => "symbol",
            _ => return,
        };
        let span = cx.span(name);
        let r = cx
            .report(
                span,
                format!(
                    "Prefer using the primitive `{prim}` as a type name, rather than the upper-cased `{text}`."
                ),
            )
            .id("bannedClassType");
        if is_ref {
            let _ = r.fix(Fix::replace(span, prim));
        }
    }
}
