//! `prefer-promise-reject-errors`: shares the typescript-eslint rule's implementation (no types).

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "prefer-promise-reject-errors",
        "Require using Error objects as Promise rejection reasons",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: |opts| {
        let mut r =
            super::ts_prefer_promise_reject_errors::PreferPromiseRejectErrors::from_options(opts)?;
        r.core = true;
        Ok(Box::new(r))
    },
};
