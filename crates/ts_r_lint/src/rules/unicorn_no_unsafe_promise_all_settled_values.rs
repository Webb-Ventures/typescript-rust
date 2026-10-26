//! `unicorn/no-unsafe-promise-all-settled-values`: reading `.value` of a
//! `Promise.allSettled()` result inside a `.map()` callback without a guard that the result
//! is fulfilled.
//!
//! Settled-result arrays: `await Promise.allSettled(x)`, a type assertion to an array of
//! `PromiseSettledResult`/`PromiseRejectedResult` (or a union holding one), a `.filter()`
//! of one, and a once-declared name annotated so or a plain `const` bound to one. Arrays
//! known to hold only fulfilled results are skipped: an assertion or annotation of
//! `PromiseFulfilledResult[]`, or a filter chain with a callback whose returned
//! expression has a fulfilled status check in an `&&` chain. Inside the callback of
//! `Promise.allSettled(x).then(results => ...)`, `.map()` calls on `results` (through
//! filters) are checked too, outside nested functions.
//!
//! In the `.map()` callback (its first parameter a name, or an object pattern with
//! `value`/`status`), a `.value` read of the entry (or a read of the destructured `value`;
//! a `value` pattern that binds no plain name is reported at its key) is safe when an
//! enclosing `?:`, `&&`, `||` or `if` tests a fulfilled status (`status === "fulfilled"`,
//! `!== "rejected"`) on its side, or the statement before it is `if (<unfulfilled check>)
//! <always exits>`. A branch always exits when control cannot fall out of it (return,
//! throw, `process.exit()`, break or continue out of it, ...), per the CFG. The walk
//! stops at the callback, and at a `return` holding the read.
//!
//! Type information is not used, so arrays only the checker knows to be settled results
//! are missed.

use super::prelude::*;
use super::unicorn_conditions::unwrap_ts_and_parens;
use super::unicorn_require_proxy_trap_boolean_return::process_exit_calls;
use super::unicorn_static::evaluated_children;
use super::unicorn_util::static_string;
use crate::cfg::Cfg;
use ts_r_ast::{Ast, VarKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unsafe-promise-all-settled-values",
        "Disallow reading `.value` from `Promise.allSettled()` results without a fulfilled status guard.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoUnsafePromiseAllSettledValues>,
};

#[derive(Debug, Default)]
struct NoUnsafePromiseAllSettledValues;

const MESSAGE: &str = "Do not read `.value` from `Promise.allSettled()` results without checking that the result is fulfilled.";

/// Bound on the walks through expressions, filter chains and variables.
const MAX_STEPS: usize = 10_000;

fn unwrap(ast: &Ast, id: NodeId) -> NodeId {
    unwrap_ts_and_parens(ast, id)
}

/// `obj.method(args)` (no optional chaining, a plain member) with an argument count in
/// `min..=max`; the object.
fn method_call(ast: &Ast, id: NodeId, method: &str, min: usize, max: usize) -> Option<NodeId> {
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(id)
    else {
        return None;
    };
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(callee)
    else {
        return None;
    };
    let n = ast.list(args).len();
    (matches!(ast.node(prop), Node::Ident { .. })
        && ident_name(ast, prop) == Some(method)
        && (min..=max).contains(&n))
    .then_some(obj)
}

fn first_arg(ast: &Ast, call: NodeId) -> Option<NodeId> {
    match *ast.node(call) {
        Node::Call { args, .. } => ast.list(args).first().copied(),
        _ => None,
    }
}

fn is_all_settled_call(ast: &Ast, id: NodeId) -> bool {
    let id = unwrap(ast, id);
    let Some(obj) = method_call(ast, id, "allSettled", 1, 1) else {
        return false;
    };
    let Node::Call { args, .. } = *ast.node(id) else {
        return false;
    };
    ident_name(ast, obj) == Some("Promise")
        && !ast
            .list(args)
            .iter()
            .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
}

fn is_awaited_all_settled(ast: &Ast, id: NodeId) -> bool {
    matches!(*ast.node(unwrap(ast, id)), Node::Await { arg } if is_all_settled_call(ast, arg))
}

