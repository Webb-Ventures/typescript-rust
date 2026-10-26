//! What the eslint-plugin-unicorn iterator rules know about iterators: a value is an
//! `Iterator` when it is an `Iterator.from/concat/zip/zipKeyed(..)` call on the global
//! `Iterator` (or `globalThis.Iterator`), an `x.entries()/keys()/values()` call (no
//! arguments) or `x.matchAll(..)`, a lazy helper (`drop/filter/flatMap/map/take` with an
//! argument) on an iterator, a call of a synchronous generator function, a plain
//! unannotated `const` (or an unchanged function declaration) whose value is one of those,
//! or, from types, one of the built-in iterator types (not an array).
//!
//! A `.toArray()` receiver is known *not* to be an iterator when it is an object, array,
//! literal, template, function or class expression, `new LocalClass()`, or a `const`
//! initialized with one of those or with a `toArray/slice/concat/split(..)` call.
//!
//! Also the pieces of the "fill a collection in a `for…of` loop" rules: the loop's single
//! `const`/`let` binding, `[a, b]` pair patterns, whether a variable is mentioned inside a
//! node, and whether a function that can run during the loop reads the variable.

#![allow(dead_code)] // A shared toolkit: each rule uses part of it.

use super::prelude::*;
use super::unicorn_ast::{method_call, no_spread_in};
use super::unicorn_no_array_reduce::const_initializer;
use super::unicorn_static::is_global_ident;
use super::unicorn_types::{Checker, is_array};
use ts_r_ast::Ast;

const ITERATOR_TYPES: &[&str] = &[
    "ArrayIterator",
    "Generator",
    "IterableIterator",
    "Iterator",
    "IteratorObject",
    "MapIterator",
    "RegExpStringIterator",
    "SetIterator",
    "StringIterator",
];

static ITERATOR: Checker = Checker {
    target_names: ITERATOR_TYPES,
    prefer_definitions: true,
    nullish_in_mixed_union: true,
    ..Checker::DEFAULT
};

/// Parentheses and TS expression wrappers skipped.
pub(crate) fn unwrap_expression(ast: &Ast, mut id: NodeId) -> NodeId {
    for _ in 0..10_000 {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
            | Node::Instantiation { expr, .. } => id = expr,
            _ => break,
        }
    }
    id
}

fn is_global_iterator(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = unwrap_expression(ast, id);
    match *ast.node(id) {
        Node::Ident { .. } => is_global_ident(cx, id, "Iterator"),
        Node::Member {
            obj,
            optional: false,
            prop,
        } => {
            ident_name(ast, prop) == Some("Iterator")
                && matches!(ast.node(prop), Node::Ident { .. })
                && is_global_ident(cx, unwrap_expression(ast, obj), "globalThis")
        }
        _ => false,
    }
}

fn is_global_iterator_method_call(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    method_call(cx.ast(), id).is_some_and(|c| {
        matches!(c.name, "concat" | "from" | "zip" | "zipKeyed") && is_global_iterator(cx, c.obj)
    })
}

/// `x.entries()`, `x.keys()`, `x.values()` or `x.matchAll(..)` (at most one argument).
pub(crate) fn is_iterator_method_call(ast: &Ast, id: NodeId) -> bool {
    method_call(ast, id).is_some_and(|c| match c.name {
        "entries" | "keys" | "values" => c.args.is_empty(),
        "matchAll" => c.args.len() <= 1 && no_spread_in(ast, c.args, 1),
        _ => false,
    })
}

/// `drop/filter/flatMap/map/take(..)` (at least one argument) on an iterator.
pub(crate) fn is_lazy_iterator_helper_call(
    cx: &Ctx<'_, '_>,
    id: NodeId,
    visited: &mut Vec<NodeId>,
) -> bool {
    method_call(cx.ast(), id).is_some_and(|c| {
        matches!(c.name, "drop" | "filter" | "flatMap" | "map" | "take")
            && !c.args.is_empty()
            && iterator_expression(cx, c.obj, visited)
    })
}

/// A literal, function, class or `new LocalClass()`.
fn is_known_non_iterator_value(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = unwrap_expression(ast, id);
    match *ast.node(id) {
        Node::ObjectLit { .. }
        | Node::ArrayLit { .. }
        | Node::StringLit { .. }
        | Node::NumberLit { .. }
        | Node::BigIntLit { .. }
        | Node::BoolLit { .. }
        | Node::NullLit {}
        | Node::RegexLit { .. }
        | Node::TemplateLit { .. }
        | Node::FunctionExpr { .. }
        | Node::Arrow { .. }
        | Node::ClassExpr { .. } => true,
        Node::New { callee, .. } => {
            matches!(ast.node(callee), Node::Ident { .. }) && {
                let sem = cx.semantic();
                sem.reference_at(callee)
                    .and_then(|r| sem.reference(r).symbol())
                    .is_some_and(|s| {
                        sem.symbol(s)
                            .decls
                            .iter()
                            .any(|d| d.kind == DeclKind::Class)
                    })
            }
        }
        _ => false,
    }
}

