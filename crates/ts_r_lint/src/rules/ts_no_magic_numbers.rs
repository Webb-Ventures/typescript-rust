//! `@typescript-eslint/no-magic-numbers`: the core rule; enums, literal types, indexed-access
//! types and readonly fields are decided by their own options first.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-magic-numbers",
        "Disallow magic numbers",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<super::no_magic_numbers::NoMagicNumbers>,
};