fn filter_object(ast: &Ast, id: NodeId) -> Option<NodeId> {
    method_call(ast, unwrap(ast, id), "filter", 1, 2)
}

// ---- Type annotations ----

fn type_name(ast: &Ast, name: NodeId) -> Option<&str> {
    match *ast.node(name) {
        Node::QualifiedName { right, .. } => ident_name(ast, right),
        _ => ident_name(ast, name),
    }
}

/// The element type of an array type annotation (`T[]`, `Array<T>`, `ReadonlyArray<T>`,
/// through parentheses and `readonly`).
fn array_element_type(ast: &Ast, mut ty: NodeId) -> Option<NodeId> {
    for _ in 0..MAX_STEPS {
        match *ast.node(ty) {
            Node::ParenType { ty: t } | Node::TypeOperator { ty: t, .. } => ty = t,
            Node::ArrayType { elem } => return Some(elem),
            Node::TypeRef {
                name,
                type_args: Some(args),
            } if matches!(ast.node(name), Node::Ident { .. })
                && matches!(ident_name(ast, name), Some("Array" | "ReadonlyArray")) =>
            {
                let Node::TypeArgs { args } = *ast.node(args) else {
                    return None;
                };
                return ast.list(args).first().copied();
            }
            _ => return None,
        }
    }
    None
}

fn is_unsafe_result_type(ast: &Ast, ty: NodeId, depth: u32) -> bool {
    if depth > 64 {
        return false;
    }
    match *ast.node(ty) {
        Node::TypeRef { name, .. } => matches!(
            type_name(ast, name),
            Some("PromiseSettledResult" | "PromiseRejectedResult")
        ),
        Node::UnionType { types } => ast
            .list(types)
            .iter()
            .any(|&t| is_unsafe_result_type(ast, t, depth + 1)),
        Node::ParenType { ty } => is_unsafe_result_type(ast, ty, depth + 1),
        _ => false,
    }
}

fn is_fulfilled_result_type(ast: &Ast, ty: NodeId, depth: u32) -> bool {
    if depth > 64 {
        return false;
    }
    match *ast.node(ty) {
        Node::TypeRef { name, .. } => type_name(ast, name) == Some("PromiseFulfilledResult"),
        Node::UnionType { types } => ast
            .list(types)
            .iter()
            .all(|&t| is_fulfilled_result_type(ast, t, depth + 1)),
        Node::ParenType { ty } => is_fulfilled_result_type(ast, ty, depth + 1),
        _ => false,
    }
}

fn is_unsafe_array_type(ast: &Ast, ty: Option<NodeId>) -> bool {
    ty.and_then(|t| array_element_type(ast, t))
        .is_some_and(|e| is_unsafe_result_type(ast, e, 0))
}

fn is_fulfilled_array_type(ast: &Ast, ty: Option<NodeId>) -> bool {
    ty.and_then(|t| array_element_type(ast, t))
        .is_some_and(|e| is_fulfilled_result_type(ast, e, 0))
}

fn assertion_type(ast: &Ast, id: NodeId) -> Option<NodeId> {
    match *ast.node(skip_parens(ast, id)) {
        Node::As { ty, .. } | Node::TypeAssertion { ty, .. } => Some(ty),
        _ => None,
    }
}

// ---- Variables ----

fn symbol_of(cx: &Ctx<'_, '_>, id: NodeId) -> Option<SymbolId> {
    let sem = cx.semantic();
    sem.symbol_of_ident(id).or_else(|| sem.symbol_of_decl(id))
}

