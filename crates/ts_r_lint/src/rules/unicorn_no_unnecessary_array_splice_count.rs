//! `unicorn/no-unnecessary-array-splice-count`: `x.splice(i, x.length)` /
//! `x.toSpliced(i, Infinity)`; see [`super::unicorn_length_or_infinity`]. Receivers known
//! not to be arrays are skipped.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unnecessary-array-splice-count",
        "Disallow using `.length` or `Infinity` as the `deleteCount` or `skipCount` argument of `Array#{splice,toSpliced}()`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoUnnecessaryArraySpliceCount>,
};

#[derive(Debug, Default)]
struct NoUnnecessaryArraySpliceCount;

impl Rule for NoUnnecessaryArraySpliceCount {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        super::unicorn_length_or_infinity::check(
            cx,
            node,
            &["splice", "toSpliced"],
            true,
            "no-unnecessary-array-splice-count",
        );
    }
}
