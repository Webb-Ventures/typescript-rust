//! `unicorn/no-array-sort`: prefer `Array#toSorted()` over `Array#sort()`; see
//! [`super::unicorn_array_mutate`].

use super::prelude::*;
use super::unicorn_array_mutate::{ArrayMutate, check};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-array-sort",
        "Prefer `Array#toSorted()` over `Array#sort()`.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create::<NoArraySort>,
};

#[derive(Debug)]
struct NoArraySort(ArrayMutate);

impl FromOptions for NoArraySort {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoArraySort(ArrayMutate::from_options(opts)))
    }
}

impl Rule for NoArraySort {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        check(&self.0, cx, node, "sort", "toSorted");
    }
}