/// The declared type annotation and `const` initializer of the once-declared variable
/// `id` refers to.
fn variable_definition(
    cx: &Ctx<'_, '_>,
    id: NodeId,
) -> Option<(SymbolId, Option<NodeId>, Option<NodeId>)> {
    let ast = cx.ast();
    let sem = cx.semantic();
    let s = sem.symbol_of_ident(id)?;
    let [decl] = sem.symbol(s).decls.as_slice() else {
        return None;
    };
    let mut ty = None;
    let mut init = None;
    match *ast.node(decl.node) {
        Node::VarDeclarator {
            name,
            type_ann,
            init: i,
            ..
        } => {
            if name == decl.name {
                ty = type_ann;
            }
            let is_const = cx.parent(decl.node).is_some_and(|p| {
                matches!(
                    ast.node(p),
                    Node::VarDecl {
                        kind: VarKind::Const,
                        ..
                    }
                )
            });
            if is_const && name == decl.name {
                init = i;
            }
        }
        Node::Param {
            pattern, type_ann, ..
        } if pattern == decl.name => ty = type_ann,
        _ => {}
    }
    Some((s, ty, init))
}

fn is_settled_array(cx: &Ctx<'_, '_>, id: NodeId, visited: &mut Vec<SymbolId>) -> bool {
    let ast = cx.ast();
    let mut cur = id;
    for _ in 0..MAX_STEPS {
        if assertion_type(ast, cur).is_some_and(|t| is_unsafe_array_type(ast, Some(t))) {
            return true;
        }
        let n = unwrap(ast, cur);
        if is_awaited_all_settled(ast, n) {
            return true;
        }
        if let Some(obj) = filter_object(ast, n) {
            cur = obj;
            continue;
        }
        if !matches!(ast.node(n), Node::Ident { .. }) {
            return false;
        }
        let Some((s, ty, init)) = variable_definition(cx, n) else {
            return false;
        };
        if visited.contains(&s) {
            return false;
        }
        visited.push(s);
        let found =
            is_unsafe_array_type(ast, ty) || init.is_some_and(|i| is_settled_array(cx, i, visited));
        visited.pop();
        return found;
    }
    false
}

fn is_known_fulfilled_array(cx: &Ctx<'_, '_>, id: NodeId, visited: &mut Vec<SymbolId>) -> bool {
    let ast = cx.ast();
    if assertion_type(ast, id).is_some_and(|t| is_fulfilled_array_type(ast, Some(t))) {
        return true;
    }
    let n = unwrap(ast, id);
    if matches!(ast.node(n), Node::Ident { .. }) {
        let Some((s, ty, init)) = variable_definition(cx, n) else {
            return false;
        };
        if visited.contains(&s) {
            return false;
        }
        visited.push(s);
        let found = is_fulfilled_array_type(ast, ty)
            || init.is_some_and(|i| is_known_fulfilled_array(cx, i, visited));
        visited.pop();
        return found;
    }
    filter_object(ast, n).is_some()
        && has_fulfilled_filter_in_chain(cx, n)
        && is_settled_array(cx, n, &mut Vec::new())
}

// ---- Status checks ----

#[derive(Default)]
struct StatusContext {
    entry_name: Option<String>,
    entry: Option<SymbolId>,
    status_name: Option<String>,
    status: Option<SymbolId>,
    value: Option<SymbolId>,
}

fn is_variable_ref(cx: &Ctx<'_, '_>, id: NodeId, var: Option<SymbolId>) -> bool {
    let ast = cx.ast();
    let id = unwrap(ast, id);
    var.is_some()
        && matches!(ast.node(id), Node::Ident { .. })
        && cx.semantic().symbol_of_ident(id) == var
}

/// The name of a member access: a plain name, or a static string index.
fn property_name(ast: &Ast, id: NodeId) -> Option<(NodeId, &str)> {
    match *ast.node(id) {
        Node::Member { obj, prop, .. } if matches!(ast.node(prop), Node::Ident { .. }) => {
            Some((obj, ident_name(ast, prop)?))
        }
        Node::Index { obj, index, .. } => Some((obj, static_string(ast, index)?)),
        _ => None,
    }
}

