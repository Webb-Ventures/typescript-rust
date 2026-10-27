//! `no-invalid-this`: `this` where it is `undefined` (strict code outside methods,
//! constructors and callbacks given a `this`). Shared with `@typescript-eslint/no-invalid-this`.

use super::prelude::*;
use super::this_binding::{ThisScope, is_default_this_binding_with, is_strict, this_scope};
use crate::env::SourceType;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-invalid-this",
        "Disallow use of `this` in contexts where the value of `this` is `undefined`",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create::<NoInvalidThis>,
};

#[derive(Debug)]
pub(crate) struct NoInvalidThis {
    cap_is_constructor: bool,
}

impl FromOptions for NoInvalidThis {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoInvalidThis {
            cap_is_constructor: bool_field(obj_opt(opts, 0), "capIsConstructor", true),
        })
    }
}

impl Rule for NoInvalidThis {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::This]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let valid = match this_scope(cx, node) {
            ThisScope::Class => true,
            ThisScope::Program => {
                let env = cx.env();
                !(env.source_type == SourceType::Module
                    || (env.global_return && is_strict(cx, node)))
            }
            ThisScope::Function(f) => {
                !is_strict(cx, f) || !is_default_this_binding_with(cx, f, self.cap_is_constructor)
            }
        };
        if !valid {
            cx.report_node(node, "Unexpected 'this'.")
                .id("unexpectedThis");
        }
    }
}
