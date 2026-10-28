//! `unicorn/require-proxy-trap-boolean-return`: the traps whose result the Proxy uses as a
//! boolean (`defineProperty`, `deleteProperty`, `has`, `isExtensible`,
//! `preventExtensions`, `set`, `setPrototypeOf`) in the handler object literal of
//! `new Proxy(target, handler)` or `Proxy.revocable(target, handler)` must return a
//! boolean.
//!
//! An `async` or generator trap is reported. Otherwise every returned value (the arrow
//! body, or each `return` of the body outside nested functions) is checked: a bare
//! `return`; a compound assignment; through `=` (no fix), `?:`, `&&`/`||`/`??` (no fix) and
//! `,` (no fix); a value with a known non-boolean static value, or an expression that is
//! never a boolean (arrays, objects, functions, classes, `new`, templates, `++`, unary
//! operators other than `!`/`delete`, arithmetic and bitwise operators). A literal,
//! expression-free template or `undefined` is fixed to its truthiness. A body that can
//! fall off its end (by the control-flow graph, unless its last statement is
//! `process.exit()`) is reported on the function.

use super::prelude::*;
use super::unicorn_static::{
    Static, has_side_effectful_const_initializer, is_global_ident, mutable_member_access,
    skip_ts_only, static_value, static_value_no_side_effects,
};
use crate::cfg::Cfg;
use ts_r_ast::{Ast, FnFlags, MethodKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/require-proxy-trap-boolean-return",
        "Require boolean-returning Proxy traps to return booleans.",
        NO_PRESET,
        Category::Correctness,
    )
    .fixable(),
    create: create_default::<RequireProxyTrapBooleanReturn>,
};

#[derive(Debug, Default)]
struct RequireProxyTrapBooleanReturn;

const BOOLEAN_TRAPS: &[&str] = &[
    "defineProperty",
    "deleteProperty",
    "has",
    "isExtensible",
    "preventExtensions",
    "set",
    "setPrototypeOf",
];

struct Problem {
    node: NodeId,
    fix: Option<String>,
}

fn unwrap_ts(ast: &Ast, mut id: NodeId) -> NodeId {
    for _ in 0..1024 {
        let next = skip_ts_only(ast, skip_parens(ast, id));
        if next == id {
            break;
        }
        id = next;
    }
    id
}

/// The proxy handler of `new Proxy(a, b)` / `Proxy.revocable(a, b)`.
fn proxy_handler(ast: &Ast, node: NodeId) -> Option<NodeId> {
    let (callee, args, is_new) = match *ast.node(node) {
        Node::New { callee, args, .. } => (callee, args, true),
        Node::Call {
            callee,
            optional: false,
            args,
            ..
        } => (callee, args, false),
        _ => return None,
    };
    let args = ast.list(args);
    if args.len() != 2 || args.iter().any(|&a| ast.kind(a) == NodeKind::Spread) {
        return None;
    }
    let callee = skip_parens(ast, callee);
    let ok = if is_new {
        ident_name(ast, callee) == Some("Proxy")
    } else {
        matches!(*ast.node(callee), Node::Member { obj, optional: false, prop }
            if ident_name(ast, obj) == Some("Proxy") && ident_name(ast, prop) == Some("revocable"))
    };
    ok.then_some(args[1])
}

fn key_name(cx: &Ctx<'_, '_>, key: NodeId) -> Option<String> {
    let ast = cx.ast();
    match *ast.node(key) {
        Node::Ident { name } => Some(ast.atom(name).to_owned()),
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        Node::ComputedKey { expr } => static_value(cx, expr).and_then(|v| v.to_js_string()),
        _ => static_value(cx, key).and_then(|v| v.to_js_string()),
    }
}

/// A trap property of the handler: its name and function.
fn trap_function(cx: &Ctx<'_, '_>, prop: NodeId) -> Option<(String, NodeId)> {
    let ast = cx.ast();
    let (key, value) = match *ast.node(prop) {
        Node::Property { key, value, .. } => (key, skip_parens(ast, value)),
        Node::MethodDef {
            key,
            kind: MethodKind::Method,
            value,
            ..
        } => (key, value),
        _ => return None,
    };
    let name = key_name(cx, key)?;
    if !BOOLEAN_TRAPS.contains(&name.as_str())
        || !matches!(ast.kind(value), NodeKind::Arrow | NodeKind::FunctionExpr)
    {
        return None;
    }
    Some((name, value))
}

