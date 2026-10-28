//! `unicorn/no-object-as-default-parameter`: a function parameter whose default is a
//! non-empty object literal (through parentheses and TS wrappers like `as const`). A plain
//! name is reported with its name; a destructuring pattern on the object.

use super::prelude::*;
use super::unicorn_util::skip_wrappers;
use super::unicorn_util_h4::estree_ident_span;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-object-as-default-parameter",
        "Disallow the use of objects as default parameters.",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create_default::<NoObjectAsDefaultParameter>,
};

#[derive(Debug, Default)]
struct NoObjectAsDefaultParameter;

impl Rule for NoObjectAsDefaultParameter {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Param]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Param {
            pattern,
            init: Some(init),
            ..
        } = *cx.node(node)
        else {
            return;
        };
        if !cx.parent(node).is_some_and(|p| is_function(ast.kind(p))) {
            return;
        }
        let right = skip_wrappers(ast, init);
        let Node::ObjectLit { props } = *ast.node(right) else {
            return;
        };
        if ast.list(props).is_empty() {
            return;
        }
        if let Some(name) = ident_name(ast, pattern) {
            let message =
                format!("Do not use an object literal as default for parameter `{name}`.");
            let span = estree_ident_span(cx, pattern);
            cx.report(span, message).id("identifier");
        } else {
            cx.report_node(right, "Do not use an object literal as default.")
                .id("non-identifier");
        }
    }
}
