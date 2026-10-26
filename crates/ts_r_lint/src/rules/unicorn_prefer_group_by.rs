//! `unicorn/prefer-group-by`: manual grouping that `Object.groupBy()` / `Map.groupBy()`
//! expresses directly.
//!
//! Two sources, both starting from `{}` / `Object.create(null)` (`Object.groupBy`) or
//! `new Map()` (`Map.groupBy`), skipped when the enclosing declaration declares `Object` /
//! `Map`:
//!
//! - `xs.reduce(cb, init)` (plain, two arguments, no optional chain in `xs`, `xs` not a
//!   sparse array or known non-indexed collection) with a non-async, non-generator callback
//!   `(acc, el[, index[, array]])` (distinct identifiers, block body) that never writes `el`
//!   and ends with `return acc`. Reported at `reduce`.
//! - `const|let acc = init;` directly followed by `for (const|let el of xs) { … }` (not
//!   `await`) where `el` is never written, `xs` mentions neither `el` nor `acc`, and no body
//!   declaration is named `acc` or `el`. Reported at the loop.
//!
//! The remaining statements may start with `const key = expr` (not mentioning `acc`, `index`
//! or `array`) and must then be one of
//! - objects: `(acc[k] ??= []).push(el)` (also `||=`, `acc[k] = acc[k] || []`, `??`), or that
//!   initializer followed by `acc[k].push(el)`;
//! - maps: `if (acc.has(k)) { acc.get(k).push(el); } else { acc.set(k, [el]); }`, or
//!   `const g = acc.get(k) ?? []; g.push(el); acc.set(k, g);` (also `||`).
//!
//! Every `k` is the key identifier, or the same reference without optional chains; the key
//! doesn't mention `acc`, `index`, `array` or a body declaration. The fix writes
//! `Method(xs, el => key)`; it is dropped for type arguments or annotations, a callback
//! return type or type parameters, a side-effecting key (for loops: repeated or holding
//! `await`/`yield`), comments, and `this`/`arguments`/`new.target`/own name in a `function`
//! callback's key. A loop whose accumulator a function that can run during it reads gets a
//! suggestion instead.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_iter::{next_statement, only_expression, referenced_by_nested_function};
use super::unicorn_no_useless_boolean_cast::concise_arrow_body_text;
use super::unicorn_static::has_side_effect;
use super::unicorn_text::{has_comment_in, is_same_reference, paren_text};
use super::unicorn_types::is_known_non_indexed_collection;
use super::unicorn_util_h5::remove_statement;
use crate::semantic::RefFlags;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-group-by",
        "Prefer `Object.groupBy()` or `Map.groupBy()` over manual grouping.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferGroupBy>,
};

#[derive(Debug, Default)]
struct PreferGroupBy;

const MAX_NODES: usize = 200_000;

/// Every node under `root`, itself included. `None` when the subtree is too large.
fn subtree(ast: &Ast, root: NodeId) -> Option<Vec<NodeId>> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        out.push(n);
        if out.len() > MAX_NODES {
            return None;
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    Some(out)
}

fn contains(ast: &Ast, root: NodeId, pred: impl Fn(NodeId) -> bool) -> bool {
    subtree(ast, root).is_none_or(|nodes| nodes.into_iter().any(pred))
}

fn contains_optional_chain(ast: &Ast, root: NodeId) -> bool {
    contains(ast, root, |n| {
        matches!(
            ast.node(n),
            Node::Member { optional: true, .. }
                | Node::Index { optional: true, .. }
                | Node::Call { optional: true, .. }
        )
    })
}

/// An identifier in a reference position named `name`, anywhere under `root`.
fn references_name(cx: &Ctx<'_, '_>, root: NodeId, name: Option<&str>) -> bool {
    let Some(name) = name else {
        return false;
    };
    let ast = cx.ast();
    let sem = cx.semantic();
    contains(ast, root, |n| {
        matches!(ast.node(n), Node::Ident { .. })
            && ident_name(ast, n) == Some(name)
            && sem
                .reference_at(n)
                .is_some_and(|r| !sem.reference(r).flags.contains(RefFlags::INIT))
    })
}

