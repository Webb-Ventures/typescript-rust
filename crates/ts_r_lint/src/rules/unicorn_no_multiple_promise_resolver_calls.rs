//! `unicorn/no-multiple-promise-resolver-calls`: a `resolve`/`reject` call of an inline
//! `new Promise()` executor that an earlier call of either can reach.
//!
//! Checked: the first two plain-name parameters (a default value is dead code, as
//! `Promise` always passes both) of a non-generator function or arrow passed as the only
//! argument of `new Promise(...)` (the global `Promise`, no type arguments), when never
//! reassigned. A call of one of them (TS wrappers on the callee looked through, `?.()`
//! counts) is an event of the code path it is in: the function, static block or class
//! field initializer holding it. Calls in different code paths never meet.
//!
//! Each code path is a CFG (`cfg::Options`, the plugin's model of ESLint's code paths):
//! - a `process.exit()` (global `process`, always evaluated) ends its path. Inside a `try`
//!   with a `catch` it does so only when nothing in that `try` block can throw before it
//!   and its block cannot complete normally (found on a second graph whose implicit throws
//!   are the nodes that may throw at all: calls, member reads, `new`, undeclared or TDZ
//!   reads, writes to constants, compound and update operators, destructuring, `using`,
//!   `extends`, ...); otherwise it is an ordinary call. An exit always evaluated in a later
//!   parameter's default ends the executor's whole path; one in a resolver's default is dead;
//! - a `catch` is reached from the end of its `try` block and from `throw` statements only.
//!   Its state comes from the "throw points" (calls other than resolver and exit calls,
//!   member reads, `new`, `yield`, `throw`, awaited names and `import()`) of the `try`
//!   block: it may be reached with the resolver uncalled when one ran uncalled, and called
//!   when one ran after a call or a resolver call ran in a `finally` that an uncalled throw
//!   point crossed on its way to the `catch`;
//! - a loop with a falsy literal test never loops again; a `continue` that crosses a
//!   `finally`, and one in the copy of a `finally` built for abrupt completion, ends its
//!   path; literal tests are folded;
//! - calls in parts that never run (`if (false)`, `while ("")`, a `for` update with a falsy
//!   test, `for (x of [])` over a known-empty value) are not events;
//! - static blocks and static field initializers run with the class; one that the parent
//!   path cannot reach (after an exit) has no reports.
//!
//! A resolver call is reported when the resolver may already have been called on a path to
//! it (a may-analysis).

use super::prelude::*;
use super::this_binding::is_strict;
use super::unicorn_conditions::unwrap_ts_and_parens;
use super::unicorn_require_proxy_trap_boolean_return::process_exit_calls;
use super::unicorn_static::is_global_ident;
use crate::cfg::{Cfg, Options};
use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-multiple-promise-resolver-calls",
        "Disallow calling Promise executor resolver functions more than once on the same execution path.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoMultiplePromiseResolverCalls>,
};

#[derive(Debug, Default)]
struct NoMultiplePromiseResolverCalls;

const MESSAGE: &str =
    "Promise executor resolver functions may be called multiple times on the same execution path.";

/// Bound on ancestor walks and on the rounds that drop ineffective exits.
const MAX_DEPTH: usize = 10_000;
const MAX_ROUNDS: usize = 8;

fn is_function(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::FunctionExpr { .. } | Node::FunctionDecl { .. } | Node::Arrow { .. }
    )
}

/// The outermost TS wrapper / parenthesis around `id`.
fn outermost_wrapper(cx: &Ctx<'_, '_>, id: NodeId) -> NodeId {
    let ast = cx.ast();
    let mut cur = id;
    for _ in 0..MAX_DEPTH {
        let Some(p) = cx.parent(cur) else { break };
        match *ast.node(p) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
                if expr == cur =>
            {
                cur = p
            }
            _ => break,
        }
    }
    cur
}

/// `new Promise(arg)` (the global, one argument, no type arguments): the argument.
fn promise_argument(cx: &Ctx<'_, '_>, new: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let Node::New {
        callee,
        type_args: None,
        args,
        ..
    } = *ast.node(new)
    else {
        return None;
    };
    let [arg] = ast.list(args) else { return None };
    (is_global_ident(cx, callee, "Promise") && !matches!(ast.node(*arg), Node::Spread { .. }))
        .then_some(*arg)
}

/// `func` is an inline executor of `new Promise(...)`.
fn is_executor(cx: &Ctx<'_, '_>, func: NodeId) -> bool {
    let ast = cx.ast();
    let ok = match *ast.node(func) {
        Node::FunctionExpr { flags, .. } => !flags.contains(FnFlags::GENERATOR),
        Node::Arrow { .. } => true,
        _ => false,
    };
    if !ok {
        return false;
    }
    let outer = outermost_wrapper(cx, func);
    cx.parent(outer)
        .is_some_and(|p| promise_argument(cx, p) == Some(outer))
}

