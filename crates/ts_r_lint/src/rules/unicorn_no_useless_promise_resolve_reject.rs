//! `unicorn/no-useless-promise-resolve-reject`: `Promise.resolve(x)` / `Promise.reject(x)`
//! (optionally awaited) as a concise arrow body, a `return` argument or a `yield` argument,
//! inside an async function or a non-generator `.then`/`.catch`/`.finally` callback.
//!
//! Fixes (at most one argument, no spread, no comment lost outside the argument):
//! `return Promise.resolve(x)` -> `return x;`, `yield x`, a concise body `x` (wrapped when
//! it would start with `{`, is a sequence or assignment, or holds `in` inside a `for`
//! initializer; `{}` with no argument); `await Promise.resolve(x)` -> `await x` (wrapped
//! below unary precedence; `undefined` with no argument). `Promise.reject(x)` becomes
//! `throw x;` (`{ throw x; }` as an arrow body, `throw undefined` with no argument), not
//! inside a `try` and, for `yield`, only when the `yield` is the whole statement.

use super::prelude::*;
use super::unicorn_text::paren_span;
use ts_r_ast::FnFlags;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-useless-promise-resolve-reject",
        "Disallow returning/yielding `Promise.resolve/reject()` in async functions or promise callbacks",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoUselessPromiseResolveReject>,
};

#[derive(Debug, Default)]
struct NoUselessPromiseResolveReject;

/// `fun` is the only argument of `.then`/`.catch`/`.finally`, or either of `.then`'s two.
fn is_promise_callback(cx: &Ctx<'_, '_>, fun: NodeId) -> bool {
    let ast = cx.ast();
    let Some(call) = parent_skip_parens(ast, cx.file.parents, fun) else {
        return false;
    };
    let Node::Call { callee, args, .. } = *ast.node(call) else {
        return false;
    };
    let Node::Member { prop, .. } = *ast.node(callee) else {
        return false;
    };
    let Some(method) = ident_name(ast, prop) else {
        return false;
    };
    let args: Vec<NodeId> = ast
        .list(args)
        .iter()
        .map(|&a| skip_parens(ast, a))
        .collect();
    match args.as_slice() {
        [only] => matches!(method, "then" | "catch" | "finally") && *only == fun,
        [first, second] => {
            method == "then"
                && (*first == fun
                    || (!matches!(ast.node(*first), Node::Spread { .. }) && *second == fun))
        }
        _ => false,
    }
}

/// The text of `arg` (parentheses dropped unless they hold comments), wrapped in
/// parentheses when `wrap`.
fn expression_text(cx: &Ctx<'_, '_>, arg: NodeId, wrap: bool) -> String {
    let inner = skip_parens(cx.ast(), arg);
    let outer = cx.span(arg);
    let inner_span = cx.span(inner);
    let comments_in_parens = cx
        .ast()
        .comments_in(outer)
        .iter()
        .any(|c| c.span.end <= inner_span.start || c.span.start >= inner_span.end);
    if inner != arg && comments_in_parens {
        return outer.text(cx.src()).to_owned();
    }
    let text = cx.text(inner);
    if wrap {
        format!("({text})")
    } else {
        text.to_owned()
    }
}

fn call_argument_text(cx: &Ctx<'_, '_>, arg: NodeId) -> String {
    let seq = matches!(cx.node(skip_parens(cx.ast(), arg)), Node::Seq { .. });
    expression_text(cx, arg, seq)
}

fn in_for_init(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let mut cur = node;
    while let Some(p) = cx.parent(cur) {
        if matches!(*cx.node(p), Node::For { init: Some(i), .. } if i == cur) {
            return true;
        }
        cur = p;
    }
    false
}

fn has_in_operator(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let span = cx.span(node);
    (1..ast.node_count() as u32)
        .filter_map(NodeId::from_raw)
        .any(|id| {
            let s = ast.span(id);
            s.start >= span.start
                && s.end <= span.end
                && matches!(
                    ast.node(id),
                    Node::Binary {
                        op: BinaryOp::In,
                        ..
                    } | Node::ForIn { .. }
                )
        })
}

fn concise_body_text(cx: &Ctx<'_, '_>, arg: NodeId) -> String {
    let inner = skip_parens(cx.ast(), arg);
    let wrap = cx.text(inner).starts_with('{')
        || matches!(cx.node(inner), Node::Seq { .. } | Node::Assign { .. })
        || (in_for_init(cx, inner) && has_in_operator(cx, inner));
    expression_text(cx, arg, wrap)
}

/// A comment inside `target` that is not inside `kept` (the argument).
fn loses_comments(cx: &Ctx<'_, '_>, target: Span, kept: Option<Span>) -> bool {
    cx.ast()
        .comments_in(target)
        .iter()
        .any(|c| !kept.is_some_and(|k| c.span.start >= k.start && c.span.end <= k.end))
}

