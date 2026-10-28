//! `unicorn/no-unnecessary-slice-end`: `x.slice(start, x.length)` / `Infinity`; see
//! [`super::unicorn_length_or_infinity`]. Any receiver (strings slice the same way).

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unnecessary-slice-end",
        "Disallow using `.length` or `Infinity` as the `end` argument of `{Array,String,TypedArray}#slice()`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoUnnecessarySliceEnd>,
};

#[derive(Debug, Default)]
struct NoUnnecessarySliceEnd;

impl Rule for NoUnnecessarySliceEnd {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        super::unicorn_length_or_infinity::check(
            cx,
            node,
            &["slice"],
            false,
            "no-unnecessary-slice-end",
        );
    }
}
