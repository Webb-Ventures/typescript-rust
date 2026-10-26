//! `unicorn/no-useless-iterator-to-array`: `x.toArray()` (plain, no arguments, `x` not known
//! to be a non-iterator) where an iterable would do. Reported at `toArray`:
//!
//! - first argument of `new Map/WeakMap/Set/WeakSet(..)` (only argument) or `new
//!   <TypedArray>(..)`, `Array.from` / `<TypedArray>.from`, `Object.fromEntries`
//!   (`iterable-accepting`): a fix with one argument, a suggestion with more;
//! - only argument of `Promise.all/allSettled/any/race` (a suggestion: a throw while
//!   iterating would become a rejection);
//! - a `for…of` iterable (`for-of`) or a `yield*` operand (`yield-star`): suggestions, since
//!   the array is filled before the loop starts;
//! - spread in an array, call or `new` (`spread`): a fix;
//! - receiver of `every/find/forEach/some` (at most one argument) or `reduce` (one or two)
//!   (`iterator-method`, a suggestion) unless the callback can read the array argument: not
//!   an inline function, too many parameters, a trailing rest, or `arguments` in a
//!   `function`.
//!
//! Removing `.toArray()` needs no comment outside the receiver in the call.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_iter::{is_known_non_iterator, unwrap_expression};
use super::unicorn_text::{paren_span, would_remove_comments};
use super::unicorn_util_h2::remove_method_call;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-useless-iterator-to-array",
        "Disallow unnecessary `.toArray()` on iterators.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<NoUselessIteratorToArray>,
};

#[derive(Debug, Default)]
struct NoUselessIteratorToArray;

const TYPED_ARRAYS: &[&str] = &[
    "Int8Array",
    "Uint8Array",
    "Uint8ClampedArray",
    "Int16Array",
    "Uint16Array",
    "Int32Array",
    "Uint32Array",
    "Float16Array",
    "Float32Array",
    "Float64Array",
    "BigInt64Array",
    "BigUint64Array",
];

const REMOVE: &str = "Remove `.toArray()`.";
const REMOVE_ID: &str = "iterable-accepting/suggestion";

enum Remedy {
    Fix,
    Suggest,
}

fn ident_text(ast: &Ast, id: NodeId) -> Option<&str> {
    let id = skip_parens(ast, id);
    matches!(ast.node(id), Node::Ident { .. })
        .then(|| ident_name(ast, id))
        .flatten()
}

fn has_spread(ast: &Ast, args: &[NodeId]) -> bool {
    args.iter()
        .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
}

/// `x.toArray()`: the call's `toArray` name.
fn to_array_call(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let call = method_call(ast, skip_parens(ast, id))?;
    (call.name == "toArray"
        && call.args.is_empty()
        && call.plain()
        && !is_known_non_iterator(cx, call.obj))
    .then_some(call.prop)
}