/// `isKnownNonIterator`: a `.toArray()` receiver that is surely not an `Iterator`.
pub(crate) fn is_known_non_iterator(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    if is_known_non_iterator_value(cx, id) {
        return true;
    }
    let ast = cx.ast();
    let id = unwrap_expression(ast, id);
    let Some(init) = const_initializer(cx, id) else {
        return false;
    };
    is_known_non_iterator_value(cx, init)
        || method_call(ast, skip_parens(ast, init)).is_some_and(|c| {
            c.plain() && matches!(c.name, "toArray" | "slice" | "concat" | "split")
        })
}

/// The value of a plain unannotated `const` or of an unchanged function declaration.
fn immutable_value(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    if !matches!(ast.node(id), Node::Ident { .. }) {
        return None;
    }
    let sem = cx.semantic();
    let sym = sem.reference(sem.reference_at(id)?).symbol()?;
    let symbol = sem.symbol(sym);
    let [d] = symbol.decls.as_slice() else {
        return None;
    };
    if symbol.refs.iter().any(|&r| {
        let f = sem.reference(r).flags;
        f.contains(RefFlags::WRITE) && !f.contains(RefFlags::INIT)
    }) {
        return None;
    }
    match *ast.node(d.node) {
        Node::FunctionDecl { .. } if d.kind == DeclKind::Function => Some(d.node),
        Node::VarDeclarator {
            name,
            type_ann: None,
            init,
            ..
        } if name == d.name
            && matches!(
                ast.node(d.parent),
                Node::VarDecl {
                    kind: VarKind::Const,
                    ..
                }
            ) =>
        {
            init
        }
        _ => None,
    }
}

fn is_sync_generator_function(cx: &Ctx<'_, '_>, id: NodeId, visited: &mut Vec<NodeId>) -> bool {
    let ast = cx.ast();
    let mut node = Some(id);
    while let Some(n) = node {
        let n = unwrap_expression(ast, n);
        if visited.contains(&n) || visited.len() > 1000 {
            return false;
        }
        visited.push(n);
        if let Node::FunctionDecl { flags, .. } | Node::FunctionExpr { flags, .. } = *ast.node(n) {
            return flags.contains(FnFlags::GENERATOR) && !flags.contains(FnFlags::ASYNC);
        }
        node = immutable_value(cx, n);
    }
    false
}

/// `isIteratorExpression`: the value is known to be an `Iterator`.
pub(crate) fn is_iterator_expression(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    iterator_expression(cx, id, &mut Vec::new())
}

fn iterator_expression(cx: &Ctx<'_, '_>, expr: NodeId, visited: &mut Vec<NodeId>) -> bool {
    let ast = cx.ast();
    let node = unwrap_expression(ast, expr);
    if visited.contains(&node) || visited.len() > 1000 {
        return false;
    }
    visited.push(node);
    if is_global_iterator_method_call(cx, node)
        || is_iterator_method_call(ast, node)
        || is_lazy_iterator_helper_call(cx, node, visited)
    {
        return true;
    }
    if let Some(value) = immutable_value(cx, node)
        && iterator_expression(cx, value, visited)
    {
        return true;
    }
    if let Node::Call {
        callee,
        optional: false,
        ..
    } = *ast.node(node)
        && is_sync_generator_function(cx, callee, visited)
    {
        return true;
    }
    !is_array(cx, expr) && ITERATOR.is_target(cx, expr)
}

// ---- for…of collection loops ----------------------------------------------------------

/// The statement after `stmt` in its statement list.
pub(crate) fn next_statement(cx: &Ctx<'_, '_>, stmt: NodeId) -> Option<NodeId> {
    let list = statement_list(cx.ast(), cx.parent(stmt)?)?;
    let i = list.iter().position(|&s| s == stmt)?;
    list.get(i + 1).copied()
}

/// The expression of the only statement of a loop body or `if` branch (`x;` or `{ x; }`).
pub(crate) fn only_expression(ast: &Ast, stmt: NodeId) -> Option<NodeId> {
    let mut stmt = stmt;
    if let Node::Block { stmts } = *ast.node(stmt) {
        let [only] = ast.list(stmts) else {
            return None;
        };
        stmt = *only;
    }
    match *ast.node(stmt) {
        Node::ExprStmt { expr } => Some(expr),
        _ => None,
    }
}