fn is_ident(ast: &Ast, id: NodeId) -> bool {
    matches!(ast.node(skip_parens(ast, id)), Node::Ident { .. })
}

/// `isSameIdentifier`: two identifiers with the same name.
fn same_ident(ast: &Ast, a: NodeId, b: NodeId) -> bool {
    let (a, b) = (skip_parens(ast, a), skip_parens(ast, b));
    is_ident(ast, a) && is_ident(ast, b) && ident_name(ast, a) == ident_name(ast, b)
}

fn is_named(ast: &Ast, id: NodeId, name: &str) -> bool {
    let id = skip_parens(ast, id);
    is_ident(ast, id) && ident_name(ast, id) == Some(name)
}

fn is_empty_array(ast: &Ast, id: NodeId) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::ArrayLit { elements } if ast.list(elements).is_empty())
}

fn has_type_args(ast: &Ast, id: NodeId) -> bool {
    matches!(
        *ast.node(skip_parens(ast, id)),
        Node::Call {
            type_args: Some(_),
            ..
        } | Node::New {
            type_args: Some(_),
            ..
        }
    )
}

fn group_by_method(ast: &Ast, init: NodeId) -> Option<&'static str> {
    let init = skip_parens(ast, init);
    match *ast.node(init) {
        Node::ObjectLit { props } if ast.list(props).is_empty() => Some("Object.groupBy"),
        Node::New { callee, args, .. }
            if is_named(ast, callee, "Map") && ast.list(args).is_empty() =>
        {
            Some("Map.groupBy")
        }
        Node::Call { .. } => {
            let call = method_call(ast, init)?;
            (call.name == "create"
                && call.plain()
                && is_named(ast, call.obj, "Object")
                && call.args.len() == 1
                && matches!(ast.node(skip_parens(ast, call.args[0])), Node::NullLit {}))
            .then_some("Object.groupBy")
        }
        _ => None,
    }
}

/// Binding identifiers declared by a variable declaration.
fn declared_names(cx: &Ctx<'_, '_>, decl: NodeId) -> Vec<NodeId> {
    let ast = cx.ast();
    let sem = cx.semantic();
    let Node::VarDecl { decls, .. } = *ast.node(decl) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for &d in ast.list(decls) {
        let Node::VarDeclarator { name, .. } = *ast.node(d) else {
            continue;
        };
        if let Some(nodes) = subtree(ast, name) {
            out.extend(nodes.into_iter().filter(|&n| {
                matches!(ast.node(n), Node::Ident { .. }) && sem.symbol_of_decl(n).is_some()
            }));
        }
    }
    out.sort_by_key(|&n| cx.span(n).start);
    out
}

fn method_for_binding(
    cx: &Ctx<'_, '_>,
    init: NodeId,
    decl: Option<NodeId>,
) -> Option<&'static str> {
    let ast = cx.ast();
    let method = group_by_method(ast, init)?;
    let conflict = decl.is_some_and(|d| {
        declared_names(cx, d).iter().any(|&n| {
            ident_name(ast, n).is_some_and(|name| {
                method
                    .strip_prefix(name)
                    .is_some_and(|rest| rest.starts_with('.'))
            })
        })
    });
    (!conflict).then_some(method)
}

#[derive(Clone, Copy)]
struct Parts<'a> {
    accumulator: &'a str,
    element: &'a str,
    index: Option<&'a str>,
    array: Option<&'a str>,
}

fn mentions_parts(cx: &Ctx<'_, '_>, id: NodeId, parts: Parts<'_>) -> bool {
    references_name(cx, id, Some(parts.accumulator))
        || references_name(cx, id, parts.index)
        || references_name(cx, id, parts.array)
}

