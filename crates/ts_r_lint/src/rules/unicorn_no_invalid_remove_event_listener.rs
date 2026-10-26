//! `unicorn/no-invalid-remove-event-listener`: the listener (second argument) of
//! `x.removeEventListener(type, listener)` must be a reference to the function that was
//! added. A function or arrow expression is reported at its head, and a `.bind(...)` call
//! (also `['bind']`, not optional) at the `bind` property. A spread first argument or an
//! optional call is skipped.

use super::prelude::*;
use super::unicorn_ast::method_call;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-invalid-remove-event-listener",
        "Prevent calling `EventTarget#removeEventListener()` with the result of an expression.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoInvalidRemoveEventListener>,
};

#[derive(Debug, Default)]
struct NoInvalidRemoveEventListener;

const MSG: &str = "The listener argument should be a function reference.";

impl Rule for NoInvalidRemoveEventListener {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, node) else {
            return;
        };
        if call.name != "removeEventListener"
            || call.optional_call
            || call.args.len() < 2
            || matches!(ast.node(call.args[0]), Node::Spread { .. })
        {
            return;
        }
        let listener = skip_parens(ast, call.args[1]);
        match *ast.node(listener) {
            Node::Arrow { .. } | Node::FunctionExpr { .. } => {
                let span = function_head_span(ast, cx.src(), cx.file.parents, listener);
                cx.report(span, MSG).id("no-invalid-remove-event-listener");
            }
            Node::Call {
                callee,
                optional: false,
                ..
            } => {
                let callee = skip_parens(ast, callee);
                let prop = match *ast.node(callee) {
                    Node::Member {
                        optional: false,
                        prop,
                        ..
                    } => prop,
                    Node::Index {
                        optional: false,
                        index,
                        ..
                    } => index,
                    _ => return,
                };
                if member_name(ast, cx.src(), callee).as_deref() == Some("bind") {
                    cx.report_node(prop, MSG)
                        .id("no-invalid-remove-event-listener");
                }
            }
            _ => {}
        }
    }
}