fn is_status_reference(cx: &Ctx<'_, '_>, id: NodeId, sc: &StatusContext) -> bool {
    let ast = cx.ast();
    let id = unwrap(ast, id);
    if sc.status.is_some() && is_variable_ref(cx, id, sc.status) {
        return true;
    }
    if sc.status.is_none()
        && sc.status_name.is_some()
        && ident_name(ast, id) == sc.status_name.as_deref()
    {
        return true;
    }
    let Some((obj, "status")) = property_name(ast, id) else {
        return false;
    };
    if sc.entry.is_some() {
        return is_variable_ref(cx, obj, sc.entry);
    }
    sc.entry_name.is_some() && ident_name(ast, unwrap(ast, obj)) == sc.entry_name.as_deref()
}

/// `Some(true)` for a fulfilled check, `Some(false)` for an unfulfilled one.
fn status_check(cx: &Ctx<'_, '_>, id: NodeId, sc: &StatusContext) -> Option<bool> {
    let ast = cx.ast();
    let Node::Binary { left, op, right } = *ast.node(unwrap(ast, id)) else {
        return None;
    };
    let equality = match op {
        BinaryOp::Eq | BinaryOp::StrictEq => true,
        BinaryOp::NotEq | BinaryOp::StrictNotEq => false,
        _ => return None,
    };
    let literal = if is_status_reference(cx, left, sc) {
        static_string(ast, right)
    } else if is_status_reference(cx, right, sc) {
        static_string(ast, left)
    } else {
        None
    };
    let fulfilled = match literal? {
        "fulfilled" => true,
        "rejected" => false,
        _ => return None,
    };
    Some(equality == fulfilled)
}

/// A fulfilled check in an `&&` chain (`want`), or an unfulfilled one in an `||` chain.
fn has_check(cx: &Ctx<'_, '_>, id: NodeId, sc: &StatusContext, fulfilled: bool) -> bool {
    let ast = cx.ast();
    let chain_op = if fulfilled {
        BinaryOp::LogicalAnd
    } else {
        BinaryOp::LogicalOr
    };
    let mut stack = vec![id];
    let mut steps = 0;
    while let Some(n) = stack.pop() {
        steps += 1;
        if steps > MAX_STEPS {
            return false;
        }
        if status_check(cx, n, sc) == Some(fulfilled) {
            return true;
        }
        if let Node::Binary { left, op, right } = *ast.node(unwrap(ast, n))
            && op == chain_op
        {
            stack.push(right);
            stack.push(left);
        }
    }
    false
}

fn function_params(ast: &Ast, f: NodeId) -> Option<&[NodeId]> {
    match *ast.node(f) {
        Node::Arrow { params, .. } | Node::FunctionExpr { params, .. } => Some(ast.list(params)),
        _ => None,
    }
}

fn function_body(ast: &Ast, f: NodeId) -> Option<NodeId> {
    match *ast.node(f) {
        Node::Arrow { body, .. } => Some(body),
        Node::FunctionExpr { body, .. } => body,
        _ => None,
    }
}

/// The parameter's binding pattern (inside `Param`).
fn param_pattern(ast: &Ast, p: NodeId) -> NodeId {
    match *ast.node(p) {
        Node::Param {
            pattern,
            init: None,
            ..
        } => pattern,
        _ => p,
    }
}

/// `getFunctionReturnExpression`: a concise arrow body, or the argument of a body that is
/// a single `return`.
fn return_expression(ast: &Ast, f: NodeId) -> Option<NodeId> {
    let body = function_body(ast, f)?;
    match *ast.node(body) {
        Node::Block { stmts } => match ast.list(stmts) {
            &[s] => match *ast.node(s) {
                Node::Return { arg } => arg,
                _ => None,
            },
            _ => None,
        },
        _ => Some(body),
    }
}

/// A property of an object pattern with a plain key `name`.
fn pattern_property(ast: &Ast, pattern: NodeId, name: &str) -> Option<NodeId> {
    let Node::ObjectPattern { props } = *ast.node(unwrap(ast, pattern)) else {
        return None;
    };
    ast.list(props).iter().copied().find(|&p| {
        matches!(*ast.node(p), Node::BindingProperty { key, .. }
        if matches!(ast.node(key), Node::Ident { .. } | Node::StringLit { .. })
            && match *ast.node(key) {
                Node::Ident { name: n } | Node::StringLit { value: n } => ast.atom(n) == name,
                _ => false,
            })
    })
}

