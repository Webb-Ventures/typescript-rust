//! `@typescript-eslint/no-empty-function`: the core rule, which already knows the
//! TypeScript cases (parameter-property constructors, private / protected constructors,
//! decorated and `override` methods).

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-empty-function",
        "Disallow empty functions",
        TS_STYLISTIC,
        Category::Suspicious,
    )
    .suggestions(),
    create: create::<super::no_empty_function::NoEmptyFunction>,
};