fn static_boolean_value(cx: &Ctx<'_, '_>, node: NodeId) -> Option<bool> {
    let ast = cx.ast();
    let inner = unwrap_ts(ast, node);
    if let Some(v) = static_value_no_side_effects(cx, inner) {
        return (!matches!(v, Static::Bool(_))).then(|| v.truthy());
    }
    if mutable_member_access(cx, inner, &mut Vec::new())
        || has_side_effectful_const_initializer(cx, inner)
    {
        return None;
    }
    let v = static_value(cx, inner)?;
    (!matches!(v, Static::Bool(_))).then(|| v.truthy())
}

fn is_known_non_boolean(ast: &Ast, node: NodeId) -> bool {
    match *ast.node(node) {
        Node::ArrayLit { .. }
        | Node::Arrow { .. }
        | Node::ClassExpr { .. }
        | Node::FunctionExpr { .. }
        | Node::New { .. }
        | Node::ObjectLit { .. }
        | Node::TemplateLit { .. }
        | Node::Update { .. } => true,
        Node::Unary { op, .. } => !matches!(op, UnaryOp::Not | UnaryOp::Delete),
        Node::Binary { op, .. } => matches!(
            op,
            BinaryOp::Mod
                | BinaryOp::BitAnd
                | BinaryOp::Mul
                | BinaryOp::Exp
                | BinaryOp::Add
                | BinaryOp::Sub
                | BinaryOp::Div
                | BinaryOp::Shl
                | BinaryOp::Shr
                | BinaryOp::UShr
                | BinaryOp::BitXor
                | BinaryOp::BitOr
        ),
        _ => false,
    }
}

fn without_fix(p: Option<Problem>) -> Option<Problem> {
    p.map(|p| Problem {
        node: p.node,
        fix: None,
    })
}

fn problem_of(cx: &Ctx<'_, '_>, node: NodeId, depth: u32) -> Option<Problem> {
    let ast = cx.ast();
    let node = skip_parens(ast, node);
    if depth > 256 {
        return None;
    }
    match *ast.node(node) {
        Node::Assign { op, right, .. } => {
            return if op == AssignOp::Assign {
                without_fix(problem_of(cx, right, depth + 1))
            } else {
                Some(Problem { node, fix: None })
            };
        }
        Node::Conditional { cons, alt, .. } => {
            return problem_of(cx, cons, depth + 1).or_else(|| problem_of(cx, alt, depth + 1));
        }
        Node::Binary {
            op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce,
            left,
            right,
        } => {
            return without_fix(
                problem_of(cx, left, depth + 1).or_else(|| problem_of(cx, right, depth + 1)),
            );
        }
        Node::Seq { exprs } => {
            let last = *ast.list(exprs).last()?;
            return without_fix(problem_of(cx, last, depth + 1));
        }
        _ => {}
    }
    let value = static_boolean_value(cx, node);
    if value.is_none() && !is_known_non_boolean(ast, node) {
        return None;
    }
    let span = cx.span(node);
    let replaceable = match *ast.node(node) {
        Node::NullLit {}
        | Node::BoolLit { .. }
        | Node::NumberLit { .. }
        | Node::BigIntLit { .. }
        | Node::StringLit { .. } => true,
        Node::TemplateLit { parts } => ast.list(parts).len() <= 1,
        Node::Ident { name } => ast.atom(name) == "undefined",
        _ => false,
    };
    let fix = (replaceable && ast.comments_in(span).is_empty())
        .then(|| value.map(|b| b.to_string()))
        .flatten();
    Some(Problem { node, fix })
}

/// The `return` statements of a body, outside nested functions.
fn returns(ast: &Ast, body: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack = vec![body];
    let mut kids = Vec::new();
    while let Some(id) = stack.pop() {
        match ast.kind(id) {
            NodeKind::Return => {
                out.push(id);
                continue;
            }
            NodeKind::Arrow | NodeKind::FunctionExpr | NodeKind::FunctionDecl => continue,
            _ => {}
        }
        kids.clear();
        ast.for_each_child(id, |c| kids.push(c));
        stack.extend(kids.iter().rev());
    }
    out
}

/// An optional link (`?.`) in the chain ending at `id`.
fn chain_has_optional(ast: &Ast, mut id: NodeId) -> bool {
    for _ in 0..4096 {
        match *ast.node(id) {
            Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                if optional {
                    return true;
                }
                id = obj;
            }
            Node::Call {
                callee, optional, ..
            } => {
                if optional {
                    return true;
                }
                id = callee;
            }
            Node::NonNull { expr } => id = expr,
            _ => return false,
        }
    }
    false
}