/// `const key = expr`: `(key identifier, expr)`.
fn single_const(ast: &Ast, stmt: Option<NodeId>) -> Option<(NodeId, NodeId)> {
    let Node::VarDecl {
        kind: VarKind::Const,
        decls,
        ..
    } = *ast.node(stmt?)
    else {
        return None;
    };
    let [d] = *ast.list(decls) else {
        return None;
    };
    match *ast.node(d) {
        Node::VarDeclarator {
            name,
            init: Some(init),
            ..
        } if matches!(ast.node(name), Node::Ident { .. }) => Some((name, init)),
        _ => None,
    }
}

struct KeyBinding<'s> {
    statements: &'s [NodeId],
    expression: Option<NodeId>,
    identifier: Option<NodeId>,
}

fn key_binding<'s>(
    cx: &Ctx<'_, '_>,
    statements: &'s [NodeId],
    parts: Parts<'_>,
) -> Option<KeyBinding<'s>> {
    let Some((identifier, init)) = single_const(cx.ast(), statements.first().copied()) else {
        return Some(KeyBinding {
            statements,
            expression: None,
            identifier: None,
        });
    };
    if mentions_parts(cx, init, parts) {
        return None;
    }
    Some(KeyBinding {
        statements: &statements[1..],
        expression: Some(init),
        identifier: Some(identifier),
    })
}

fn expected_key(
    cx: &Ctx<'_, '_>,
    key: NodeId,
    expected: NodeId,
    identifier: Option<NodeId>,
) -> bool {
    let ast = cx.ast();
    match identifier {
        Some(i) => same_ident(ast, key, i),
        None => {
            !contains_optional_chain(ast, key)
                && !contains_optional_chain(ast, expected)
                && is_same_reference(cx, key, expected)
        }
    }
}

fn valid_key(cx: &Ctx<'_, '_>, key: NodeId, parts: Parts<'_>) -> bool {
    !contains_optional_chain(cx.ast(), key) && !mentions_parts(cx, key, parts)
}

/// `acc[k]`: `k`.
fn group_member(ast: &Ast, id: NodeId, accumulator: &str) -> Option<NodeId> {
    match *ast.node(skip_parens(ast, id)) {
        Node::Index {
            obj,
            optional: false,
            index,
        } if is_named(ast, obj, accumulator) => Some(skip_parens(ast, index)),
        _ => None,
    }
}

/// `acc[k] ??= []`, `acc[k] ||= []`, `acc[k] = acc[k] || []` / `?? []`: `k`.
fn initializer_key(cx: &Ctx<'_, '_>, id: NodeId, parts: Parts<'_>) -> Option<NodeId> {
    let ast = cx.ast();
    let Node::Assign { op, left, right } = *ast.node(skip_parens(ast, id)) else {
        return None;
    };
    match op {
        AssignOp::CoalesceAssign | AssignOp::OrAssign if is_empty_array(ast, right) => {
            group_member(ast, left, parts.accumulator)
        }
        AssignOp::Assign => {
            let Node::Binary {
                left: l,
                op: BinaryOp::LogicalOr | BinaryOp::Coalesce,
                right: r,
            } = *ast.node(skip_parens(ast, right))
            else {
                return None;
            };
            if !is_empty_array(ast, r) {
                return None;
            }
            let key = group_member(ast, left, parts.accumulator)?;
            is_same_reference(cx, left, l).then_some(key)
        }
        _ => None,
    }
}

/// `x.name(a)` (plain, exactly `n` non-spread arguments): `(x, args)`.
fn plain_call<'a>(
    ast: &'a Ast,
    id: NodeId,
    name: &str,
    n: usize,
) -> Option<(NodeId, &'a [NodeId])> {
    let call = method_call(ast, skip_parens(ast, id))?;
    (call.name == name
        && call.plain()
        && call.args.len() == n
        && !call
            .args
            .iter()
            .any(|&a| matches!(ast.node(a), Node::Spread { .. })))
    .then_some((call.obj, call.args))
}

