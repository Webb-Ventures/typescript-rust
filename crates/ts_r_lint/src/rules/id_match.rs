//! `id-match`: shares `unicorn/id-match`'s implementation (its extra
//! `checkNamedSpecifiers` option defaults to the core rule's behaviour).

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "id-match",
        "Require identifiers to match a specified regular expression",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<super::unicorn_id_match::IdMatch>,
};