fn params(ast: &Ast, func: NodeId) -> &[NodeId] {
    match *ast.node(func) {
        Node::FunctionExpr { params, .. }
        | Node::FunctionDecl { params, .. }
        | Node::Arrow { params, .. } => ast.list(params),
        _ => &[],
    }
}

/// The resolver symbols of executor `func`: its first two plain-name parameters, unless
/// reassigned.
fn resolver_symbols(cx: &Ctx<'_, '_>, func: NodeId) -> Vec<SymbolId> {
    let ast = cx.ast();
    let sem = cx.semantic();
    let mut out = Vec::new();
    for &p in params(ast, func).iter().take(2) {
        let Node::Param {
            pattern,
            rest: false,
            ..
        } = *ast.node(p)
        else {
            continue;
        };
        if !matches!(ast.node(pattern), Node::Ident { .. }) {
            continue;
        }
        let Some(sym) = sem.symbol_of_decl(pattern) else {
            continue;
        };
        let reassigned = sem.symbol(sym).refs.iter().any(|&r| {
            let r = sem.reference(r);
            r.is_write() && r.node != pattern
        });
        if !reassigned {
            out.push(sym);
        }
    }
    out
}

/// The call whose callee is `ident` (through TS wrappers and parentheses).
fn call_of_callee(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<NodeId> {
    let outer = outermost_wrapper(cx, ident);
    let p = cx.parent(outer)?;
    matches!(*cx.node(p), Node::Call { callee, .. } if callee == outer).then_some(p)
}

/// The identifier `call` calls (through TS wrappers and parentheses).
fn callee_ident(ast: &Ast, call: NodeId) -> Option<NodeId> {
    let Node::Call { callee, .. } = *ast.node(call) else {
        return None;
    };
    let c = unwrap_ts_and_parens(ast, callee);
    matches!(ast.node(c), Node::Ident { .. }).then_some(c)
}

/// A call of a resolver of any inline executor.
fn is_any_resolver_call(cx: &Ctx<'_, '_>, call: NodeId) -> bool {
    let ast = cx.ast();
    let Some(ident) = callee_ident(ast, call) else {
        return false;
    };
    let sem = cx.semantic();
    let Some(sym) = sem
        .reference_at(ident)
        .and_then(|r| sem.reference(r).symbol())
    else {
        return false;
    };
    let Some(decl) = sem.symbol(sym).decls.first() else {
        return false;
    };
    if decl.kind != DeclKind::Param {
        return false;
    }
    let Some(func) = cx.parent(decl.node) else {
        return false;
    };
    params(ast, func).iter().take(2).any(|&p| p == decl.node)
        && is_executor(cx, func)
        && resolver_symbols(cx, func).contains(&sym)
}

/// The parameter whose default holds `id` (the walk stops at functions).
fn param_default_of(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let mut cur = id;
    for _ in 0..MAX_DEPTH {
        let p = cx.parent(cur)?;
        if let Node::Param { init: Some(i), .. } = *ast.node(p)
            && i == cur
        {
            return Some(p);
        }
        if is_function(ast, p) {
            return None;
        }
        cur = p;
    }
    None
}

/// The index of parameter `param` of an inline executor.
fn executor_param_index(cx: &Ctx<'_, '_>, param: NodeId) -> Option<usize> {
    let func = cx.parent(param)?;
    if !is_executor(cx, func) {
        return None;
    }
    params(cx.ast(), func).iter().position(|&p| p == param)
}

/// `id` is in the default of a resolver parameter, which never runs.
fn in_resolver_default(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let mut cur = id;
    for _ in 0..MAX_DEPTH {
        let Some(param) = param_default_of(cx, cur) else {
            return false;
        };
        if executor_param_index(cx, param).is_some_and(|i| i < 2) {
            return true;
        }
        let Some(func) = cx.parent(param) else {
            return false;
        };
        cur = func;
    }
    false
}

/// A literal test that is falsy (`false`, `0`, `""`, ``` `` ```, `null`; TS wrappers).
fn falsy_literal(ast: &Ast, test: NodeId) -> bool {
    match *ast.node(unwrap_ts_and_parens(ast, test)) {
        Node::BoolLit { value } => !value,
        Node::NullLit {} => true,
        Node::NumberLit { value } => value.get() == 0.0,
        Node::StringLit { value } => ast.atom(value).is_empty(),
        Node::BigIntLit { value } => ast.atom(value).trim_start_matches('0').is_empty(),
        Node::TemplateLit { parts } => match ast.list(parts) {
            [only] => {
                matches!(*ast.node(*only), Node::TemplateElement { cooked, .. } if ast.atom(cooked).is_empty())
            }
            [] => true,
            _ => false,
        },
        _ => false,
    }
}

/// A value known to iterate zero times: falsy literals, `undefined`, `[]`, `{}`, or a
/// `const` bound to one that nothing else references.
fn empty_iterable(cx: &Ctx<'_, '_>, id: NodeId, depth: usize) -> bool {
    let ast = cx.ast();
    let id = unwrap_ts_and_parens(ast, id);
    if falsy_literal(ast, id) {
        return true;
    }
    match *ast.node(id) {
        Node::ArrayLit { elements } => ast.list(elements).is_empty(),
        Node::ObjectLit { props } => ast.list(props).is_empty(),
        Node::Ident { .. } => {
            if is_global_ident(cx, id, "undefined") {
                return true;
            }
            if depth > 8 {
                return false;
            }
            let sem = cx.semantic();
            let Some(sym) = sem.reference_at(id).and_then(|r| sem.reference(r).symbol()) else {
                return false;
            };
            let s = sem.symbol(sym);
            let [decl] = s.decls.as_slice() else {
                return false;
            };
            let uses = s
                .refs
                .iter()
                .filter(|&&r| !sem.reference(r).flags.contains(RefFlags::INIT))
                .count();
            if decl.kind != DeclKind::Const || uses != 1 {
                return false;
            }
            match *ast.node(decl.node) {
                Node::VarDeclarator {
                    name,
                    init: Some(init),
                    ..
                } if name == decl.name => empty_iterable(cx, init, depth + 1),
                _ => false,
            }
        }
        _ => false,
    }
}

/// `id` is in a part of the code that never runs (the walk crosses functions).
fn never_executed(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = id;
    for _ in 0..MAX_DEPTH {
        let Some(p) = cx.parent(cur) else {
            return false;
        };
        let never = match *ast.node(p) {
            Node::If { test, cons, .. } => cons == cur && falsy_literal(ast, test),
            Node::While { test, body } => body == cur && falsy_literal(ast, test),
            Node::For {
                test: Some(test),
                update,
                body,
                ..
            } => (body == cur || update == Some(cur)) && falsy_literal(ast, test),
            Node::ForOf { right, body, .. } | Node::ForIn { right, body, .. } => {
                body == cur && empty_iterable(cx, right, 0)
            }
            _ => false,
        };
        if never {
            return true;
        }
        cur = p;
    }
    false
}

/// The code path `id` runs in: a function, a static block or a class field initializer.
fn owner_of(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let mut cur = id;
    for _ in 0..MAX_DEPTH {
        let p = cx.parent(cur)?;
        match *ast.node(p) {
            Node::FunctionExpr { .. } | Node::FunctionDecl { .. } | Node::Arrow { .. } => {
                return Some(p);
            }
            Node::StaticBlock { .. } => return Some(p),
            Node::PropertyDef { value: Some(v), .. } if v == cur => return Some(p),
            Node::Program { .. } => return Some(p),
            _ => {}
        }
        cur = p;
    }
    None
}

fn is_static_field(ast: &Ast, id: NodeId) -> bool {
    matches!(*ast.node(id), Node::PropertyDef { modifiers, .. } if modifiers.contains(Modifiers::STATIC))
}

// ---- What may throw ----

fn is_literal(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(unwrap_ts_and_parens(ast, id)),
        Node::NumberLit { .. }
            | Node::StringLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::BigIntLit { .. }
            | Node::RegexLit { .. }
    )
}

