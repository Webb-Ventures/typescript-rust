//! `@typescript-eslint/no-useless-constructor`: disallow unnecessary constructors.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-useless-constructor",
        "Disallow unnecessary constructors",
        TS_STRICT,
        Category::Suspicious,
    )
    .suggestions(),
    create: create_default::<super::no_useless_constructor::NoUselessConstructor>,
};
