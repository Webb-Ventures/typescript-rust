//! `@typescript-eslint/no-use-before-define`: typescript-eslint's variant of the core rule.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-use-before-define",
        "Disallow the use of variables before they are defined",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create::<super::no_use_before_define::NoUseBeforeDefine>,
};
