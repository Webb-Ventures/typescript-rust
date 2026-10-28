//! `@typescript-eslint/consistent-return`: the core rule, where bare returns of functions
//! returning `void` (or `Promise<void>`) don't count.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/consistent-return",
        "Require `return` statements to either always or never specify values",
        NO_PRESET,
        Category::Suspicious,
    )
    .typed(),
    create: |opts| {
        let mut r = super::consistent_return::ConsistentReturn::from_options(opts)?;
        r.ts = true;
        Ok(Box::new(r))
    },
};