/// The plain name a pattern property binds (`key`, `key: name`), if any.
fn property_binding(ast: &Ast, prop: NodeId) -> Option<NodeId> {
    match *ast.node(prop) {
        Node::BindingProperty {
            key,
            value: None,
            init: None,
        } => Some(key),
        Node::BindingProperty {
            value: Some(v),
            init: None,
            ..
        } if matches!(ast.node(v), Node::Ident { .. }) => Some(v),
        _ => None,
    }
}

fn is_fulfilled_filter_callback(cx: &Ctx<'_, '_>, callback: NodeId) -> bool {
    let ast = cx.ast();
    let Some(&first) = function_params(ast, callback).and_then(<[NodeId]>::first) else {
        return false;
    };
    let pattern = param_pattern(ast, first);
    let entry_name = ident_name(ast, unwrap(ast, pattern)).map(str::to_owned);
    let status_name = pattern_property(ast, pattern, "status")
        .and_then(|p| match *ast.node(p) {
            Node::BindingProperty {
                key, value: None, ..
            } => ident_name(ast, key),
            Node::BindingProperty { value: Some(v), .. } => ident_name(ast, v),
            _ => None,
        })
        .map(str::to_owned);
    let sc = StatusContext {
        entry_name,
        status_name,
        ..StatusContext::default()
    };
    return_expression(ast, callback).is_some_and(|e| has_check(cx, e, &sc, true))
}

fn has_fulfilled_filter_in_chain(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = unwrap(ast, id);
    for _ in 0..MAX_STEPS {
        let Some(obj) = filter_object(ast, cur) else {
            return false;
        };
        if first_arg(ast, cur).is_some_and(|cb| is_fulfilled_filter_callback(cx, cb)) {
            return true;
        }
        cur = unwrap(ast, obj);
    }
    false
}

// ---- Guards ----

fn always_exits(cx: &Ctx<'_, '_>, stmt: NodeId) -> bool {
    let ast = cx.ast();
    let exits = process_exit_calls(cx, stmt);
    let reads = defined_reads(cx, stmt);
    let cfg = Cfg::statements_with_exits(ast, &[stmt], &exits, &reads);
    !cfg.is_reachable(cfg.exit)
}

/// The identifiers under `root` that read a variable declared before them (so not in its
/// temporal dead zone), sorted.
fn defined_reads(cx: &Ctx<'_, '_>, root: NodeId) -> Vec<NodeId> {
    let ast = cx.ast();
    let sem = cx.semantic();
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if matches!(ast.node(id), Node::Ident { .. })
            && let Some(s) = sem.symbol_of_ident(id)
        {
            let start = ast.span(id).start;
            let decls = &sem.symbol(s).decls;
            if !decls.is_empty() && decls.iter().all(|d| ast.span(d.node).end <= start) {
                out.push(id);
            }
        }
        ast.for_each_child(id, |c| stack.push(c));
    }
    out.sort_unstable();
    out
}

fn previous_statement_exits(cx: &Ctx<'_, '_>, stmt: NodeId, sc: &StatusContext) -> bool {
    let ast = cx.ast();
    let Some(parent) = cx.parent(stmt) else {
        return false;
    };
    let stmts = match *ast.node(parent) {
        Node::Block { stmts } | Node::Program { stmts } => stmts,
        Node::StaticBlock { body } => body,
        _ => return false,
    };
    let stmts = ast.list(stmts);
    let Some(i) = stmts.iter().position(|&s| s == stmt) else {
        return false;
    };
    let Some(&prev) = i.checked_sub(1).and_then(|j| stmts.get(j)) else {
        return false;
    };
    matches!(*ast.node(prev), Node::If { test, cons, alt: None }
        if has_check(cx, test, sc, false) && always_exits(cx, cons))
}

