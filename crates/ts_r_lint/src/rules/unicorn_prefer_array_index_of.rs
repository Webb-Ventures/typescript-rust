//! `unicorn/prefer-array-index-of`: `indexOf(x)` / `lastIndexOf(x)` over
//! `findIndex(e => e === x)` / `findLastIndex(e => e === x)`.
//!
//! A non-optional call of a non-computed `.findIndex` / `.findLastIndex` member with one
//! non-spread argument is reported at the method name when the argument is a simple
//! comparison callback: a non-async, non-generator arrow or function expression with one
//! plain identifier parameter whose single returned expression (concise body or a lone
//! `return`) is `param === value` or `value === param`. Skipped when the receiver is known
//! not to be an array or typed array (`unicorn_types`), when the parameter is used anywhere
//! else, or when a function expression uses `this`, `arguments` (directly, not in nested
//! functions) or its own name.
//!
//! The fix renames the method and replaces the callback with the value (parenthesized when
//! it was and the callback wasn't); it is a suggestion when the value has side effects
//! (it is then evaluated once up front), and dropped when the callback holds a comment.

use super::prelude::*;
use super::unicorn_static::has_side_effect;
use super::unicorn_types::should_skip_known_non_array_receiver;
use super::unicorn_util::is_parenthesized;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-array-index-of",
        "Prefer `Array#{indexOf,lastIndexOf}()` over `Array#{findIndex,findLastIndex}()` when looking for the index of an item.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferArrayIndexOf>,
};

#[derive(Debug, Default)]
struct PreferArrayIndexOf;

impl Rule for PreferArrayIndexOf {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        simple_array_search(cx, node, "findIndex", "indexOf");
        simple_array_search(cx, node, "findLastIndex", "lastIndexOf");
    }
}

/// unicorn's shared "simple array search" check: `recv.<method>(e => e === v)` becomes
/// `recv.<replacement>(v)`.
pub(crate) fn simple_array_search(
    cx: &mut Ctx<'_, '_>,
    node: NodeId,
    method: &str,
    replacement: &str,
) {
    let ast = cx.ast();
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *cx.node(node)
    else {
        return;
    };
    let Node::Member { obj, prop, .. } = *ast.node(skip_parens(ast, callee)) else {
        return;
    };
    let &[arg] = ast.list(args) else {
        return;
    };
    if ident_name(ast, prop) != Some(method) {
        return;
    }
    let callback = skip_parens(ast, arg);
    let Some((param, param_name, compare)) = simple_compare(ast, callback) else {
        return;
    };
    if should_skip_known_non_array_receiver(cx, obj) {
        return;
    }
    let Node::Binary { left, right, .. } = *ast.node(compare) else {
        return;
    };
    let (value, param_use) = if ident_name(ast, skip_parens(ast, left)) == Some(param_name) {
        (right, skip_parens(ast, left))
    } else if ident_name(ast, skip_parens(ast, right)) == Some(param_name) {
        (left, skip_parens(ast, right))
    } else {
        return;
    };
    let sem = cx.semantic();
    let Some(sym) = sem.symbol_of_decl(param) else {
        return;
    };
    if sem
        .symbol(sym)
        .refs
        .iter()
        .any(|&r| sem.reference(r).node != param_use)
        || function_self_used(cx, callback)
    {
        return;
    }
    let inner = skip_parens(ast, value);
    let fix = ast.comments_in(cx.span(callback)).is_empty().then(|| {
        let mut text = cx.text(inner).to_owned();
        if inner != value && !is_parenthesized(cx, callback) {
            text = format!("({text})");
        }
        Fix::replace(ast.span(prop), replacement).and(cx.span(callback), text)
    });
    let side_effect = has_side_effect(cx.ast(), value);
    let (error, suggestion) = message_ids(method);
    let message = match method {
        "findIndex" => {
            "Use `.indexOf()` instead of `.findIndex()` when looking for the index of an item."
                .to_owned()
        }
        "findLastIndex" => "Use `.lastIndexOf()` instead of `.findLastIndex()` when looking for the index of an item.".to_owned(),
        _ => format!(
            "Use `.{replacement}()` instead of `.{method}()` when checking value existence."
        ),
    };
    let r = cx.report_node(prop, message).id(error);
    if let Some(f) = fix {
        if side_effect {
            r.suggest(
                suggestion,
                format!("Replace `.{method}()` with `.{replacement}()`."),
                f,
            );
        } else {
            r.fix(f);
        }
    }
}

