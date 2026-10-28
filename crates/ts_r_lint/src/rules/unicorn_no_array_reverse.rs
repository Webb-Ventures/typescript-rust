//! `unicorn/no-array-reverse`: prefer `Array#toReversed()` over `Array#reverse()`; see
//! [`super::unicorn_array_mutate`].

use super::prelude::*;
use super::unicorn_array_mutate::{ArrayMutate, check};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-array-reverse",
        "Prefer `Array#toReversed()` over `Array#reverse()`.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create::<NoArrayReverse>,
};

#[derive(Debug)]
struct NoArrayReverse(ArrayMutate);

impl FromOptions for NoArrayReverse {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoArrayReverse(ArrayMutate::from_options(opts)))
    }
}

impl Rule for NoArrayReverse {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        check(&self.0, cx, node, "reverse", "toReversed");
    }
}
