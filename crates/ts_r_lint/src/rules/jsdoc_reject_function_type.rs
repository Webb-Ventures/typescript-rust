//! `jsdoc/reject-function-type`: `Function` type is too loose for a JSDoc type.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/reject-function-type",
        "Reports use of `Function` type",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<RejectFunctionType>,
};

#[derive(Debug)]
struct RejectFunctionType;

impl FromOptions for RejectFunctionType {
    fn from_options(_: &[Value]) -> Result<Self, String> {
        Ok(RejectFunctionType)
    }
}

impl Rule for RejectFunctionType {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        crate::jsdoc_util::reject_type_names(
            cx,
            &[("Function", "Prefer a more specific type to `Function`")],
        );
    }
}
