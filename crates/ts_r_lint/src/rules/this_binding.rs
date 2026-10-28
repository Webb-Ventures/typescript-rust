//! Shared helpers for rules about `this` and strict mode: where `this` is bound and whether
//! that binding is the default one (the global object in sloppy code).

use super::prelude::*;
use crate::env::SourceType;
use ts_r_ast::Ast;

/// Where a `this` expression gets its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThisScope {
    /// The top level of the file.
    Program,
    /// A non-arrow function.
    Function(NodeId),
    /// A class field initializer or a static block: always the class or instance.
    Class,
}

/// The scope a `this` at `node` refers to (arrows are transparent).
pub(crate) fn this_scope(cx: &Ctx<'_, '_>, node: NodeId) -> ThisScope {
    let mut child = node;
    let mut cur = cx.parent(node);
    while let Some(p) = cur {
        match *cx.node(p) {
            Node::FunctionDecl { .. } | Node::FunctionExpr { .. } => return ThisScope::Function(p),
            Node::PropertyDef { value: Some(v), .. } if v == child => return ThisScope::Class,
            Node::StaticBlock { .. } => return ThisScope::Class,
            _ => {}
        }
        child = p;
        cur = cx.parent(p);
    }
    ThisScope::Program
}

/// True when a statement list starts with a `"use strict"` directive.
fn has_use_strict(cx: &Ctx<'_, '_>, stmts: &[NodeId]) -> bool {
    if cx.env().ecma_version < 5 {
        return false;
    }
    stmts
        .iter()
        .take_while(|&&s| matches!(cx.node(s), Node::Directive { .. }))
        .any(|&s| {
            let t = cx.text(s);
            t.starts_with("'use strict'") || t.starts_with("\"use strict\"")
        })
}

/// The directive prologue of a function or the program.
fn prologue(ast: &Ast, f: NodeId) -> &[NodeId] {
    match *ast.node(f) {
        Node::Program { stmts } => ast.list(stmts),
        Node::FunctionDecl { body: Some(b), .. }
        | Node::FunctionExpr { body: Some(b), .. }
        | Node::Arrow { body: b, .. } => match *ast.node(b) {
            Node::Block { stmts } => ast.list(stmts),
            _ => &[],
        },
        _ => &[],
    }
}

/// True when the code at `node` is strict mode code.
pub(crate) fn is_strict(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let env = cx.env();
    // ESLint's scope analysis has no strict mode before ES5.
    if crate::globals::normalize_ecma_version(env.ecma_version) == 3 {
        return false;
    }
    if env.source_type == SourceType::Module || env.implied_strict {
        return true;
    }
    let ast = cx.ast();
    ancestors_inclusive(cx.file.parents, node).any(|a| match ast.node(a) {
        Node::ClassDecl { .. } | Node::ClassExpr { .. } => true,
        Node::Program { .. }
        | Node::FunctionDecl { .. }
        | Node::FunctionExpr { .. }
        | Node::Arrow { .. } => has_use_strict(cx, prologue(ast, a)),
        _ => false,
    })
}

/// True when `this` at the top level of the file is the global object.
pub(crate) fn top_level_this_is_global(cx: &Ctx<'_, '_>) -> bool {
    let env = cx.env();
    env.source_type == SourceType::Script && !env.global_return
}

fn starts_with_upper(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.to_lowercase().ne(std::iter::once(c)))
}

fn is_nullish(ast: &Ast, id: NodeId) -> bool {
    let id = skip_parens(ast, id);
    matches!(
        ast.node(id),
        Node::NullLit {}
            | Node::Unary {
                op: UnaryOp::Void,
                ..
            }
    ) || is_undefined(ast, id)
}

/// Callee of a call: the call when `id` (through parentheses) is its callee.
fn call_of_callee(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let p = parent_skip_parens(cx.ast(), cx.file.parents, id)?;
    match *cx.node(p) {
        Node::Call { callee, .. } if skip_parens(cx.ast(), callee) == skip_parens_up(cx, id) => {
            Some(p)
        }
        _ => None,
    }
}

/// The outermost parenthesized form of `id`.
fn skip_parens_up(cx: &Ctx<'_, '_>, mut id: NodeId) -> NodeId {
    while let Some(p) = cx.parent(id) {
        if !matches!(cx.node(p), Node::Paren { .. }) {
            break;
        }
        id = p;
    }
    skip_parens(cx.ast(), id)
}

/// The innermost function (of any kind) containing `id`.
fn upper_function(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    cx.file
        .parents
        .ancestors(id)
        .find(|&a| is_function(ast.kind(a)))
}

/// A comment right before `func` (or before the statement or declaration it starts, for a
/// documented function expression) has a `@this` tag.
fn has_this_tag(cx: &Ctx<'_, '_>, func: NodeId) -> bool {
    let ast = cx.ast();
    let src = cx.src();
    let tagged = |pos: u32| {
        let i = ast.comments.partition_point(|c| c.span.end <= pos);
        let Some(c) = i.checked_sub(1).map(|i| ast.comments[i]) else {
            return false;
        };
        src[c.span.end as usize..pos as usize].trim().is_empty()
            && c.span.text(src).lines().any(|l| {
                l.trim_start_matches(['/', '*', ' ', '\t'])
                    .starts_with("@this")
            })
    };
    if tagged(cx.span(func).start) {
        return true;
    }
    // `/** @this X */ return function () {}`, `/** @this X */ const f = function () {}`.
    let mut cur = func;
    for _ in 0..64 {
        let Some(p) = cx.parent(cur) else {
            return false;
        };
        match *cx.node(p) {
            Node::Return { .. } | Node::VarDecl { .. } | Node::ExprStmt { .. } => {
                return tagged(cx.span(p).start);
            }
            Node::Call { .. } | Node::New { .. } => return false,
            _ if p_is_function(ast, p) || cx.node(p).is_statement() => return false,
            _ => cur = p,
        }
    }
    false
}

