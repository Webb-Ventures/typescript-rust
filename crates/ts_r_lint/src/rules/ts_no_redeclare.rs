//! `@typescript-eslint/no-redeclare`: disallow variable redeclaration, including types.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-redeclare",
        "Disallow variable redeclaration",
        NO_PRESET,
        Category::Correctness,
    ),
    create: super::no_redeclare::create_ts,
};