/// The enclosing function (or program) of `id`.
fn function_of(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let mut cur = id;
    for _ in 0..MAX_DEPTH {
        let p = cx.parent(cur)?;
        if is_function(ast, p) || matches!(ast.node(p), Node::Program { .. }) {
            return Some(p);
        }
        cur = p;
    }
    None
}

/// The reference at `ident` is in the temporal dead zone of its `let`/`const`/`class`.
fn in_tdz(cx: &Ctx<'_, '_>, ident: NodeId) -> bool {
    let sem = cx.semantic();
    let Some(sym) = sem
        .reference_at(ident)
        .and_then(|r| sem.reference(r).symbol())
    else {
        return false;
    };
    let Some(decl) = sem.symbol(sym).decls.first() else {
        return false;
    };
    if !matches!(
        decl.kind,
        DeclKind::Let | DeclKind::Const | DeclKind::Class | DeclKind::Using
    ) {
        return false;
    }
    cx.span(decl.name).start > cx.span(ident).start
        && function_of(cx, decl.name) == function_of(cx, ident)
}

/// Reading `ident` may throw: undeclared, or in its temporal dead zone.
fn read_may_throw(cx: &Ctx<'_, '_>, ident: NodeId) -> bool {
    let sem = cx.semantic();
    match sem.reference_at(ident).map(|r| sem.reference(r).resolution) {
        Some(Resolution::Unresolved) => true,
        Some(Resolution::Symbol(_)) => in_tdz(cx, ident),
        _ => false,
    }
}

/// Assigning to `ident` may throw: a constant or import, undeclared in strict code, or in
/// its temporal dead zone.
fn write_may_throw(cx: &Ctx<'_, '_>, ident: NodeId) -> bool {
    let sem = cx.semantic();
    match sem.reference_at(ident).map(|r| sem.reference(r).resolution) {
        Some(Resolution::Unresolved) => is_strict(cx, ident),
        Some(Resolution::Symbol(s)) => {
            sem.symbol(s).decls.first().is_some_and(|d| {
                matches!(
                    d.kind,
                    DeclKind::Const
                        | DeclKind::Using
                        | DeclKind::ImportDefault
                        | DeclKind::ImportNamed
                        | DeclKind::ImportNamespace
                        | DeclKind::ImportEquals
                )
            }) || in_tdz(cx, ident)
        }
        _ => false,
    }
}

