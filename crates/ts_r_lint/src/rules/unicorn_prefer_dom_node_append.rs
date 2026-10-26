//! `unicorn/prefer-dom-node-append`: `Element#append()` over `Node#appendChild()`.
//!
//! A call `x.appendChild(y)` (exactly one argument, not an optional call, non-computed)
//! whose receiver and argument can be DOM nodes (not literals, functions, classes, arrays,
//! objects, templates or `undefined`) is reported as a whole. The rename is a fix only when
//! the call's value is unused (`append` returns `undefined`).

use super::prelude::*;
use super::unicorn_util::{is_value_not_dom_node, is_value_not_usable};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-dom-node-append",
        "Prefer `Element#append()` over `Node#appendChild()`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferDomNodeAppend>,
};

#[derive(Debug, Default)]
struct PreferDomNodeAppend;

impl Rule for PreferDomNodeAppend {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call {
            callee,
            optional: false,
            args,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        let Node::Member { obj, prop, .. } = *ast.node(skip_parens(ast, callee)) else {
            return;
        };
        let &[arg] = ast.list(args) else {
            return;
        };
        if ident_name(ast, prop) != Some("appendChild")
            || matches!(ast.node(arg), Node::Spread { .. })
            || is_value_not_dom_node(ast, obj)
            || is_value_not_dom_node(ast, arg)
        {
            return;
        }
        let fix = is_value_not_usable(cx, node).then(|| Fix::replace(ast.span(prop), "append"));
        cx.report_node(node, "Prefer `Element#append()` over `Node#appendChild()`.")
            .id("prefer-dom-node-append")
            .fix_opt(fix);
    }
}
