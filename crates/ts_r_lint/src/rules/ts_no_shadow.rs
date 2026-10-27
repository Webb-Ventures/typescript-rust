//! `@typescript-eslint/no-shadow`: the core rule with typescript-eslint's defaults.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-shadow",
        "Disallow variable declarations from shadowing variables declared in the outer scope",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create::<super::no_shadow::NoShadow>,
};
