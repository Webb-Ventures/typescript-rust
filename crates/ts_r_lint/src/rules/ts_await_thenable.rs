//! `@typescript-eslint/await-thenable`: disallow awaiting a value that is not a thenable
//! (and `for await` / `await using` / promise aggregators on values that cannot be awaited).

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/await-thenable",
        "Disallow awaiting a value that is not a Thenable",
        TS_REC_TC,
        Category::Correctness,
    )
    .typed()
    .suggestions(),
    create: create_default::<AwaitThenable>,
};

#[derive(Debug, Default)]
struct AwaitThenable;

const AWAIT: &str = "Unexpected `await` of a non-Promise (non-\"Thenable\") value.";
const REMOVE_AWAIT: &str = "Remove unnecessary `await`.";
const AGGREGATOR: &str =
    "Unexpected iterable of non-Promise (non-\"Thenable\") values passed to promise aggregator.";

impl Rule for AwaitThenable {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Await,
            NodeKind::ForOf,
            NodeKind::VarDecl,
            NodeKind::Call,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::Await { arg } => check_await(node, arg, cx),
            Node::ForOf {
                is_await: true,
                right,
                ..
            } => check_for_await(node, right, cx),
            Node::VarDecl {
                kind: VarKind::AwaitUsing,
                decls,
                ..
            } => check_await_using(node, decls, cx),
            Node::Call { callee, args, .. } => check_aggregator(callee, args, cx),
            _ => {}
        }
    }
}

fn check_await(node: NodeId, arg: NodeId, cx: &mut Ctx<'_, '_>) {
    let ti = cx.types();
    let Some(t) = ti.type_at(arg) else { return };
    if thenable(ti, t) != Some(false) {
        return;
    }
    let fix = remove_await_fix(cx, node, arg);
    cx.report_node(node, AWAIT)
        .id("await")
        .suggest("removeAwait", REMOVE_AWAIT, fix);
}

/// Removes `await` (and the whitespace after it) from `await <arg>`, parenthesising the
/// argument where it would otherwise parse differently.
pub(crate) fn remove_await_fix(cx: &Ctx<'_, '_>, node: NodeId, arg: NodeId) -> Fix {
    let ast = cx.ast();
    let start = cx.span(node).start;
    let fix = Fix::delete(keyword_and_space(cx.src(), start, "await"));
    let first = leftmost_expr(ast, arg);
    if needs_parens_at(ast, expr_start(ast, cx.file.parents, node), first) {
        let span = cx.span(arg);
        return fix.and(span, format!("({})", span.text(cx.src())));
    }
    fix
}

fn check_for_await(node: NodeId, right: NodeId, cx: &mut Ctx<'_, '_>) {
    let ti = cx.types();
    let Some(t) = ti.type_at(right) else { return };
    let Some(name) = ti.well_known_symbol("asyncIterator") else {
        return;
    };
    if has_property_somewhere(ti, t, &name) != Some(false) {
        return;
    }
    let src = cx.src();
    let start = cx.span(node).start;
    let Some(end) = find_char(src, cx.span(right).end, b')').map(|p| p + 1) else {
        return;
    };
    // `for await (` -> `for (`.
    let Some(kw) = src[start as usize..end as usize]
        .find("await")
        .map(|i| start + i as u32)
    else {
        return;
    };
    let fix = Fix::delete(keyword_and_space(src, kw, "await"));
    cx.report(
        Span::new(start, end),
        "Unexpected `for await...of` of a value that is not async iterable.",
    )
    .id("forAwaitOfNonAsyncIterable")
    .suggest(
        "convertToOrdinaryFor",
        "Convert to an ordinary `for...of` loop.",
        fix,
    );
}

