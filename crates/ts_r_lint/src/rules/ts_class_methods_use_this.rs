//! `@typescript-eslint/class-methods-use-this`: the core rule with
//! `ignoreClassesThatImplementAnInterface` (accessibility only) and `ignoreOverrideMethods`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/class-methods-use-this",
        "Enforce that class methods utilize `this`",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<super::class_methods_use_this::ClassMethodsUseThis>,
};
