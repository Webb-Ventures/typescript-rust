//! `unicorn/no-late-event-control`: `preventDefault()`, `stopPropagation()` and
//! `stopImmediatePropagation()` only work while the event is dispatched.
//!
//! The call is `<param>.<method>()` (`?.`, a computed key with a static string value and TS
//! wrappers around `<param>` allowed) where `<param>` is a parameter that is not known to
//! be something other than an event and is named like one (`e`, `event`, `evt`, `*Event`)
//! or typed as one (`unicorn_no_late_current_target_access::EVENT`). Reported, in this
//! order of message: inside a function nested in the handler; anywhere in a generator
//! handler; in the handler after one of its `await`, `yield` or `for await` has finished
//! (the point ends before the call ends and doesn't contain it, so `f(await x)` counts), or
//! inside a loop part that runs again after one.

use super::prelude::*;
use super::unicorn_no_late_current_target_access::{
    EVENT, enclosing_function, is_event_name, is_repeated_part, suspension_points,
};
use super::unicorn_static::{Static, static_value};
use super::unicorn_util::skip_wrappers;
use crate::jsdoc_util::fn_flags;
use ts_r_ast::FnFlags;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-late-event-control",
        "Disallow event-control method calls after the synchronous event dispatch has finished.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoLateEventControl>,
};

#[derive(Debug, Default)]
struct NoLateEventControl;

const METHODS: &[&str] = &[
    "preventDefault",
    "stopImmediatePropagation",
    "stopPropagation",
];

impl Rule for NoLateEventControl {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call { callee, .. } = *ast.node(node) else {
            return;
        };
        let (obj, method) = match *ast.node(skip_parens(ast, callee)) {
            Node::Member { obj, prop, .. } => (obj, ident_name(ast, prop).map(str::to_owned)),
            Node::Index { obj, index, .. } => match static_value(cx, index) {
                Some(Static::Str(s)) => (obj, Some(s)),
                _ => return,
            },
            _ => return,
        };
        let Some(method) = method.filter(|m| METHODS.contains(&m.as_str())) else {
            return;
        };
        let event = skip_wrappers(ast, obj);
        if !matches!(ast.node(event), Node::Ident { .. }) {
            return;
        }
        let Some(name) = ident_name(ast, event) else {
            return;
        };
        let sem = cx.semantic();
        let Some(sym) = sem
            .reference_at(event)
            .and_then(|r| sem.reference(r).symbol())
        else {
            return;
        };
        let Some(decl) = sem
            .symbol(sym)
            .decls
            .iter()
            .find(|d| d.kind == DeclKind::Param)
        else {
            return;
        };
        let Some(handler) = enclosing_function(cx, decl.name) else {
            return;
        };
        if EVENT.is_known_non_target(cx, event)
            || !(is_event_name(name) || EVENT.is_target(cx, event))
        {
            return;
        }
        let nested = enclosing_function(cx, node) != Some(handler);
        let generator = fn_flags(ast, handler).contains(FnFlags::GENERATOR);
        if !nested && !generator {
            let span = cx.span(node);
            let mut points = Vec::new();
            suspension_points(ast, handler, &mut points);
            let suspended = points.iter().any(|&p| {
                let p = cx.span(p);
                p.end <= span.end && !(p.start <= span.start && span.end <= p.end)
            });
            let in_loop = || {
                let mut child = node;
                while let Some(cur) = cx.parent(child) {
                    if cur == handler {
                        break;
                    }
                    if is_repeated_part(ast, cur, child) {
                        let mut inner = Vec::new();
                        suspension_points(ast, cur, &mut inner);
                        if !inner.is_empty() {
                            return true;
                        }
                    }
                    child = cur;
                }
                false
            };
            if !suspended && !in_loop() {
                return;
            }
        }
        let (id, message) = if nested {
            (
                "in-nested-function",
                format!(
                    "`{name}.{method}()` may run after synchronous event dispatch has finished. Call it in the outer handler before deferring work."
                ),
            )
        } else if generator {
            (
                "in-generator",
                format!(
                    "`{name}.{method}()` is not called during event dispatch inside a generator function. Use a normal function handler instead."
                ),
            )
        } else {
            (
                "after-suspension",
                format!(
                    "`{name}.{method}()` has no effect after the handler suspends. Call it before `await` or `yield` while the event is still being dispatched."
                ),
            )
        };
        cx.report(cx.span(node), message).id(id);
    }
}
