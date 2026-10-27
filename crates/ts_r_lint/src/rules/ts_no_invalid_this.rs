//! `@typescript-eslint/no-invalid-this`: the core rule (a `this` parameter and class fields
//! make `this` valid there too).

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-invalid-this",
        "Disallow `this` keywords outside of classes or class-like objects",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create::<super::no_invalid_this::NoInvalidThis>,
};