fn is_guarded(cx: &Ctx<'_, '_>, read: NodeId, sc: &StatusContext) -> bool {
    let ast = cx.ast();
    let mut child = read;
    while let Some(cur) = cx.parent(child) {
        match *ast.node(cur) {
            Node::Arrow { .. } | Node::FunctionExpr { .. } => return false,
            Node::Paren { .. } => {
                child = cur;
                continue;
            }
            Node::Conditional { test, cons, alt } => {
                if (cons == child && has_check(cx, test, sc, true))
                    || (alt == child && has_check(cx, test, sc, false))
                {
                    return true;
                }
            }
            Node::Binary { left, op, right } if right == child => {
                if (op == BinaryOp::LogicalAnd && has_check(cx, left, sc, true))
                    || (op == BinaryOp::LogicalOr && has_check(cx, left, sc, false))
                {
                    return true;
                }
            }
            Node::If { test, cons, alt } => {
                if (cons == child && has_check(cx, test, sc, true))
                    || (alt == Some(child) && has_check(cx, test, sc, false))
                {
                    return true;
                }
            }
            _ => {}
        }
        if ast.node(cur).is_statement() && previous_statement_exits(cx, cur, sc) {
            return true;
        }
        if cx
            .parent(cur)
            .is_some_and(|p| matches!(*ast.node(p), Node::Return { arg: Some(a) } if a == cur))
        {
            return false;
        }
        child = cur;
    }
    false
}

fn is_unsafe_read(cx: &Ctx<'_, '_>, id: NodeId, sc: &StatusContext) -> bool {
    let ast = cx.ast();
    if let Some((obj, "value")) = property_name(ast, id)
        && is_variable_ref(cx, obj, sc.entry)
    {
        return true;
    }
    matches!(ast.node(id), Node::Ident { .. }) && is_variable_ref(cx, id, sc.value)
}

/// The first unguarded read under `root` (pre-order, outside functions and classes).
fn unsafe_read(cx: &Ctx<'_, '_>, root: NodeId, sc: &StatusContext) -> Option<NodeId> {
    let ast = cx.ast();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        let n = unwrap(ast, id);
        if is_unsafe_read(cx, n, sc) && !is_guarded(cx, n, sc) {
            return Some(n);
        }
        let kids = match *ast.node(n) {
            Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::ClassExpr { .. }
            | Node::ClassDecl { .. }
            | Node::FunctionDecl { .. } => continue,
            Node::ShorthandProp { name, init, .. } => {
                let mut k = vec![name];
                k.extend(init);
                k
            }
            _ => evaluated_children(ast, n),
        };
        stack.extend(kids.into_iter().rev());
    }
    None
}

fn map_problem(cx: &Ctx<'_, '_>, call: NodeId, known_settled: bool) -> Option<NodeId> {
    let ast = cx.ast();
    let obj = method_call(ast, call, "map", 1, 2)?;
    if (!known_settled && !is_settled_array(cx, obj, &mut Vec::new()))
        || is_known_fulfilled_array(cx, obj, &mut Vec::new())
    {
        return None;
    }
    let callback = first_arg(ast, call)?;
    let &first = function_params(ast, callback)?.first()?;
    let pattern = param_pattern(ast, first);
    let value_prop = pattern_property(ast, pattern, "value");
    if let Some(p) = value_prop
        && property_binding(ast, p).is_none()
    {
        let Node::BindingProperty { key, .. } = *ast.node(p) else {
            return None;
        };
        return Some(key);
    }
    let entry_ident = unwrap(ast, pattern);
    let entry_name = ident_name(ast, entry_ident).map(str::to_owned);
    let entry = entry_name.as_ref().and_then(|_| symbol_of(cx, entry_ident));
    let value = value_prop
        .and_then(|p| property_binding(ast, p))
        .and_then(|v| symbol_of(cx, v));
    let status_ident =
        pattern_property(ast, pattern, "status").and_then(|p| property_binding(ast, p));
    let sc = StatusContext {
        entry_name,
        entry,
        status_name: status_ident
            .and_then(|s| ident_name(ast, s))
            .map(str::to_owned),
        status: status_ident.and_then(|s| symbol_of(cx, s)),
        value,
    };
    unsafe_read(cx, function_body(ast, callback)?, &sc)
}