/// The callee member `process.exit` of one of `exits`.
fn is_exit_callee(cx: &Ctx<'_, '_>, member: NodeId, exits: &[NodeId]) -> bool {
    let outer = outermost_wrapper(cx, member);
    cx.parent(outer).is_some_and(|p| {
        exits.binary_search(&p).is_ok()
            && matches!(*cx.node(p), Node::Call { callee, .. } if callee == outer)
    })
}

/// Visits the nodes of code path `owner` in it and in the static blocks and static field
/// initializers of its classes (they run with the class), not in nested functions or
/// instance fields.
fn walk_path(cx: &Ctx<'_, '_>, owner: NodeId, inline_static: bool, mut f: impl FnMut(NodeId)) {
    let ast = cx.ast();
    let mut stack = vec![owner];
    let mut kids = Vec::new();
    while let Some(id) = stack.pop() {
        f(id);
        kids.clear();
        match *ast.node(id) {
            _ if id != owner && is_function(ast, id) => {}
            Node::StaticBlock { .. } if id != owner && !inline_static => {}
            Node::PropertyDef { key, value, .. } if id != owner => {
                kids.push(key);
                if let Some(v) = value
                    && inline_static
                    && is_static_field(ast, id)
                {
                    kids.push(v);
                }
            }
            Node::PropertyDef { value, .. } => kids.extend(value),
            _ => ast.for_each_child(id, |c| kids.push(c)),
        }
        stack.extend(kids.iter().rev().copied());
    }
}

/// The nodes of `owner`'s code path after which an exception may be thrown, sorted.
fn may_throw_nodes(cx: &Ctx<'_, '_>, owner: NodeId, exits: &[NodeId]) -> Vec<NodeId> {
    let ast = cx.ast();
    let mut out = Vec::new();
    walk_path(cx, owner, true, |id| {
        let throws = match *ast.node(id) {
            Node::Call { .. } => exits.binary_search(&id).is_err() && !is_any_resolver_call(cx, id),
            Node::New { .. } | Node::TaggedTemplate { .. } | Node::Yield { .. } => true,
            Node::Await { .. } => true,
            Node::Member { obj, .. } | Node::Index { obj, .. } => {
                let o = unwrap_ts_and_parens(ast, obj);
                !matches!(
                    ast.node(o),
                    Node::ObjectLit { .. } | Node::ArrayLit { .. } | Node::StringLit { .. }
                ) && !is_exit_callee(cx, id, exits)
            }
            Node::Ident { .. } => {
                let sem = cx.semantic();
                sem.reference_at(id).is_some_and(|r| {
                    let r = sem.reference(r);
                    r.is_read() && !r.is_type()
                }) && read_may_throw(cx, id)
            }
            Node::Assign { left, op, .. } => {
                let l = unwrap_ts_and_parens(ast, left);
                match op {
                    AssignOp::Assign
                    | AssignOp::AndAssign
                    | AssignOp::OrAssign
                    | AssignOp::CoalesceAssign => {
                        matches!(ast.node(l), Node::Ident { .. }) && write_may_throw(cx, l)
                    }
                    _ => true,
                }
            }
            Node::Update { .. } => true,
            Node::ObjectPattern { .. } | Node::ArrayPattern { .. } => true,
            Node::ObjectLit { .. } | Node::ArrayLit { .. } => is_assignment_target(cx, id),
            Node::VarDeclarator { .. } => cx.parent(id).is_some_and(|p| {
                matches!(
                    *ast.node(p),
                    Node::VarDecl {
                        kind: VarKind::Using | VarKind::AwaitUsing,
                        ..
                    }
                )
            }),
            Node::Binary { left, op, right } => {
                let safe_op = matches!(
                    op,
                    BinaryOp::StrictEq
                        | BinaryOp::StrictNotEq
                        | BinaryOp::LogicalAnd
                        | BinaryOp::LogicalOr
                        | BinaryOp::Coalesce
                );
                let both_literal = is_literal(ast, left) && is_literal(ast, right);
                !(safe_op || both_literal)
            }
            Node::Unary { op, arg } => match op {
                UnaryOp::Void | UnaryOp::Not | UnaryOp::TypeOf => false,
                UnaryOp::Delete => true,
                _ => !is_literal(ast, arg),
            },
            Node::Spread { .. } => true,
            Node::TemplateLit { parts } => ast.list(parts).len() > 1 && !is_tagged_quasi(cx, id),
            _ => false,
        };
        if throws {
            out.push(id);
        }
        // A class with `extends` may throw once its heritage is evaluated.
        if let Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. } = *ast.node(id) {
            for &h in ast.list(heritage) {
                if let Node::HeritageClause { types, .. } = *ast.node(h) {
                    for &t in ast.list(types) {
                        if let Node::ExprWithTypeArgs { expr, .. } = *ast.node(t) {
                            out.push(expr);
                        }
                    }
                }
            }
        }
    });
    out.sort_unstable();
    out.dedup();
    out
}