fn message_ids(method: &str) -> (&'static str, &'static str) {
    match method {
        "findIndex" => (
            "prefer-indexOf-over-findIndex/error",
            "prefer-indexOf-over-findIndex/suggestion",
        ),
        "findLastIndex" => (
            "prefer-lastIndexOf-over-findLastIndex/error",
            "prefer-lastIndexOf-over-findLastIndex/suggestion",
        ),
        _ => (
            "prefer-includes-over-some/error",
            "prefer-includes-over-some/suggestion",
        ),
    }
}

/// For `p => p === v` style callbacks: the parameter's identifier, its name and the `===`.
fn simple_compare(ast: &ts_r_ast::Ast, f: NodeId) -> Option<(NodeId, &str, NodeId)> {
    let (flags, params, body) = match *ast.node(f) {
        Node::Arrow {
            flags,
            params,
            body,
            ..
        } => (flags, params, body),
        Node::FunctionExpr {
            flags,
            params,
            body: Some(body),
            ..
        } => (flags, params, body),
        _ => return None,
    };
    if flags.intersects(FnFlags::ASYNC | FnFlags::GENERATOR) {
        return None;
    }
    let &[param] = ast.list(params) else {
        return None;
    };
    let Node::Param {
        pattern,
        rest: false,
        init: None,
        ..
    } = *ast.node(param)
    else {
        return None;
    };
    let name = ident_name(ast, pattern)?;
    let expr = if flags.contains(FnFlags::EXPRESSION_BODY) {
        body
    } else {
        let Node::Block { stmts } = *ast.node(body) else {
            return None;
        };
        let &[only] = ast.list(stmts) else {
            return None;
        };
        let Node::Return { arg: Some(arg) } = *ast.node(only) else {
            return None;
        };
        arg
    };
    let expr = skip_parens(ast, expr);
    let Node::Binary {
        left,
        op: BinaryOp::StrictEq,
        right,
    } = *ast.node(expr)
    else {
        return None;
    };
    let is_param = |id: NodeId| ident_name(ast, skip_parens(ast, id)) == Some(name);
    (is_param(left) || is_param(right)).then_some((pattern, name, expr))
}

/// unicorn's `isFunctionSelfUsedInside`: a function expression (not an arrow) that uses
/// `this` or `arguments` outside nested functions, or refers to its own name.
fn function_self_used(cx: &Ctx<'_, '_>, f: NodeId) -> bool {
    let ast = cx.ast();
    let Node::FunctionExpr { name, body, .. } = *ast.node(f) else {
        return false;
    };
    if let Some(name) = name {
        let sem = cx.semantic();
        if sem
            .symbol_of_decl(name)
            .is_some_and(|s| !sem.symbol(s).refs.is_empty())
        {
            return true;
        }
    }
    fn walk(ast: &ts_r_ast::Ast, id: NodeId, depth: u32) -> bool {
        if depth > 500 {
            return false;
        }
        match *ast.node(id) {
            Node::This {} => return true,
            Node::Ident { name } if ast.atom(name) == "arguments" => return true,
            Node::FunctionDecl { .. }
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::PropertyDef { .. }
            | Node::StaticBlock { .. } => return false,
            Node::Member { obj, .. } => return walk(ast, obj, depth + 1),
            _ => {}
        }
        let mut found = false;
        ast.for_each_child(id, |c| found = found || walk(ast, c, depth + 1));
        found
    }
    body.is_some_and(|b| walk(ast, b, 0))
}
