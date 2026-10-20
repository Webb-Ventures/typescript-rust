//! `@typescript-eslint/no-dupe-class-members`: disallow duplicate class members,
//! ignoring overload signatures.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-dupe-class-members",
        "Disallow duplicate class members",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<super::no_dupe_class_members::NoDupeClassMembers>,
};