impl Rule for NoUselessPromiseResolveReject {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, call: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call {
            callee,
            optional: false,
            args,
            ..
        } = *ast.node(call)
        else {
            return;
        };
        let Node::Member {
            obj,
            optional: false,
            prop,
        } = *ast.node(callee)
        else {
            return;
        };
        let is_reject = match ident_name(ast, prop) {
            Some("reject") => true,
            Some("resolve") => false,
            _ => return,
        };
        if ident_name(ast, obj) != Some("Promise") {
            return;
        }
        let parents = cx.file.parents;
        let Some(mut parent) = parent_skip_parens(ast, parents, call) else {
            return;
        };
        let mut expression = call;
        let mut awaited = false;
        if let Node::Await { arg } = *ast.node(parent)
            && skip_parens(ast, arg) == call
        {
            expression = parent;
            awaited = true;
            let Some(p) = parent_skip_parens(ast, parents, parent) else {
                return;
            };
            parent = p;
        }
        let is_yield = match *ast.node(parent) {
            Node::Arrow { body, .. } if skip_parens(ast, body) == expression => false,
            Node::Return { arg: Some(a) } if skip_parens(ast, a) == expression => false,
            Node::Yield {
                delegate: false,
                arg: Some(a),
            } if skip_parens(ast, a) == expression => true,
            _ => return,
        };
        // The enclosing function, and whether a `try` lies between.
        let mut in_try = false;
        let mut function = None;
        for a in ancestors_inclusive(parents, call) {
            match *ast.node(a) {
                Node::FunctionDecl { flags, .. }
                | Node::FunctionExpr { flags, .. }
                | Node::Arrow { flags, .. } => {
                    function = Some((a, flags));
                    break;
                }
                Node::Try { .. } => in_try = true,
                _ => {}
            }
        }
        let Some((function, flags)) = function else {
            return;
        };
        if !(flags.contains(FnFlags::ASYNC)
            || (!flags.contains(FnFlags::GENERATOR) && is_promise_callback(cx, function)))
        {
            return;
        }
        let keyword = if is_yield { "yield" } else { "return" };
        let message = if is_reject {
            format!("Prefer `throw error` over `{keyword} Promise.reject(error)`.")
        } else {
            format!("Prefer `{keyword} value` over `{keyword} Promise.resolve(value)`.")
        };
        let fix = self.fix(
            cx, call, args, parent, expression, awaited, is_reject, is_yield, in_try,
        );
        cx.report_node(callee, message)
            .id(if is_reject { "reject" } else { "resolve" })
            .fix_opt(fix);
    }
}

impl NoUselessPromiseResolveReject {
    #[allow(clippy::too_many_arguments)]
    fn fix(
        &self,
        cx: &Ctx<'_, '_>,
        call: NodeId,
        args: ts_r_ast::NodeList,
        parent: NodeId,
        expression: NodeId,
        awaited: bool,
        is_reject: bool,
        is_yield: bool,
        in_try: bool,
    ) -> Option<Fix> {
        let ast = cx.ast();
        let args = ast.list(args);
        if args.len() > 1 {
            return None;
        }
        let arg = args.first().copied();
        if arg.is_some_and(|a| matches!(ast.node(a), Node::Spread { .. })) {
            return None;
        }
        let arrow_body = matches!(ast.node(parent), Node::Arrow { .. });
        if is_reject && is_yield {
            let stmt = parent_skip_parens(ast, cx.file.parents, parent);
            if !stmt.is_some_and(|s| matches!(ast.node(s), Node::ExprStmt { .. })) {
                return None;
            }
        }
        if is_reject && in_try {
            return None;
        }
        let target = if awaited && !is_reject {
            paren_span(cx, call)
        } else if is_reject && is_yield {
            paren_span(cx, parent)
        } else if arrow_body {
            if is_reject {
                paren_span(cx, expression)
            } else {
                cx.span(call)
            }
        } else {
            cx.span(parent)
        };
        let kept = arg.map(|a| cx.span(skip_parens(ast, a)));
        if loses_comments(cx, target, kept) {
            return None;
        }
        let text = arg.map(|a| call_argument_text(cx, a)).unwrap_or_default();
        let replacement = if is_reject {
            let value = if text.is_empty() { "undefined" } else { &text };
            if is_yield {
                format!("throw {value}")
            } else if arrow_body {
                format!("{{ throw {value}; }}")
            } else {
                format!("throw {value};")
            }
        } else if awaited {
            match arg {
                None => "undefined".to_owned(),
                Some(a) => {
                    let inner = skip_parens(ast, a);
                    if !matches!(ast.node(inner), Node::Seq { .. }) && precedence(ast, inner) < 16 {
                        format!("({text})")
                    } else {
                        text
                    }
                }
            }
        } else if is_yield {
            if text.is_empty() {
                "yield".to_owned()
            } else {
                format!("yield {text}")
            }
        } else if arrow_body {
            arg.map_or_else(|| "{}".to_owned(), |a| concise_body_text(cx, a))
        } else if text.is_empty() {
            "return;".to_owned()
        } else {
            format!("return {text};")
        };
        Some(Fix::replace(target, replacement))
    }
}