fn push_key(
    cx: &Ctx<'_, '_>,
    id: NodeId,
    parts: Parts<'_>,
    require_initializer: bool,
) -> Option<NodeId> {
    let ast = cx.ast();
    let (obj, args) = plain_call(ast, id, "push", 1)?;
    if !is_named(ast, args[0], parts.element) {
        return None;
    }
    if matches!(ast.node(skip_parens(ast, obj)), Node::Assign { .. }) {
        return initializer_key(cx, obj, parts);
    }
    if require_initializer {
        return None;
    }
    group_member(ast, obj, parts.accumulator)
}

fn expression_of(ast: &Ast, stmt: NodeId) -> Option<NodeId> {
    match *ast.node(stmt) {
        Node::ExprStmt { expr } => Some(expr),
        _ => None,
    }
}

fn object_key(cx: &Ctx<'_, '_>, statements: &[NodeId], parts: Parts<'_>) -> Option<NodeId> {
    let ast = cx.ast();
    let binding = key_binding(cx, statements, parts)?;
    match *binding.statements {
        [only] => {
            let pushed = push_key(cx, expression_of(ast, only)?, parts, true)?;
            let key = binding.expression.unwrap_or(pushed);
            (expected_key(cx, pushed, key, binding.identifier) && valid_key(cx, key, parts))
                .then_some(key)
        }
        [first, second] => {
            let initialized = initializer_key(cx, expression_of(ast, first)?, parts)?;
            let pushed = push_key(cx, expression_of(ast, second)?, parts, false)?;
            let key = binding.expression.unwrap_or(initialized);
            (expected_key(cx, initialized, key, binding.identifier)
                && expected_key(cx, pushed, key, binding.identifier)
                && valid_key(cx, key, parts))
            .then_some(key)
        }
        _ => None,
    }
}

/// `acc.method(k)` (plain, one argument): `k`.
fn map_call_key(ast: &Ast, id: NodeId, accumulator: &str, method: &str) -> Option<NodeId> {
    let (obj, args) = plain_call(ast, id, method, 1)?;
    is_named(ast, obj, accumulator).then(|| skip_parens(ast, args[0]))
}

fn map_set<'a>(ast: &'a Ast, id: NodeId, accumulator: &str) -> Option<&'a [NodeId]> {
    let (obj, args) = plain_call(ast, id, "set", 2)?;
    is_named(ast, obj, accumulator).then_some(args)
}

fn map_if_else_key(
    cx: &Ctx<'_, '_>,
    stmt: NodeId,
    parts: Parts<'_>,
    expression: Option<NodeId>,
    identifier: Option<NodeId>,
) -> Option<NodeId> {
    let ast = cx.ast();
    let Node::If {
        test,
        cons,
        alt: Some(alt),
    } = *ast.node(stmt)
    else {
        return None;
    };
    if !matches!(ast.node(cons), Node::Block { .. }) || !matches!(ast.node(alt), Node::Block { .. })
    {
        return None;
    }
    let tested = map_call_key(ast, test, parts.accumulator, "has")?;
    let key = expression.unwrap_or(tested);
    if !expected_key(cx, tested, key, identifier) {
        return None;
    }
    let pushes = only_expression(ast, cons).is_some_and(|e| {
        plain_call(ast, e, "push", 1).is_some_and(|(obj, args)| {
            is_named(ast, args[0], parts.element)
                && map_call_key(ast, obj, parts.accumulator, "get")
                    .is_some_and(|k| expected_key(cx, k, key, identifier))
        })
    });
    let sets = only_expression(ast, alt).is_some_and(|e| {
        map_set(ast, e, parts.accumulator).is_some_and(|args| {
            expected_key(cx, skip_parens(ast, args[0]), key, identifier)
                && matches!(*ast.node(skip_parens(ast, args[1])), Node::ArrayLit { elements }
                    if matches!(*ast.list(elements), [e] if is_ident(ast, e) && is_named(ast, e, parts.element)))
        })
    });
    (pushes && sets).then_some(key)
}

