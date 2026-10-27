//! `logical-assignment-operators`: shares `unicorn/logical-assignment-operators`'s
//! implementation, with ESLint's single `||=` report for a falsy `if` assignment.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "logical-assignment-operators",
        "Require or disallow logical assignment operator shorthand",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: |opts| {
        Ok(Box::new(
            super::unicorn_logical_assignment_operators::LogicalAssignmentOperators::core(opts)?,
        ))
    },
};