fn is_derived_through_filters(cx: &Ctx<'_, '_>, id: NodeId, var: SymbolId) -> bool {
    let ast = cx.ast();
    let mut cur = id;
    for _ in 0..MAX_STEPS {
        if is_variable_ref(cx, cur, Some(var)) {
            return true;
        }
        let Some(obj) = filter_object(ast, cur) else {
            return false;
        };
        cur = obj;
    }
    false
}

fn then_problem(cx: &Ctx<'_, '_>, call: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let obj = method_call(ast, call, "then", 1, usize::MAX)?;
    if !is_all_settled_call(ast, obj) {
        return None;
    }
    let callback = first_arg(ast, call)?;
    let &first = function_params(ast, callback)?.first()?;
    let pattern = param_pattern(ast, first);
    let pattern = unwrap(ast, pattern);
    if !matches!(ast.node(pattern), Node::Ident { .. }) {
        return None;
    }
    let results = symbol_of(cx, pattern)?;
    let mut stack = vec![function_body(ast, callback)?];
    while let Some(id) = stack.pop() {
        let n = unwrap(ast, id);
        if matches!(
            ast.node(n),
            Node::FunctionExpr { .. }
                | Node::Arrow { .. }
                | Node::ClassExpr { .. }
                | Node::ClassDecl { .. }
                | Node::FunctionDecl { .. }
        ) {
            continue;
        }
        if let Some(obj) = method_call(ast, n, "map", 1, 2)
            && is_derived_through_filters(cx, obj, results)
            && !has_fulfilled_filter_in_chain(cx, obj)
            && let Some(p) = map_problem(cx, n, true)
        {
            return Some(p);
        }
        stack.extend(evaluated_children(ast, n).into_iter().rev());
    }
    None
}

impl Rule for NoUnsafePromiseAllSettledValues {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Some(read) = then_problem(cx, node).or_else(|| map_problem(cx, node, false)) else {
            return;
        };
        cx.report_node(read, MESSAGE)
            .id("no-unsafe-promise-all-settled-values");
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{ConfigFile, ConfigKind, ResolveOptions, objects_from_eslint_json};

    fn lint(src: &str) -> usize {
        let reg = crate::Registry::global();
        let json = serde_json::json!([{ "files": ["**/*.js"], "rules": {
            "unicorn/no-unsafe-promise-all-settled-values": "error" } }]);
        let objs = objects_from_eslint_json(&json, reg).unwrap();
        let file = ConfigFile::new(ConfigKind::Eslint, None, "/p".into(), objs);
        let fc = file.file_config("a.js", &ResolveOptions::new()).unwrap();
        crate::lint_source(
            "/p/a.js",
            src,
            &fc,
            &crate::NoTypeInfo,
            crate::types::FileKey(0),
        )
        .diagnostics
        .len()
    }

    #[test]
    fn long_chains_cycles_and_malformed_input() {
        let filters = ".filter(() => true)".repeat(200);
        assert_eq!(
            lint(&format!(
                "(await Promise.allSettled(p)){filters}.map(r => r.value);"
            )),
            1
        );
    }

    #[test]
    fn probe_checks() {
        let checks = vec!["r.status === 'fulfilled'"; 300].join(" && ");
        assert_eq!(
            lint(&format!(
                "(await Promise.allSettled(p)).map(r => {checks} && r.value);"
            )),
            0
        );
    }

    #[test]
    fn probe_cycle() {
        // Self-referencing constants end the variable walk.
        let _ = lint("const a = b; const b = a; a.map(r => r.value);");
        for src in [
            "(await Promise.allSettled(p)).map(r => { if (r.status !== 'fulfilled') {",
            "Promise.allSettled(p).then(rs => rs.map(",
            "(await Promise.allSettled(p)).map(({value",
        ] {
            let _ = lint(src);
        }
    }
}