fn map_get_set_key(
    cx: &Ctx<'_, '_>,
    statements: &[NodeId],
    parts: Parts<'_>,
    expression: Option<NodeId>,
    identifier: Option<NodeId>,
) -> Option<NodeId> {
    let ast = cx.ast();
    let [first, second, third] = *statements else {
        return None;
    };
    let (group, init) = single_const(ast, Some(first))?;
    let Node::Binary {
        left,
        op: BinaryOp::LogicalOr | BinaryOp::Coalesce,
        right,
    } = *ast.node(skip_parens(ast, init))
    else {
        return None;
    };
    if !is_empty_array(ast, right) {
        return None;
    }
    let got = map_call_key(ast, left, parts.accumulator, "get")?;
    let key = expression.unwrap_or(got);
    let group_name = ident_name(ast, group)?;
    let pushes = expression_of(ast, second)
        .and_then(|e| plain_call(ast, e, "push", 1))
        .is_some_and(|(obj, args)| {
            is_named(ast, obj, group_name) && is_named(ast, args[0], parts.element)
        });
    let sets = expression_of(ast, third)
        .and_then(|e| map_set(ast, e, parts.accumulator))
        .is_some_and(|args| {
            expected_key(cx, skip_parens(ast, args[0]), key, identifier)
                && same_ident(ast, args[1], group)
        });
    (expected_key(cx, got, key, identifier) && pushes && sets).then_some(key)
}

fn map_key(cx: &Ctx<'_, '_>, statements: &[NodeId], parts: Parts<'_>) -> Option<NodeId> {
    let binding = key_binding(cx, statements, parts);
    let (expression, identifier) = binding
        .as_ref()
        .map_or((None, None), |b| (b.expression, b.identifier));
    let statements = binding.as_ref().map_or(statements, |b| b.statements);
    let key = match *statements {
        [only] => map_if_else_key(cx, only, parts, expression, identifier),
        _ => map_get_set_key(cx, statements, parts, expression, identifier),
    }?;
    valid_key(cx, key, parts).then_some(key)
}

fn group_key(
    cx: &Ctx<'_, '_>,
    method: &str,
    statements: &[NodeId],
    parts: Parts<'_>,
) -> Option<NodeId> {
    let key = if method == "Object.groupBy" {
        object_key(cx, statements, parts)
    } else {
        map_key(cx, statements, parts)
    }?;
    let ast = cx.ast();
    let locals: Vec<NodeId> = statements
        .iter()
        .filter(|&&s| matches!(ast.node(s), Node::VarDecl { .. }))
        .flat_map(|&s| declared_names(cx, s))
        .collect();
    (!locals
        .iter()
        .any(|&l| references_name(cx, key, ident_name(ast, l))))
    .then_some(key)
}

fn is_sparse_receiver(ast: &Ast, id: NodeId, depth: u32) -> bool {
    if depth > 1000 {
        return true;
    }
    let mut id = id;
    for _ in 0..10_000 {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. } => id = expr,
            _ => break,
        }
    }
    match *ast.node(id) {
        Node::ArrayLit { elements } => ast
            .list(elements)
            .iter()
            .any(|&e| matches!(ast.node(e), Node::Hole {})),
        Node::Call { callee, args, .. } | Node::New { callee, args, .. }
            if is_named(ast, callee, "Array") && skip_parens(ast, callee) == callee =>
        {
            ast.list(args).len() == 1
        }
        Node::Member { obj, .. } | Node::Index { obj, .. } => {
            is_sparse_receiver(ast, obj, depth + 1)
        }
        Node::Call { callee, .. } => match *ast.node(skip_parens(ast, callee)) {
            Node::Member { obj, .. } | Node::Index { obj, .. } => {
                is_sparse_receiver(ast, obj, depth + 1)
            }
            _ => false,
        },
        _ => false,
    }
}

