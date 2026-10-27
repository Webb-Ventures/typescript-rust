//! `@typescript-eslint/init-declarations`: the core rule, reported at the name when there is
//! no initializer.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/init-declarations",
        "Require or disallow initialization in variable declarations",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<super::init_declarations::InitDeclarations>,
};