fn is_tagged_quasi(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    cx.parent(id)
        .is_some_and(|p| matches!(*cx.node(p), Node::TaggedTemplate { quasi, .. } if quasi == id))
}

/// An object or array literal that is a destructuring assignment target.
fn is_assignment_target(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let Some(p) = cx.parent(id) else {
        return false;
    };
    match *ast.node(p) {
        Node::Assign {
            left,
            op: AssignOp::Assign,
            ..
        } => left == id,
        Node::ForOf { left, .. } | Node::ForIn { left, .. } => left == id,
        _ => false,
    }
}

/// `continue` statements of the path that cross a `finally` on the way to their loop.
fn continues_across_finally(cx: &Ctx<'_, '_>, owner: NodeId) -> Vec<NodeId> {
    let ast = cx.ast();
    let mut out = Vec::new();
    walk_path(cx, owner, true, |id| {
        let Node::Continue { label } = *ast.node(id) else {
            return;
        };
        let name = label.and_then(|l| ident_name(ast, l));
        let mut crosses = false;
        let mut cur = id;
        for _ in 0..MAX_DEPTH {
            let Some(p) = cx.parent(cur) else { break };
            match *ast.node(p) {
                Node::Try {
                    block,
                    handler,
                    finalizer: Some(_),
                } if block == cur || handler == Some(cur) => crosses = true,
                Node::Labeled { label, .. } if name.is_some() && ident_name(ast, label) == name => {
                    break;
                }
                Node::For { .. }
                | Node::ForIn { .. }
                | Node::ForOf { .. }
                | Node::While { .. }
                | Node::DoWhile { .. }
                    if name.is_none() =>
                {
                    break;
                }
                _ if is_function(ast, p) => break,
                _ => {}
            }
            cur = p;
        }
        if crosses {
            out.push(id);
        }
    });
    out.sort_unstable();
    out
}

/// The `try` (with a `catch`) whose block holds `id`, nearest first; the walk stops at
/// functions.
fn catching_try(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let mut cur = id;
    for _ in 0..MAX_DEPTH {
        let p = cx.parent(cur)?;
        if is_function(ast, p) {
            return None;
        }
        if let Node::Try {
            block,
            handler: Some(_),
            ..
        } = *ast.node(p)
            && block == cur
        {
            return Some(p);
        }
        cur = p;
    }
    None
}

/// `child` of `parent` is always evaluated when `parent` is.
fn always_evaluates(cx: &Ctx<'_, '_>, parent: NodeId, child: NodeId, exits: &[NodeId]) -> bool {
    let ast = cx.ast();
    match *ast.node(parent) {
        Node::Seq { .. } | Node::Unary { .. } | Node::Paren { .. } => true,
        Node::As { .. }
        | Node::Satisfies { .. }
        | Node::NonNull { .. }
        | Node::TypeAssertion { .. } => true,
        Node::Binary { left, op, .. } => {
            !matches!(
                op,
                BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce
            ) || left == child
        }
        Node::Conditional { cons, alt, .. } => {
            always_exits_expr(ast, cons, exits) && always_exits_expr(ast, alt, exits)
        }
        Node::Call {
            callee, optional, ..
        } => callee == child || (!optional && !chain_has_optional(ast, callee)),
        Node::New { .. } => true,
        Node::Member { obj, .. } => obj == child,
        Node::Index { obj, optional, .. } => {
            obj == child || (!optional && !chain_has_optional(ast, obj))
        }
        _ => false,
    }
}

fn always_exits_expr(ast: &Ast, id: NodeId, exits: &[NodeId]) -> bool {
    let id = unwrap_ts_and_parens(ast, id);
    exits.binary_search(&id).is_ok()
        || matches!(*ast.node(id), Node::Conditional { cons, alt, .. }
            if always_exits_expr(ast, cons, exits) && always_exits_expr(ast, alt, exits))
}