fn p_is_function(ast: &Ast, id: NodeId) -> bool {
    is_function(ast.kind(id))
}

/// True when a function called plainly gets the default `this` (the global object in sloppy
/// code, `undefined` in strict code): not a method, not a constructor-looking function, not
/// a callback given an explicit `this`.
pub(crate) fn is_default_this_binding(cx: &Ctx<'_, '_>, func: NodeId) -> bool {
    is_default_this_binding_with(cx, func, true)
}

/// [`is_default_this_binding`]; `cap_is_constructor`: a capitalized name makes a constructor.
pub(crate) fn is_default_this_binding_with(
    cx: &Ctx<'_, '_>,
    func: NodeId,
    cap_is_constructor: bool,
) -> bool {
    let ast = cx.ast();
    let starts_with_upper = |n: &str| cap_is_constructor && starts_with_upper(n);
    if has_this_tag(cx, func) {
        return false;
    }
    let src = cx.src();
    let (name, params) = match *ast.node(func) {
        Node::FunctionDecl { name, params, .. } | Node::FunctionExpr { name, params, .. } => {
            (name, params)
        }
        _ => return true,
    };
    let this_param = ast.list(params).iter().any(|&p| {
        matches!(*ast.node(p), Node::Param { pattern, .. } if ast.span(pattern).text(src) == "this")
    });
    if this_param {
        return false;
    }
    if name
        .and_then(|n| ident_name(ast, n))
        .is_some_and(starts_with_upper)
    {
        return false;
    }
    let anonymous = name.is_none();
    let mut cur = func;
    // Each step moves to an ancestor, so the walk ends at the root.
    loop {
        let Some(parent) = cx.parent(cur) else {
            return true;
        };
        match *ast.node(parent) {
            Node::Paren { .. } => cur = parent,
            Node::Binary {
                op: BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce,
                ..
            }
            | Node::Conditional { .. } => cur = parent,
            Node::Return { .. } => {
                let Some(f) = upper_function(cx, parent) else {
                    return true;
                };
                let Some(call) = call_of_callee(cx, f) else {
                    return true;
                };
                cur = call;
            }
            Node::Arrow { body, flags, .. } => {
                if !flags.contains(FnFlags::EXPRESSION_BODY) || body != cur {
                    return true;
                }
                let Some(call) = call_of_callee(cx, parent) else {
                    return true;
                };
                cur = call;
            }
            Node::Property { value, .. } => return value != cur,
            Node::PropertyDef { value, .. } => return value != Some(cur),
            Node::MethodDef { value, .. } => return value != cur,
            Node::Assign { left, .. } | Node::AssignPattern { left, .. } => {
                let left = skip_parens(ast, left);
                if matches!(ast.node(left), Node::Member { .. } | Node::Index { .. }) {
                    return false;
                }
                return !(anonymous && ident_name(ast, left).is_some_and(starts_with_upper));
            }
            // `(Foo = function () {}) => ...` and `{ Foo = function () {} } = x`.
            Node::Param { pattern, init, .. }
            | Node::BindingProperty {
                value: Some(pattern),
                init,
                ..
            } if init == Some(cur) => {
                return !(anonymous && ident_name(ast, pattern).is_some_and(starts_with_upper));
            }
            Node::BindingProperty { key, init, .. }
            | Node::ShorthandProp {
                name: key, init, ..
            } if init == Some(cur) => {
                return !(anonymous && ident_name(ast, key).is_some_and(starts_with_upper));
            }
            Node::VarDeclarator { name, init, .. } => {
                return !(anonymous
                    && init == Some(cur)
                    && ident_name(ast, name).is_some_and(starts_with_upper));
            }
            Node::Member { obj, .. } | Node::Index { obj, .. } => {
                if obj != cur
                    || !member_name(ast, src, parent)
                        .is_some_and(|n| matches!(&*n, "bind" | "call" | "apply"))
                {
                    return true;
                }
                return !call_of_callee(cx, parent).is_some_and(|call| {
                    let Node::Call { args, .. } = *ast.node(call) else {
                        return false;
                    };
                    ast.list(args).first().is_some_and(|&a| !is_nullish(ast, a))
                });
            }
            Node::Call { callee, args, .. } => {
                let args = ast.list(args);
                let Some((obj, method)) = member_parts(ast, src, callee) else {
                    return true;
                };
                let obj_name = ident_name(ast, skip_parens(ast, obj));
                if method == "apply" && obj_name == Some("Reflect") {
                    return args.len() != 3 || args[0] != cur || is_nullish(ast, args[1]);
                }
                if matches!(&*method, "from" | "fromAsync")
                    && obj_name.is_some_and(|n| n.ends_with("Array"))
                {
                    return args.len() != 3 || args[1] != cur || is_nullish(ast, args[2]);
                }
                if matches!(
                    &*method,
                    "every"
                        | "filter"
                        | "find"
                        | "findIndex"
                        | "findLast"
                        | "findLastIndex"
                        | "flatMap"
                        | "forEach"
                        | "map"
                        | "some"
                ) {
                    return args.len() != 2 || args[0] != cur || is_nullish(ast, args[1]);
                }
                return true;
            }
            _ => return true,
        }
    }
}