/// A plain identifier parameter: its pattern.
fn ident_param(ast: &Ast, param: Option<&NodeId>) -> Option<NodeId> {
    match *ast.node(*param?) {
        Node::Param {
            rest: false,
            init: None,
            pattern,
            ..
        } if matches!(ast.node(pattern), Node::Ident { .. }) => Some(pattern),
        _ => None,
    }
}

fn arrow_param_text(cx: &Ctx<'_, '_>, element: NodeId) -> String {
    let ast = cx.ast();
    let (span, wrap) = match cx.parent(element).map(|p| (p, *ast.node(p))) {
        Some((
            p,
            Node::Param {
                type_ann, optional, ..
            },
        )) => (cx.span(p), type_ann.is_some() || optional),
        Some((p, Node::VarDeclarator { type_ann, .. })) => {
            let end = type_ann.map_or(cx.span(element).end, |t| cx.span(t).end);
            (Span::new(cx.span(p).start, end), type_ann.is_some())
        }
        _ => (cx.span(element), false),
    };
    let text = span.text(cx.src());
    if wrap {
        format!("({text})")
    } else {
        text.to_owned()
    }
}

fn writes_besides(cx: &Ctx<'_, '_>, ident: NodeId) -> bool {
    let sem = cx.semantic();
    let Some(sym) = sem.symbol_of_decl(ident) else {
        return true;
    };
    sem.symbol(sym).refs.iter().any(|&r| {
        let r = sem.reference(r);
        r.node != ident && r.is_write() && !r.flags.contains(RefFlags::INIT)
    })
}