fn chain_has_optional(ast: &Ast, mut id: NodeId) -> bool {
    for _ in 0..MAX_DEPTH {
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

/// Exit `x` is always evaluated in the default of a parameter after the resolvers.
fn in_always_run_default(cx: &Ctx<'_, '_>, x: NodeId, exits: &[NodeId]) -> bool {
    let ast = cx.ast();
    let mut child = x;
    for _ in 0..MAX_DEPTH {
        let Some(p) = cx.parent(child) else {
            return false;
        };
        if let Node::Param { init: Some(i), .. } = *ast.node(p) {
            return i == child && executor_param_index(cx, p).is_some_and(|i| i >= 2);
        }
        if !always_evaluates(cx, p, child, exits) {
            return false;
        }
        child = p;
    }
    false
}

/// The graph of code path `owner`.
fn build(ast: &Ast, owner: NodeId, opts: &Options<'_>) -> Option<Cfg> {
    match *ast.node(owner) {
        Node::FunctionExpr { .. } | Node::FunctionDecl { .. } | Node::Arrow { .. } => {
            Some(Cfg::function_with(ast, owner, opts))
        }
        Node::StaticBlock { body } => Some(Cfg::statements_with(ast, ast.list(body), opts)),
        Node::PropertyDef { value: Some(v), .. } => Some(Cfg::expression_with(ast, v, opts)),
        Node::Program { stmts } => Some(Cfg::statements_with(ast, ast.list(stmts), opts)),
        _ => None,
    }
}

/// The `process.exit()` calls that end their path in `owner`, sorted.
fn effective_exits(cx: &Ctx<'_, '_>, owner: NodeId, dead_continues: &[NodeId]) -> Vec<NodeId> {
    let ast = cx.ast();
    let mut exits: Vec<NodeId> = process_exit_calls(cx, owner)
        .into_iter()
        .filter(|&x| !in_resolver_default(cx, x))
        .collect();
    let guarded: FxHashMap<NodeId, NodeId> = exits
        .iter()
        .filter_map(|&x| catching_try(cx, x).map(|t| (x, t)))
        .collect();
    if guarded.is_empty() {
        return exits;
    }
    for _ in 0..MAX_ROUNDS {
        let throws = may_throw_nodes(cx, owner, &exits);
        let opts = Options {
            exits: &exits,
            throws: Some(&throws),
            fold_literals: true,
            dead_continues,
            abrupt_finally_continue_dead: true,
            static_fields: true,
            ..Options::default()
        };
        let Some(cfg) = build(ast, owner, &opts) else {
            return exits;
        };
        let reach = cfg.event_reachability();
        let ends: FxHashMap<NodeId, bool> = cfg
            .try_ends
            .iter()
            .map(|&(t, b)| (t, cfg.is_reachable(b)))
            .fold(FxHashMap::default(), |mut m, (t, r)| {
                *m.entry(t).or_insert(false) |= r;
                m
            });
        let catchable = |t: NodeId| {
            let Node::Try {
                handler: Some(h), ..
            } = *ast.node(t)
            else {
                return false;
            };
            reach.get(&h).copied().unwrap_or(false) || ends.get(&t).copied().unwrap_or(false)
        };
        let before = exits.len();
        exits.retain(|x| guarded.get(x).is_none_or(|&t| !catchable(t)));
        if exits.len() == before {
            break;
        }
    }
    exits
}

// ---- The analysis ----

#[derive(Clone, PartialEq, Default, Debug)]
struct State {
    /// The resolvers may not have been called yet.
    uncalled: bool,
    /// A resolver may have been called.
    called: bool,
    /// `catch` clauses a throw point reached after a resolver call (sorted).
    after_call: Vec<NodeId>,
    /// `catch` clauses a throw point reached with the resolvers uncalled, with the `finally`
    /// blocks on its way (sorted).
    pending: Vec<(NodeId, Vec<NodeId>)>,
}

fn insert_sorted(v: &mut Vec<NodeId>, x: NodeId) {
    if let Err(i) = v.binary_search(&x) {
        v.insert(i, x);
    }
}

fn join(a: &State, b: &State) -> State {
    let mut s = a.clone();
    s.uncalled |= b.uncalled;
    s.called |= b.called;
    for &c in &b.after_call {
        insert_sorted(&mut s.after_call, c);
    }
    for (c, fins) in &b.pending {
        add_pending(&mut s, *c, fins);
    }
    s
}

fn add_pending(s: &mut State, catch: NodeId, fins: &[NodeId]) {
    let i = match s.pending.binary_search_by_key(&catch, |e| e.0) {
        Ok(i) => i,
        Err(i) => {
            s.pending.insert(i, (catch, Vec::new()));
            i
        }
    };
    for &f in fins {
        insert_sorted(&mut s.pending[i].1, f);
    }
}

/// Where an exception thrown at a node goes: its `catch` and the `finally` blocks on the way.
struct ExcCtx {
    catch: Option<NodeId>,
    finallys: Vec<NodeId>,
}

enum Event {
    Catch,
    /// A resolver call of the executor, with the `finally` blocks holding it.
    Resolver(Vec<NodeId>),
    /// A throw point that a `catch` receives.
    Throw(usize),
}

/// The events of `owner`'s code path for one executor's resolver `calls`.
fn events(
    cx: &Ctx<'_, '_>,
    owner: NodeId,
    calls: &FxHashSet<NodeId>,
    exits: &[NodeId],
) -> (FxHashMap<NodeId, Event>, Vec<ExcCtx>) {
    let ast = cx.ast();
    let mut ctxs = vec![ExcCtx {
        catch: None,
        finallys: Vec::new(),
    }];
    let mut out = FxHashMap::default();
    let never = never_executed(cx, owner);
    // (node, exception context, finally blocks holding it, never runs)
    let mut stack: Vec<(NodeId, usize, Vec<NodeId>, bool)> = vec![(owner, 0, Vec::new(), never)];
    let mut kids = Vec::new();
    while let Some((id, exc, fins, never)) = stack.pop() {
        let node = *ast.node(id);
        if let Node::CatchClause { .. } = node {
            out.insert(id, Event::Catch);
        }
        if calls.contains(&id) {
            out.insert(id, Event::Resolver(fins.clone()));
        } else if !never && ctxs[exc].catch.is_some() && is_throw_point(cx, id, exits) {
            out.insert(id, Event::Throw(exc));
        }
        if id != owner && (is_function(ast, id) || matches!(node, Node::StaticBlock { .. })) {
            continue;
        }
        kids.clear();
        match node {
            Node::PropertyDef { key, value, .. } if id != owner => {
                kids.push((key, exc, fins.clone(), never));
                let _ = value;
            }
            Node::PropertyDef { value: Some(v), .. } => kids.push((v, exc, fins.clone(), never)),
            Node::Try {
                block,
                handler,
                finalizer,
            } => {
                let outer_catch = ctxs[exc].catch;
                let mut crossed = ctxs[exc].finallys.clone();
                crossed.extend(finalizer);
                let block_ctx = match handler {
                    Some(h) => ExcCtx {
                        catch: Some(h),
                        finallys: Vec::new(),
                    },
                    None => ExcCtx {
                        catch: outer_catch,
                        finallys: crossed.clone(),
                    },
                };
                let handler_ctx = ExcCtx {
                    catch: outer_catch,
                    finallys: crossed,
                };
                ctxs.push(block_ctx);
                kids.push((block, ctxs.len() - 1, fins.clone(), never));
                if let Some(h) = handler {
                    ctxs.push(handler_ctx);
                    kids.push((h, ctxs.len() - 1, fins.clone(), never));
                }
                if let Some(f) = finalizer {
                    let mut inner = fins.clone();
                    inner.push(f);
                    kids.push((f, exc, inner, never));
                }
            }
            _ => {
                let mut cs = Vec::new();
                ast.for_each_child(id, |c| cs.push(c));
                for c in cs {
                    let n = never || child_never_runs(cx, id, c);
                    kids.push((c, exc, fins.clone(), n));
                }
            }
        }
        while let Some(k) = kids.pop() {
            stack.push(k);
        }
    }
    (out, ctxs)
}

/// `child` of `parent` never runs (a falsy literal loop or `if` test, an empty iterable).
fn child_never_runs(cx: &Ctx<'_, '_>, parent: NodeId, child: NodeId) -> bool {
    let ast = cx.ast();
    match *ast.node(parent) {
        Node::If { test, cons, .. } => cons == child && falsy_literal(ast, test),
        Node::While { test, body } => body == child && falsy_literal(ast, test),
        Node::For {
            test: Some(test),
            update,
            body,
            ..
        } => (body == child || update == Some(child)) && falsy_literal(ast, test),
        Node::ForOf { right, body, .. } | Node::ForIn { right, body, .. } => {
            body == child && empty_iterable(cx, right, 0)
        }
        _ => false,
    }
}

fn is_directly_awaited(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let outer = outermost_wrapper(cx, id);
    cx.parent(outer)
        .is_some_and(|p| matches!(*cx.node(p), Node::Await { .. }))
}

/// A node at which the plugin lets an exception reach a `catch`.
fn is_throw_point(cx: &Ctx<'_, '_>, id: NodeId, exits: &[NodeId]) -> bool {
    match *cx.node(id) {
        Node::Call { .. } => exits.binary_search(&id).is_err() && !is_any_resolver_call(cx, id),
        Node::New { .. } | Node::Member { .. } | Node::Index { .. } | Node::Yield { .. } => true,
        Node::Throw { .. } => true,
        Node::Ident { .. } => is_directly_awaited(cx, id),
        Node::ImportCall { .. } => is_directly_awaited(cx, id),
        _ => false,
    }
}

impl Rule for NoMultiplePromiseResolverCalls {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::New]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(arg) = promise_argument(cx, node) else {
            return;
        };
        let func = unwrap_ts_and_parens(ast, arg);
        if !is_executor(cx, func) {
            return;
        }
        let syms = resolver_symbols(cx, func);
        if syms.is_empty() {
            return;
        }
        // The resolver calls, by code path.
        let mut by_owner: Vec<(NodeId, NodeId)> = Vec::new();
        {
            let sem = cx.semantic();
            for &sym in &syms {
                for &r in &sem.symbol(sym).refs {
                    let r = sem.reference(r);
                    if !r.is_read() {
                        continue;
                    }
                    let Some(call) = call_of_callee(cx, r.node) else {
                        continue;
                    };
                    if never_executed(cx, call) || in_resolver_default(cx, call) {
                        continue;
                    }
                    if let Some(owner) = owner_of(cx, call) {
                        by_owner.push((owner, call));
                    }
                }
            }
        }
        by_owner.sort_unstable();
        let mut reports: Vec<NodeId> = Vec::new();
        let mut i = 0;
        while i < by_owner.len() {
            let owner = by_owner[i].0;
            let mut calls = FxHashSet::default();
            while i < by_owner.len() && by_owner[i].0 == owner {
                calls.insert(by_owner[i].1);
                i += 1;
            }
            if calls.len() < 2 && !has_loop(cx, owner) {
                continue;
            }
            analyze(cx, func, owner, &calls, &mut reports);
        }
        reports.sort_unstable_by_key(|&c| cx.span(c).start);
        reports.dedup();
        for call in reports {
            cx.report(cx.span(call), MESSAGE)
                .id("no-multiple-promise-resolver-calls");
        }
    }
}