fn mentions_arguments(ast: &Ast, root: NodeId) -> bool {
    let mut stack = vec![root];
    let mut guard = 0u32;
    while let Some(n) = stack.pop() {
        guard += 1;
        if guard > 1_000_000 {
            return true;
        }
        if matches!(ast.node(n), Node::Ident { .. }) && ident_name(ast, n) == Some("arguments") {
            return true;
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    false
}

fn can_observe_array(ast: &Ast, callback: Option<NodeId>, index: usize) -> bool {
    let Some(callback) = callback else {
        return false;
    };
    let f = unwrap_expression(ast, callback);
    let params = match *ast.node(f) {
        Node::Arrow { params, .. } | Node::FunctionExpr { params, .. } => ast.list(params),
        _ => return true,
    };
    params.len() > index
        || params
            .last()
            .is_some_and(|&p| matches!(ast.node(p), Node::Param { rest: true, .. }))
        || (matches!(ast.node(f), Node::FunctionExpr { .. }) && mentions_arguments(ast, f))
}

fn report(
    cx: &mut Ctx<'_, '_>,
    to_array_id: NodeId,
    prop: NodeId,
    id: &'static str,
    message: String,
    remedy: Remedy,
    suggestion: (&'static str, String),
) {
    let ast = cx.ast();
    let call = skip_parens(ast, to_array_id);
    let Node::Call { callee, .. } = *ast.node(call) else {
        return;
    };
    let Node::Member { obj, .. } = *ast.node(skip_parens(ast, callee)) else {
        return;
    };
    let fix = (!would_remove_comments(cx, cx.span(call), &[paren_span(cx, obj)]))
        .then(|| remove_method_call(cx, call, Fix::default()));
    let r = cx.report(cx.span(prop), message).id(id);
    if let Some(f) = fix {
        match remedy {
            Remedy::Fix => {
                r.fix(f);
            }
            Remedy::Suggest => {
                r.suggest(suggestion.0, suggestion.1, f);
            }
        }
    }
}

fn accepting(cx: &mut Ctx<'_, '_>, arg: NodeId, prop: NodeId, description: String, remedy: Remedy) {
    report(
        cx,
        arg,
        prop,
        "iterable-accepting",
        format!("`{description}` accepts an iterable, `.toArray()` is unnecessary."),
        remedy,
        (REMOVE_ID, REMOVE.to_owned()),
    );
}

impl NoUselessIteratorToArray {
    fn check_new(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::New { callee, args, .. } = *ast.node(id) else {
            return;
        };
        let args = ast.list(args);
        let Some(name) = ident_text(ast, callee) else {
            return;
        };
        let matches = !has_spread(ast, args)
            && ((["Map", "WeakMap", "Set", "WeakSet"].contains(&name) && args.len() == 1)
                || (TYPED_ARRAYS.contains(&name) && !args.is_empty()));
        if !matches {
            return;
        }
        let Some(prop) = to_array_call(cx, args[0]) else {
            return;
        };
        let remedy = if args.len() == 1 {
            Remedy::Fix
        } else {
            Remedy::Suggest
        };
        accepting(cx, args[0], prop, format!("new {name}(…)"), remedy);
    }

    fn check_call(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, id) else {
            return;
        };
        if !call.plain() || has_spread(ast, call.args) {
            return;
        }
        let object = ident_text(ast, call.obj);
        let static_call = match (object, call.name) {
            (Some(o), "from") if o == "Array" || TYPED_ARRAYS.contains(&o) => Some((o, false)),
            (Some("Object"), "fromEntries") => Some(("Object", false)),
            (Some("Promise"), "all" | "allSettled" | "any" | "race") => Some(("Promise", true)),
            _ => None,
        };
        if let Some((o, promise)) = static_call {
            if call.args.is_empty() || (promise && call.args.len() != 1) {
                return;
            }
            let Some(prop) = to_array_call(cx, call.args[0]) else {
                return;
            };
            let remedy = if call.args.len() == 1 && !promise {
                Remedy::Fix
            } else {
                Remedy::Suggest
            };
            accepting(
                cx,
                call.args[0],
                prop,
                format!("{o}.{}(…)", call.name),
                remedy,
            );
            return;
        }
        let index = match call.name {
            "every" | "find" | "forEach" | "some" if call.args.len() <= 1 => 2,
            "reduce" if (1..=2).contains(&call.args.len()) => 3,
            _ => return,
        };
        let Some(prop) = to_array_call(cx, call.obj) else {
            return;
        };
        if can_observe_array(ast, call.args.first().copied(), index) {
            return;
        }
        let method = call.name;
        report(
            cx,
            call.obj,
            prop,
            "iterator-method",
            format!("`Iterator` has a `.{method}()` method, `.toArray()` is unnecessary."),
            Remedy::Suggest,
            (
                "iterator-method/suggestion",
                format!("Remove `.toArray()` and use `Iterator#{method}()`."),
            ),
        );
    }
}

impl Rule for NoUselessIteratorToArray {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::New,
            NodeKind::Call,
            NodeKind::ForOf,
            NodeKind::Yield,
            NodeKind::Spread,
        ]
    }
    fn enter(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *ast.node(id) {
            Node::New { .. } => self.check_new(id, cx),
            Node::Call { .. } => self.check_call(id, cx),
            Node::ForOf { right, .. } => {
                if let Some(prop) = to_array_call(cx, right) {
                    report(
                        cx,
                        right,
                        prop,
                        "for-of",
                        "`for…of`/`for await…of` can iterate over an iterable, `.toArray()` is unnecessary.".to_owned(),
                        Remedy::Suggest,
                        (REMOVE_ID, REMOVE.to_owned()),
                    );
                }
            }
            Node::Yield {
                delegate: true,
                arg: Some(arg),
            } => {
                if let Some(prop) = to_array_call(cx, arg) {
                    report(
                        cx,
                        arg,
                        prop,
                        "yield-star",
                        "`yield*` can delegate to an iterable, `.toArray()` is unnecessary."
                            .to_owned(),
                        Remedy::Suggest,
                        (REMOVE_ID, REMOVE.to_owned()),
                    );
                }
            }
            Node::Spread { arg } => {
                let in_list = cx.parent(id).is_some_and(|p| {
                    matches!(
                        cx.node(p),
                        Node::ArrayLit { .. } | Node::Call { .. } | Node::New { .. }
                    )
                });
                if in_list && let Some(prop) = to_array_call(cx, arg) {
                    report(
                        cx,
                        arg,
                        prop,
                        "spread",
                        "Spread works on iterables, `.toArray()` is unnecessary.".to_owned(),
                        Remedy::Fix,
                        (REMOVE_ID, REMOVE.to_owned()),
                    );
                }
            }
            _ => {}
        }
    }
}