/// The binding of a `for (const|let <pattern> of …)` head with one declarator.
pub(crate) fn for_of_declaration_pattern(ast: &Ast, left: NodeId) -> Option<NodeId> {
    let Node::VarDecl { kind, decls, .. } = *ast.node(left) else {
        return None;
    };
    let [d] = ast.list(decls) else {
        return None;
    };
    match (kind, *ast.node(*d)) {
        (VarKind::Const | VarKind::Let, Node::VarDeclarator { name, .. }) => Some(name),
        _ => None,
    }
}

/// `[a, b]`: an array pattern of exactly two identifiers.
pub(crate) fn pair_pattern(ast: &Ast, pattern: NodeId) -> Option<(NodeId, NodeId)> {
    let Node::ArrayPattern { elements } = *ast.node(pattern) else {
        return None;
    };
    match *ast.list(elements) {
        [a, b]
            if matches!(ast.node(a), Node::Ident { .. })
                && matches!(ast.node(b), Node::Ident { .. }) =>
        {
            Some((a, b))
        }
        _ => None,
    }
}

/// A declaration or reference identifier of `sym` lies inside `node`.
pub(crate) fn references_symbol(cx: &Ctx<'_, '_>, sym: SymbolId, node: NodeId) -> bool {
    let sem = cx.semantic();
    let symbol = sem.symbol(sym);
    let range = cx.span(node);
    symbol
        .decls
        .iter()
        .map(|d| d.name)
        .chain(symbol.refs.iter().map(|&r| sem.reference(r).node))
        .any(|i| {
            let s = cx.span(i);
            s.start >= range.start && s.end <= range.end
        })
}

/// The function (or class field, static block, program) whose variables `id` belongs to,
/// and whether it is a function declaration.
fn variable_scope(cx: &Ctx<'_, '_>, id: NodeId) -> (Option<NodeId>, bool) {
    let ast = cx.ast();
    let mut cur = cx.parent(id);
    while let Some(c) = cur {
        match ast.node(c) {
            Node::FunctionDecl { .. } => return (Some(c), true),
            Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::PropertyDef { .. }
            | Node::StaticBlock { .. } => return (Some(c), false),
            _ => {}
        }
        cur = cx.parent(c);
    }
    (None, false)
}

/// `isReferencedByNestedFunction`: a reference to `sym` from another function that can run
/// during `loop_node` (created before its end, or a hoisted function declaration).
pub(crate) fn referenced_by_nested_function(
    cx: &Ctx<'_, '_>,
    sym: SymbolId,
    loop_node: NodeId,
) -> bool {
    let sem = cx.semantic();
    let symbol = sem.symbol(sym);
    let Some(decl) = symbol.decls.first() else {
        return false;
    };
    let own = variable_scope(cx, decl.name).0;
    let loop_end = cx.span(loop_node).end;
    symbol.refs.iter().any(|&r| {
        let ident = sem.reference(r).node;
        let (scope, declaration) = variable_scope(cx, ident);
        scope != own && (cx.span(ident).start < loop_end || declaration)
    })
}

/// `isGlobalNameAvailable`: no runtime binding named `name` is visible at `node`, so code
/// inserted there can use the global (type aliases, interfaces and type-only imports
/// can't shadow a value).
pub(crate) fn global_name_available(cx: &Ctx<'_, '_>, name: &str, node: NodeId) -> bool {
    let ast = cx.ast();
    let sem = cx.semantic();
    let Some(s) = sem.lookup(sem.enclosing_scope(node), name) else {
        return true;
    };
    sem.symbol(s).decls.iter().all(|d| match d.kind {
        DeclKind::TypeAlias | DeclKind::Interface => true,
        DeclKind::ImportNamed | DeclKind::ImportDefault | DeclKind::ImportNamespace => {
            matches!(
                *ast.node(d.parent),
                Node::ImportDecl {
                    type_only: true,
                    ..
                }
            ) || matches!(
                *ast.node(d.node),
                Node::ImportSpecifier {
                    type_only: true,
                    ..
                }
            )
        }
        _ => false,
    })
}

/// An `await`, `yield` or `for await` in `root`, outside nested functions.
pub(crate) fn contains_suspension_point(ast: &Ast, root: NodeId) -> bool {
    let mut stack = vec![root];
    let mut guard = 0u32;
    while let Some(id) = stack.pop() {
        guard += 1;
        if guard > 1_000_000 {
            return true;
        }
        if matches!(
            ast.node(id),
            Node::Await { .. } | Node::Yield { .. } | Node::ForOf { is_await: true, .. }
        ) {
            return true;
        }
        if crate::ast_util::is_function(ast.kind(id))
            || matches!(ast.node(id), Node::MethodDef { .. })
        {
            continue;
        }
        ast.for_each_child(id, |c| stack.push(c));
    }
    false
}