impl PreferGroupBy {
    fn check_reduce(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, id) else {
            return;
        };
        if call.name != "reduce"
            || !call.plain()
            || call.args.len() != 2
            || call
                .args
                .iter()
                .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
            || contains_optional_chain(ast, call.obj)
            || is_sparse_receiver(ast, call.obj, 0)
            || is_known_non_indexed_collection(cx, call.obj)
        {
            return;
        }
        let callback = skip_parens(ast, call.args[0]);
        let initial = call.args[1];
        let (flags, params, body, return_type, type_params, fn_name, is_function) =
            match *ast.node(callback) {
                Node::Arrow {
                    flags,
                    params,
                    body,
                    return_type,
                    type_params,
                } => (flags, params, body, return_type, type_params, None, false),
                Node::FunctionExpr {
                    flags,
                    params,
                    body: Some(body),
                    return_type,
                    type_params,
                    name,
                    ..
                } => (flags, params, body, return_type, type_params, name, true),
                _ => return,
            };
        let params = ast.list(params);
        let Node::Block { stmts } = *ast.node(body) else {
            return;
        };
        if flags.contains(FnFlags::ASYNC) || flags.contains(FnFlags::GENERATOR) || params.len() > 4
        {
            return;
        }
        let (Some(accumulator), Some(element)) = (
            ident_param(ast, params.first()),
            ident_param(ast, params.get(1)),
        ) else {
            return;
        };
        let index = ident_param(ast, params.get(2));
        let array = ident_param(ast, params.get(3));
        if (params.len() > 2 && index.is_none()) || (params.len() > 3 && array.is_none()) {
            return;
        }
        let names: Vec<&str> = params
            .iter()
            .filter_map(|&p| match *ast.node(p) {
                Node::Param { pattern, .. } => ident_name(ast, pattern),
                _ => None,
            })
            .collect();
        if (1..names.len()).any(|i| names[..i].contains(&names[i])) || writes_besides(cx, element) {
            return;
        }
        let (Some(acc_name), Some(el_name)) =
            (ident_name(ast, accumulator), ident_name(ast, element))
        else {
            return;
        };
        let parts = Parts {
            accumulator: acc_name,
            element: el_name,
            index: index.and_then(|i| ident_name(ast, i)),
            array: array.and_then(|a| ident_name(ast, a)),
        };
        let declarator = {
            let mut cur = id;
            let mut found = None;
            for _ in 0..100_000 {
                let Some(p) = cx.parent(cur) else {
                    break;
                };
                if matches!(ast.node(p), Node::VarDeclarator { .. }) {
                    found = Some(p);
                    break;
                }
                cur = p;
            }
            found
        };
        let Some(method) = method_for_binding(cx, initial, declarator.and_then(|d| cx.parent(d)))
        else {
            return;
        };
        let stmts = ast.list(stmts);
        let Some((&last, statements)) = stmts.split_last() else {
            return;
        };
        if !matches!(*ast.node(last), Node::Return { arg: Some(a) } if same_ident(ast, a, accumulator))
        {
            return;
        }
        let Some(key) = group_key(cx, method, statements, parts) else {
            return;
        };
        let typed_declarator = declarator.is_some_and(|d| {
            matches!(
                *ast.node(d),
                Node::VarDeclarator {
                    type_ann: Some(_),
                    ..
                }
            )
        });
        let typed_accumulator = matches!(
            cx.parent(accumulator).map(|p| *ast.node(p)),
            Some(Node::Param {
                type_ann: Some(_),
                ..
            })
        );
        let function_specific = is_function
            && (matches!(el_name, "await" | "yield")
                || contains(ast, key, |n| {
                    matches!(
                        ast.node(n),
                        Node::This {}
                            | Node::MetaProperty {
                                kind: ts_r_ast::MetaPropKind::NewTarget,
                            }
                    )
                })
                || references_name(cx, key, Some("arguments"))
                || references_name(cx, key, fn_name.and_then(|n| ident_name(ast, n))));
        let skip = has_type_args(ast, id)
            || has_type_args(ast, initial)
            || typed_accumulator
            || return_type.is_some()
            || type_params.is_some()
            || typed_declarator
            || function_specific
            || has_side_effect(ast, key)
            || has_comment_in(cx, cx.span(id));
        let fix = (!skip).then(|| {
            Fix::replace(
                cx.span(id),
                format!(
                    "{method}({}, {} => {})",
                    paren_text(cx, call.obj),
                    arrow_param_text(cx, element),
                    concise_arrow_body_text(cx, key)
                ),
            )
        });
        let r = cx
            .report(
                cx.span(call.prop),
                format!("Prefer `{method}()` over `Array#reduce()`."),
            )
            .id("prefer-group-by");
        if let Some(f) = fix {
            r.fix(f);
        }
    }

    fn check_loop(&self, decl: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::VarDecl { kind, decls, .. } = *ast.node(decl) else {
            return;
        };
        if !matches!(kind, VarKind::Const | VarKind::Let) {
            return;
        }
        let [declarator] = *ast.list(decls) else {
            return;
        };
        let Node::VarDeclarator {
            name: accumulator,
            init: Some(init),
            type_ann,
            ..
        } = *ast.node(declarator)
        else {
            return;
        };
        if !matches!(ast.node(accumulator), Node::Ident { .. }) {
            return;
        }
        let Some(method) = method_for_binding(cx, init, Some(decl)) else {
            return;
        };
        let Some(loop_node) = next_statement(cx, decl) else {
            return;
        };
        let Node::ForOf {
            is_await: false,
            left,
            right,
            body,
        } = *ast.node(loop_node)
        else {
            return;
        };
        let Node::VarDecl {
            kind: VarKind::Const | VarKind::Let,
            decls: loop_decls,
            ..
        } = *ast.node(left)
        else {
            return;
        };
        let Some(&first) = ast.list(loop_decls).first() else {
            return;
        };
        let Node::VarDeclarator { name: element, .. } = *ast.node(first) else {
            return;
        };
        if !matches!(ast.node(element), Node::Ident { .. }) || writes_besides(cx, element) {
            return;
        }
        let sem = cx.semantic();
        let right_span = cx.span(right);
        if let Some(sym) = sem.symbol_of_decl(element)
            && sem.symbol(sym).refs.iter().any(|&r| {
                let s = cx.span(sem.reference(r).node);
                s.start >= right_span.start && s.end <= right_span.end
            })
        {
            return;
        }
        let Node::Block { stmts } = *ast.node(body) else {
            return;
        };
        let statements = ast.list(stmts);
        let (Some(acc_name), Some(el_name)) =
            (ident_name(ast, accumulator), ident_name(ast, element))
        else {
            return;
        };
        let locals: Vec<NodeId> = statements
            .iter()
            .filter(|&&s| matches!(ast.node(s), Node::VarDecl { .. }))
            .flat_map(|&s| declared_names(cx, s))
            .collect();
        if acc_name == el_name
            || references_name(cx, right, Some(acc_name))
            || locals
                .iter()
                .any(|&l| matches!(ident_name(ast, l), Some(n) if n == acc_name || n == el_name))
        {
            return;
        }
        let parts = Parts {
            accumulator: acc_name,
            element: el_name,
            index: None,
            array: None,
        };
        let Some(key) = group_key(cx, method, statements, parts) else {
            return;
        };
        let loop_span = cx.span(loop_node);
        let end = next_token_start(cx, loop_span.end);
        let repeated_key = has_side_effect(ast, key) && {
            let key_span = cx.span(key);
            statements.iter().any(|&s| {
                contains(ast, s, |n| {
                    let ns = cx.span(n);
                    (ns.end <= key_span.start || ns.start >= key_span.end)
                        && is_same_reference(cx, n, key)
                })
            })
        };
        let skip = has_comment_in(cx, Span::new(cx.span(init).start, end))
            || type_ann.is_some()
            || has_type_args(ast, init)
            || repeated_key
            || contains(ast, key, |n| {
                matches!(ast.node(n), Node::Await { .. } | Node::Yield { .. })
            });
        let fix = (!skip).then(|| {
            let mut fix = Fix::replace(
                cx.span(skip_parens(ast, init)),
                format!(
                    "{method}({}, {} => {})",
                    paren_text(cx, right),
                    arrow_param_text(cx, element),
                    concise_arrow_body_text(cx, key)
                ),
            );
            for e in remove_statement(cx, loop_node).edits {
                fix = fix.and(e.span, e.text);
            }
            fix
        });
        let nested = sem
            .symbol_of_decl(accumulator)
            .is_some_and(|s| referenced_by_nested_function(cx, s, loop_node));
        let r = cx
            .report(
                loop_span,
                format!("Prefer `{method}()` over a `for-of` loop."),
            )
            .id("prefer-group-by-loop");
        if let Some(f) = fix {
            if nested {
                r.suggest(
                    "replace-loop-with-group-by",
                    format!("Replace the loop with `{method}()`."),
                    f,
                );
            } else {
                r.fix(f);
            }
        }
    }
}

/// Where the next token after `pos` starts (comments and whitespace skipped), or the end
/// of the file.
fn next_token_start(cx: &Ctx<'_, '_>, pos: u32) -> u32 {
    let src = cx.src();
    let len = src.len() as u32;
    let comments = cx.ast().comments_in(Span::new(pos, len));
    let mut p = pos;
    for _ in 0..1_000_000 {
        let rest = &src[p as usize..];
        let trimmed = rest.trim_start();
        p += (rest.len() - trimmed.len()) as u32;
        match comments.iter().find(|c| c.span.start == p) {
            Some(c) => p = c.span.end,
            None => break,
        }
    }
    p
}

impl Rule for PreferGroupBy {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::VarDecl]
    }
    fn enter(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        match cx.node(id) {
            Node::Call { .. } => self.check_reduce(id, cx),
            _ => self.check_loop(id, cx),
        }
    }
}
