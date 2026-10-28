//! `@typescript-eslint/max-params`: the core rule, for TypeScript syntax.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/max-params",
        "Enforce a maximum number of parameters in function definitions",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<super::max_params::MaxParams>,
};
