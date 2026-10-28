//! `jsdoc/reject-any-type`: `any` or `*` type is too loose for a JSDoc type.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/reject-any-type",
        "Reports use of `any` or `*` type",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<RejectAnyType>,
};

#[derive(Debug)]
struct RejectAnyType;

impl FromOptions for RejectAnyType {
    fn from_options(_: &[Value]) -> Result<Self, String> {
        Ok(RejectAnyType)
    }
}

impl Rule for RejectAnyType {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        crate::jsdoc_util::reject_type_names(
            cx,
            &[
                ("*", "Prefer a more specific type to `*`"),
                ("any", "Prefer a more specific type to `any`"),
            ],
        );
    }
}
