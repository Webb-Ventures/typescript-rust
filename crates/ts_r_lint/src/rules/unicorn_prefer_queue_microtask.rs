//! `unicorn/prefer-queue-microtask`: `process.nextTick` (the global `process`, not the
//! callee of an optional call), and with options `setImmediate(cb)` / `setTimeout(cb, 0)`
//! calls of the globals whose value is unused and whose first argument may be a function.
//! Fixes: `process.nextTick(cb)` with one non-spread argument and no comment inside the
//! member becomes `queueMicrotask(cb)`; `setImmediate(cb)` and `setTimeout(cb, 0)` (with
//! exactly those arguments) too, unless `cb` is a non-arrow function that uses `this`.

use super::prelude::*;
use super::unicorn_array_mutate::is_value_not_function;
use super::unicorn_ast::{contains_lexical_this, is_value_not_usable, remove_argument};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-queue-microtask",
        "Prefer `queueMicrotask()` over `process.nextTick()`, `setImmediate()`, and `setTimeout(…, 0)`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<PreferQueueMicrotask>,
};

#[derive(Debug, Default)]
struct PreferQueueMicrotask {
    check_set_immediate: bool,
    check_set_timeout: bool,
}

impl FromOptions for PreferQueueMicrotask {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(PreferQueueMicrotask {
            check_set_immediate: bool_field(o, "checkSetImmediate", false),
            check_set_timeout: bool_field(o, "checkSetTimeout", false),
        })
    }
}

fn this_bound_callback(cx: &Ctx<'_, '_>, cb: NodeId) -> bool {
    match *cx.node(cb) {
        Node::FunctionExpr { body: Some(b), .. } => contains_lexical_this(cx.ast(), b),
        _ => false,
    }
}

impl Rule for PreferQueueMicrotask {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Member, NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::Member {
                obj,
                optional: false,
                prop,
            } => {
                if ident_name(ast, obj) != Some("process")
                    || ident_name(ast, prop) != Some("nextTick")
                    || !cx.semantic().refers_to_global(obj)
                {
                    return;
                }
                let call = cx.parent(node).filter(
                    |&p| matches!(*ast.node(p), Node::Call { callee, .. } if callee == node),
                );
                if call.is_some_and(|c| matches!(*ast.node(c), Node::Call { optional: true, .. })) {
                    return;
                }
                let span = cx.span(node);
                let fix = call
                    .and_then(|c| match *ast.node(c) {
                        Node::Call { args, .. } => match ast.list(args) {
                            [a] if !matches!(ast.node(*a), Node::Spread { .. }) => Some(()),
                            _ => None,
                        },
                        _ => None,
                    })
                    .filter(|_| ast.comments_in(span).is_empty())
                    .map(|_| Fix::replace(span, "queueMicrotask"));
                cx.report(
                    cx.span(prop),
                    "Prefer `queueMicrotask()` over `process.nextTick()`.",
                )
                .id("prefer-queue-microtask")
                .fix_opt(fix);
            }
            Node::Call {
                callee,
                optional: false,
                args,
                ..
            } => {
                let name = match ident_name(ast, callee) {
                    Some(n @ "setImmediate") if self.check_set_immediate => n,
                    Some(n @ "setTimeout") if self.check_set_timeout => n,
                    _ => return,
                };
                let args = ast.list(args);
                let Some(&cb) = args.first() else {
                    return;
                };
                let cb = skip_parens(ast, cb);
                if name == "setTimeout"
                    && !matches!(args.get(1).map(|&a| ast.node(skip_parens(ast, a))), Some(Node::NumberLit { value }) if value.get() == 0.0)
                {
                    return;
                }
                if matches!(ast.node(cb), Node::Spread { .. })
                    || is_value_not_function(cx, cb)
                    || !is_value_not_usable(cx, node)
                    || !cx.semantic().refers_to_global(callee)
                {
                    return;
                }
                let wanted = if name == "setTimeout" { 2 } else { 1 };
                let fix = (args.len() == wanted && !this_bound_callback(cx, cb)).then(|| {
                    let rename = Fix::replace(cx.span(callee), "queueMicrotask");
                    if name == "setTimeout" {
                        let mut f = rename;
                        if let Some(r) = remove_argument(cx, node, 1) {
                            for e in r.edits {
                                f = f.and(e.span, e.text);
                            }
                        }
                        f
                    } else {
                        rename
                    }
                });
                cx.report(
                    cx.span(callee),
                    format!("Prefer `queueMicrotask()` over `{name}()`."),
                )
                .id("prefer-queue-microtask")
                .fix_opt(fix);
            }
            _ => {}
        }
    }
}
