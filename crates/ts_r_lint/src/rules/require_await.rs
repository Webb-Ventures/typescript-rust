//! `require-await`: shares the typescript-eslint rule's implementation (no types).

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "require-await",
        "Disallow async functions which have no `await` expression",
        NO_PRESET,
        Category::Suspicious,
    )
    .suggestions(),
    create: |_| {
        Ok(Box::new(super::ts_require_await::RequireAwait {
            core: true,
        }))
    },
};
