//! `@typescript-eslint/no-unused-expressions`: the TypeScript-aware variant of
//! `no-unused-expressions` (type assertions and instantiation expressions).

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-unused-expressions",
        "Disallow unused expressions",
        TS_REC,
        Category::Suspicious,
    ),
    create: create::<super::no_unused_expressions::NoUnusedExpressions>,
};
