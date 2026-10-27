//! `@typescript-eslint/default-param-last`: the core rule, for TypeScript syntax.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/default-param-last",
        "Enforce default parameters to be last",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<super::default_param_last::DefaultParamLast>,
};