/// A single resolver call can still be reported when it is in a loop.
fn has_loop(cx: &Ctx<'_, '_>, owner: NodeId) -> bool {
    let ast = cx.ast();
    let mut found = false;
    walk_path(cx, owner, false, |id| {
        found |= matches!(
            ast.node(id),
            Node::For { .. }
                | Node::ForIn { .. }
                | Node::ForOf { .. }
                | Node::While { .. }
                | Node::DoWhile { .. }
        );
    });
    found
}

/// A sync child path (static block, static field) the parent path cannot reach.
fn unreachable_sync_child(cx: &Ctx<'_, '_>, owner: NodeId) -> bool {
    let ast = cx.ast();
    let first = match *ast.node(owner) {
        Node::StaticBlock { body } => match ast.list(body).first() {
            Some(&s) => s,
            None => return false,
        },
        Node::PropertyDef { value: Some(v), .. } if is_static_field(ast, owner) => v,
        _ => return false,
    };
    let Some(parent) = owner_of(cx, owner) else {
        return false;
    };
    let dead = continues_across_finally(cx, parent);
    let exits = effective_exits(cx, parent, &dead);
    if exits.is_empty() {
        return false;
    }
    let opts = Options {
        exits: &exits,
        throws: Some(&[]),
        try_end_to_catch: true,
        fold_literals: true,
        dead_continues: &dead,
        abrupt_finally_continue_dead: true,
        static_fields: true,
        ..Options::default()
    };
    let Some(cfg) = build(ast, parent, &opts) else {
        return false;
    };
    cfg.event_reachability().get(&first) == Some(&false)
}

