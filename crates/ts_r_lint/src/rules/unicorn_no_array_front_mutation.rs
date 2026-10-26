//! `unicorn/no-array-front-mutation`: `x.shift()` and `x.unshift(...)` (plain member, no
//! optional call), reported on the method name. Skipped for receivers known not to be
//! arrays, and for `unshift` on `stream`, `this`, `this.stream` and `process.std{in,out,err}`
//! (streams have an `unshift` of their own).

use super::prelude::*;
use super::unicorn_types::should_skip_known_non_array_receiver;
use super::unicorn_util::{is_method_call, node_matches_path};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-array-front-mutation",
        "Disallow front-of-array mutation.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoArrayFrontMutation>,
};

#[derive(Debug, Default)]
struct NoArrayFrontMutation;

const STREAM_RECEIVERS: &[&str] = &[
    "stream",
    "this",
    "this.stream",
    "process.stdin",
    "process.stdout",
    "process.stderr",
];

impl Rule for NoArrayFrontMutation {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        if !is_method_call(ast, node, &["shift", "unshift"], None, Some(false)) {
            return;
        }
        let Node::Call { callee, .. } = *cx.node(node) else {
            return;
        };
        let Node::Member { obj, prop, .. } = *ast.node(callee) else {
            return;
        };
        let Some(method) = ident_name(ast, prop) else {
            return;
        };
        if (method == "unshift"
            && STREAM_RECEIVERS
                .iter()
                .any(|p| node_matches_path(ast, obj, p)))
            || should_skip_known_non_array_receiver(cx, obj)
        {
            return;
        }
        let msg = format!("Avoid front-of-array mutation with `Array#{method}()`.");
        cx.report_node(prop, msg).id("no-array-front-mutation");
    }
}