/// `id` is an argument or computed key of an optional chain (`a?.(x)`, `a?.b[x]`), which
/// may be skipped.
fn in_short_circuit(cx: &Ctx<'_, '_>, id: NodeId, func: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = id;
    while let Some(p) = cx.parent(cur) {
        if p == func {
            return false;
        }
        let skipped = match *ast.node(p) {
            Node::Call { callee, .. } => callee != cur && chain_has_optional(ast, p),
            Node::Index { obj, .. } => obj != cur && chain_has_optional(ast, p),
            _ => false,
        };
        if skipped {
            return true;
        }
        cur = p;
    }
    false
}

/// The `process.exit(...)` calls (on the global `process`) inside `func`, sorted.
pub(crate) fn process_exit_calls(cx: &Ctx<'_, '_>, func: NodeId) -> Vec<NodeId> {
    let ast = cx.ast();
    let mut out = Vec::new();
    let mut stack = vec![func];
    let mut kids = Vec::new();
    while let Some(id) = stack.pop() {
        if let Node::Call {
            callee,
            optional: false,
            ..
        } = *ast.node(id)
            && matches!(*ast.node(unwrap_ts(ast, callee)), Node::Member { obj, optional: false, prop }
                if ident_name(ast, prop) == Some("exit") && is_global_ident(cx, obj, "process"))
            && !in_short_circuit(cx, id, func)
        {
            out.push(id);
        }
        kids.clear();
        if let Node::Switch { disc, cases } = *ast.node(id) {
            // Upstream counts a `case` test as a path end only before the `default`
            // clause: a test after it is not seen as always evaluated.
            kids.push(disc);
            let mut seen_default = false;
            for &c in ast.list(cases) {
                if let Node::SwitchCase { test, body } = *ast.node(c) {
                    match test {
                        Some(t) if !seen_default => kids.push(t),
                        Some(_) => {}
                        None => seen_default = true,
                    }
                    kids.extend(ast.list(body).iter().copied());
                }
            }
        } else {
            ast.for_each_child(id, |c| kids.push(c));
        }
        stack.extend(kids.iter().copied());
    }
    out.sort_unstable();
    out
}

fn trap_problem(cx: &Ctx<'_, '_>, func: NodeId) -> Option<Problem> {
    let ast = cx.ast();
    let (flags, body) = match *ast.node(func) {
        Node::Arrow { flags, body, .. } => (flags, Some(body)),
        Node::FunctionExpr { flags, body, .. } => (flags, body),
        _ => return None,
    };
    let whole = Problem {
        node: func,
        fix: None,
    };
    if flags.intersects(FnFlags::ASYNC | FnFlags::GENERATOR) {
        return Some(whole);
    }
    let body = body?;
    if ast.kind(body) != NodeKind::Block {
        return problem_of(cx, body, 0);
    }
    let always_exits = || {
        let cfg = Cfg::function_with_exits(ast, func, &process_exit_calls(cx, func));
        !cfg.is_reachable(cfg.exit)
    };
    let rets = returns(ast, body);
    if rets.is_empty() {
        return (!always_exits()).then_some(whole);
    }
    for r in rets {
        let Node::Return { arg } = *ast.node(r) else {
            continue;
        };
        let Some(arg) = arg else {
            return Some(Problem { node: r, fix: None });
        };
        if let Some(p) = problem_of(cx, arg, 0) {
            return Some(p);
        }
    }
    (!always_exits()).then_some(whole)
}

impl Rule for RequireProxyTrapBooleanReturn {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::New]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(handler) = proxy_handler(ast, node) else {
            return;
        };
        let Node::ObjectLit { props } = *ast.node(handler) else {
            return;
        };
        for &prop in ast.list(props) {
            let Some((name, func)) = trap_function(cx, prop) else {
                continue;
            };
            let Some(problem) = trap_problem(cx, func) else {
                continue;
            };
            let span = cx.span(problem.node);
            let fix = problem.fix.map(|t| Fix::replace(span, t));
            cx.report(
                span,
                format!("Proxy trap `{name}` should return a boolean."),
            )
            .id("require-proxy-trap-boolean-return")
            .fix_opt(fix);
        }
    }
}