fn analyze(
    cx: &Ctx<'_, '_>,
    executor: NodeId,
    owner: NodeId,
    calls: &FxHashSet<NodeId>,
    reports: &mut Vec<NodeId>,
) {
    let ast = cx.ast();
    if unreachable_sync_child(cx, owner) {
        return;
    }
    let dead = continues_across_finally(cx, owner);
    let exits = effective_exits(cx, owner, &dead);
    if owner == executor && exits.iter().any(|&x| in_always_run_default(cx, x, &exits)) {
        return;
    }
    let all_exits: Vec<NodeId> = process_exit_calls(cx, owner);
    let opts = Options {
        exits: &exits,
        throws: Some(&[]),
        try_end_to_catch: true,
        fold_literals: true,
        dead_continues: &dead,
        abrupt_finally_continue_dead: true,
        static_fields: true,
        ..Options::default()
    };
    let Some(cfg) = build(ast, owner, &opts) else {
        return;
    };
    let (evs, ctxs) = events(cx, owner, calls, &all_exits);
    let transfer = |s: &State, e: NodeId| -> State {
        match evs.get(&e) {
            None => s.clone(),
            Some(Event::Catch) => State {
                uncalled: s.pending.binary_search_by_key(&e, |p| p.0).is_ok(),
                called: s.after_call.binary_search(&e).is_ok(),
                ..State::default()
            },
            Some(Event::Resolver(fins)) => {
                let mut n = s.clone();
                for (c, pf) in &s.pending {
                    if pf.iter().any(|f| fins.contains(f)) {
                        insert_sorted(&mut n.after_call, *c);
                    }
                }
                n.called = s.uncalled || s.called;
                n.uncalled = false;
                n
            }
            Some(Event::Throw(ctx)) => {
                let ExcCtx { catch, finallys } = &ctxs[*ctx];
                let Some(catch) = *catch else {
                    return s.clone();
                };
                let mut n = s.clone();
                if s.uncalled {
                    add_pending(&mut n, catch, finallys);
                }
                if s.called {
                    insert_sorted(&mut n.after_call, catch);
                }
                n
            }
        }
    };
    let init = State {
        uncalled: true,
        ..State::default()
    };
    let entry = cfg.forward(init, join, transfer);
    for (b, s) in cfg.blocks.iter().zip(entry) {
        let Some(mut s) = s else { continue };
        for &e in &b.events {
            if calls.contains(&e) && s.called {
                reports.push(e);
            }
            s = transfer(&s, e);
        }
    }
}
