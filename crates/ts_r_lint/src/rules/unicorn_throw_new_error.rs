//! `unicorn/throw-new-error`: create errors with `new` (`new Error()`, `new lib.CustomError()`).
//!
//! A call is reported when its callee is a name, or a non-computed member's property,
//! shaped like `Error`, `TypeError` or `FooBar2Error` (capitalised words, then `Error`). It is
//! left alone when its value is used as a function: as a decorator, a template tag or the
//! callee of another call (`Schema.TaggedError()(…)`), and under optional chaining (`new`
//! can't be optional) and for Effect's `Data.TaggedError(…)`. The fix inserts `new`
//! ([`super::unicorn_util::call_to_new`]).

use super::prelude::*;
use super::unicorn_util::{call_to_new, has_optional_chain, is_error_name};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/throw-new-error",
        "Require `new` when creating an error.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<ThrowNewError>,
};

#[derive(Debug, Default)]
struct ThrowNewError;

impl Rule for ThrowNewError {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call { callee, .. } = *cx.node(node) else {
            return;
        };
        if let Some(p) = parent_skip_parens(ast, cx.file.parents, node) {
            let used_as_function = match *ast.node(p) {
                Node::Decorator { .. } => true,
                Node::TaggedTemplate { tag, .. } => skip_parens(ast, tag) == node,
                Node::Call { callee, .. } => skip_parens(ast, callee) == node,
                _ => false,
            };
            if used_as_function {
                return;
            }
        }
        if has_optional_chain(ast, node) {
            return;
        }
        let callee = skip_parens(ast, callee);
        let name = match *ast.node(callee) {
            Node::Ident { .. } => ident_name(ast, callee),
            Node::Member { obj, prop, .. } => {
                if ident_name(ast, prop) == Some("TaggedError")
                    && ident_name(ast, skip_parens(ast, obj)) == Some("Data")
                {
                    return;
                }
                ident_name(ast, prop)
            }
            _ => None,
        };
        if !name.is_some_and(is_error_name) {
            return;
        }
        let fix = call_to_new(cx, node);
        cx.report_node(node, "Use `new` when creating an error.")
            .id("throw-new-error")
            .fix(fix);
    }
}

#[cfg(test)]
mod tests {
    use super::super::unicorn_util::is_error_name;

    #[test]
    fn error_names() {
        for ok in [
            "Error",
            "TypeError",
            "ABCError",
            "Abc3Error",
            "FooBarBazError",
        ] {
            assert!(is_error_name(ok), "{ok}");
        }
        for bad in ["error", "myError", "Errors", "Foo_Error", "3Error"] {
            assert!(!is_error_name(bad), "{bad}");
        }
    }
}
