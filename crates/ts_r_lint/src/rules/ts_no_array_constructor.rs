//! `@typescript-eslint/no-array-constructor`: disallow generic `Array` constructors.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-array-constructor",
        "Disallow generic `Array` constructors",
        TS_REC,
        Category::Style,
    )
    .fixable(),
    create: super::no_array_constructor::create_ts,
};