fn check_await_using(node: NodeId, decls: ts_r_ast::NodeList, cx: &mut Ctx<'_, '_>) {
    let ti = cx.types();
    let Some(name) = ti.well_known_symbol("asyncDispose") else {
        return;
    };
    let list = cx.ast().list(decls);
    let single = list.len() == 1;
    for &d in list {
        let Node::VarDeclarator {
            init: Some(init), ..
        } = *cx.node(d)
        else {
            continue;
        };
        let Some(t) = ti.type_at(init) else { continue };
        if has_property_somewhere(ti, t, &name) != Some(false) {
            continue;
        }
        let msg = "Unexpected `await using` of a value that is not async disposable.";
        if single {
            let fix = Fix::delete(keyword_and_space(cx.src(), cx.span(node).start, "await"));
            cx.report_node(init, msg)
                .id("awaitUsingOfNonAsyncDisposable")
                .suggest("removeAwait", REMOVE_AWAIT, fix);
        } else {
            cx.report_node(init, msg)
                .id("awaitUsingOfNonAsyncDisposable");
        }
    }
}

/// Some union member of `t` (constraints followed) has property `name`: `None` when unknown
/// (`any`, `unknown`, unresolved).
fn has_property_somewhere(ti: &dyn TypeInfo, t: Ty, name: &str) -> Option<bool> {
    let mut unknown = false;
    for p in parts(ti, constrained(ti, t)) {
        let p = constrained(ti, p);
        if p == Ty::UNRESOLVED || is_any(ti, p) || is_unknown(ti, p) {
            unknown = true;
            continue;
        }
        if ti.property_of_type(p, name).is_some() {
            return Some(true);
        }
    }
    if unknown { None } else { Some(false) }
}

fn check_aggregator(callee: NodeId, args: ts_r_ast::NodeList, cx: &mut Ctx<'_, '_>) {
    let ast = cx.ast();
    let callee = skip_parens(ast, callee);
    let Node::Member { obj, prop, .. } = *cx.node(callee) else {
        return;
    };
    if !matches!(
        ident_name(ast, prop),
        Some("all" | "allSettled" | "race" | "any")
    ) {
        return;
    }
    let ti = cx.types();
    let Some(ot) = ti.type_at(obj) else { return };
    if !is_builtin_symbol_like(ti, ot, &["PromiseConstructor"]) {
        return;
    }
    let Some(&arg) = ast.list(args).first() else {
        return;
    };
    if matches!(cx.node(arg), Node::Spread { .. }) {
        return;
    }
    let inner = skip_parens(ast, arg);
    if let Node::ArrayLit { elements } = *cx.node(inner) {
        for &e in ast.list(elements) {
            let bad = match *cx.node(e) {
                Node::Hole {} => false,
                Node::Spread { arg } => ti.type_at(arg).is_some_and(|t| container_bad(ti, t)),
                // A literal element may be a thenable in some cases only.
                _ => ti
                    .type_at(e)
                    .is_some_and(|t| thenable(ti, t) == Some(false)),
            };
            if bad {
                cx.report_node(e, AGGREGATOR)
                    .id("invalidPromiseAggregatorInput");
            }
        }
        return;
    }
    let Some(t) = ti.type_at(arg) else { return };
    if container_bad(ti, t) {
        cx.report_node(arg, AGGREGATOR)
            .id("invalidPromiseAggregatorInput");
    }
}

/// Some union member of the iterable `t` yields a value that is certainly not a thenable.
fn container_bad(ti: &dyn TypeInfo, t: Ty) -> bool {
    for p in parts(ti, constrained(ti, t)) {
        let p = constrained(ti, p);
        if p == Ty::UNRESOLVED || is_any(ti, p) || is_unknown(ti, p) {
            continue;
        }
        let Some(e) = ti.iterated_type(p, false) else {
            continue;
        };
        if element_bad(ti, e) {
            return true;
        }
    }
    false
}

/// Some union member of an element type is certainly not a thenable.
fn element_bad(ti: &dyn TypeInfo, t: Ty) -> bool {
    parts(ti, t)
        .into_iter()
        .any(|p| thenable(ti, p) == Some(false))
}
