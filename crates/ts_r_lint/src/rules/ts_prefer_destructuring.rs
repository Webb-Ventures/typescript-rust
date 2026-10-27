//! `@typescript-eslint/prefer-destructuring`: the core rule, with types for `x[0]`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/prefer-destructuring",
        "Require destructuring from arrays and/or objects",
        NO_PRESET,
        Category::Style,
    )
    .typed()
    .fixable(),
    create: |opts| {
        let mut r = super::prefer_destructuring::PreferDestructuring::from_options(opts)?;
        r.ts = true;
        Ok(Box::new(r))
    },
};
