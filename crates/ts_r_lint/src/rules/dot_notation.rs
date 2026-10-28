//! `dot-notation`: shares the typescript-eslint rule's implementation (no types).

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "dot-notation",
        "Enforce dot notation whenever possible",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<super::ts_dot_notation::DotNotation>,
};
