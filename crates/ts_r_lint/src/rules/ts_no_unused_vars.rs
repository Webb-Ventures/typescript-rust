//! `@typescript-eslint/no-unused-vars`: the TypeScript-aware variant of `no-unused-vars`
//! (type references, declaration merging, ambient declarations, `usedOnlyAsType`).

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-unused-vars",
        "Disallow unused variables",
        TS_REC,
        Category::Correctness,
    )
    .suggestions()
    .fixable(),
    create: super::no_unused_vars::create_ts,
};
