//! `unicorn/prefer-native-coercion-functions`: use `String`, `Number`, `BigInt`, `Boolean`
//! and `Symbol` directly instead of wrapping them.
//!
//! A function (not async, not a generator, no type-predicate return type, not a
//! constructor or setter) whose first parameter is a plain identifier `v` is reported at its
//! head when
//! - it returns `Coercion(v, ...)` (an arrow's expression body or the only `return`, TS
//!   wrappers removed; the callee written as one of the five names, not an optional call);
//! - or it returns `v` itself and is the first argument of a non-optional
//!   `.every/.filter/.find/.findLast/.findIndex/.findLastIndex/.some(...)` call (then
//!   `Boolean`).
//!
//! The fix replaces the function by the name: in an object method `foo(v) {}` it becomes
//! `foo: Name`, a class method becomes the field `foo = Name;` (a suggestion: that moves it
//! off the prototype), and a `;` is added when a token on a later line could continue the
//! expression. No fix for declarations, extra parameters or call arguments, comments inside
//! the function, or an operand of `delete`.

use super::prelude::*;
use super::unicorn_util::{space_around_keyword, with_edits};
use super::unicorn_util_h5::{utils_function_head_span, utils_function_name_with_kind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-native-coercion-functions",
        "Prefer using `String`, `Number`, `BigInt`, `Boolean`, and `Symbol` directly.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferNativeCoercion>,
};

#[derive(Debug, Default)]
struct PreferNativeCoercion;

const COERCIONS: &[&str] = &["String", "Number", "BigInt", "Boolean", "Symbol"];
const BOOLEAN_CALLBACK_METHODS: &[&str] = &[
    "every",
    "filter",
    "find",
    "findLast",
    "findIndex",
    "findLastIndex",
    "some",
];

impl Rule for PreferNativeCoercion {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::FunctionDecl,
            NodeKind::FunctionExpr,
            NodeKind::Arrow,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (flags, params, return_type, body) = match *cx.node(node) {
            Node::FunctionDecl {
                flags,
                params,
                return_type,
                body: Some(body),
                ..
            }
            | Node::FunctionExpr {
                flags,
                params,
                return_type,
                body: Some(body),
                ..
            }
            | Node::Arrow {
                flags,
                params,
                return_type,
                body,
                ..
            } => (flags, params, return_type, body),
            _ => return,
        };
        if flags.intersects(FnFlags::ASYNC | FnFlags::GENERATOR) {
            return;
        }
        if let Some(rt) = return_type
            && matches!(ast.node(rt), Node::TypePredicate { .. })
        {
            return;
        }
        let params = ast.list(params);
        let Some(&first) = params.first() else {
            return;
        };
        let Node::Param {
            rest: false,
            pattern,
            init: None,
            ..
        } = *ast.node(first)
        else {
            return;
        };
        let Some(param) = ident_name(ast, pattern) else {
            return;
        };
        let parent = cx.parent(node);
        let method_parent = parent.filter(|&p| matches!(cx.node(p), Node::MethodDef { .. }));
        if let Some(p) = method_parent
            && let Node::MethodDef { kind, .. } = *cx.node(p)
            && matches!(kind, MethodKind::Constructor | MethodKind::Set)
        {
            return;
        }
        let returned = returned_expr(cx, node, body, flags);
        let (name, fixable) = if let Some(r) = returned
            && is_identity(ast, r, param)
            && is_boolean_callback(cx, node)
        {
            ("Boolean", true)
        } else if let Some(r) = returned
            && let Node::Call {
                callee,
                optional: false,
                args,
                ..
            } = *ast.node(r)
            && let Some(callee) = ident_name(ast, skip_parens(ast, callee))
            && let Some(name) = COERCIONS.iter().find(|n| **n == callee)
            && let Some(&arg) = ast.list(args).first()
            && ident_name(ast, arg) == Some(param)
        {
            let fixable =
                !matches!(cx.node(node), Node::FunctionDecl { .. }) && ast.list(args).len() == 1;
            (*name, fixable)
        } else {
            return;
        };
        let head = utils_function_head_span(cx, node);
        let kind = utils_function_name_with_kind(cx, node);
        let message = format!("{kind} is equivalent to `{name}`. Use `{name}` directly.");
        let span = cx.span(node);
        let fix = (fixable
            && params.len() == 1
            && !under_delete(cx, node)
            && ast.comments_in(span).is_empty())
        .then(|| {
            let class_method = method_parent.is_some_and(|p| {
                !cx.parent(p)
                    .is_some_and(|o| matches!(cx.node(o), Node::ObjectLit { .. }))
            });
            let text = if class_method {
                format!(" = {name};")
            } else if method_parent.is_some() {
                format!(": {name}")
            } else if needs_semicolon_after(cx, node) {
                format!("{name};")
            } else {
                name.to_owned()
            };
            (
                class_method,
                with_edits(Fix::replace(span, text), space_around_keyword(cx, node)),
            )
        });
        let r = cx
            .report(head, message)
            .id("prefer-native-coercion-functions");
        match fix {
            Some((true, f)) => {
                r.suggest(
                    "prefer-native-coercion-functions/suggestion",
                    format!("Replace with `{name}`."),
                    f,
                );
            }
            Some((false, f)) => {
                r.fix(f);
            }
            None => {}
        }
    }
}

/// What the function returns when its body is one expression (arrow) or one `return`,
/// without TS wrappers.
fn returned_expr(cx: &Ctx<'_, '_>, f: NodeId, body: NodeId, flags: FnFlags) -> Option<NodeId> {
    let ast = cx.ast();
    let expr =
        if matches!(cx.node(f), Node::Arrow { .. }) && flags.contains(FnFlags::EXPRESSION_BODY) {
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
    Some(skip_ts_wrappers(ast, skip_parens(ast, expr)))
}

fn is_identity(ast: &ts_r_ast::Ast, expr: NodeId, param: &str) -> bool {
    ident_name(ast, skip_parens(ast, expr)) == Some(param)
}

/// The first argument of a non-optional `.some(...)`-like call.
fn is_boolean_callback(cx: &Ctx<'_, '_>, f: NodeId) -> bool {
    let ast = cx.ast();
    let Some(call) = parent_skip_parens(ast, cx.file.parents, f) else {
        return false;
    };
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(call)
    else {
        return false;
    };
    if ast.list(args).first().map(|&a| skip_parens(ast, a)) != Some(f) {
        return false;
    }
    let Node::Member {
        optional: false,
        prop,
        ..
    } = *ast.node(skip_parens(ast, callee))
    else {
        return false;
    };
    ident_name(ast, prop).is_some_and(|n| BOOLEAN_CALLBACK_METHODS.contains(&n))
}

/// The function (through parentheses and TS wrappers) is the operand of `delete`.
fn under_delete(cx: &Ctx<'_, '_>, f: NodeId) -> bool {
    let mut cur = f;
    while let Some(p) = cx.parent(cur) {
        match *cx.node(p) {
            Node::Paren { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
            | Node::Instantiation { .. } => cur = p,
            Node::Unary {
                op: UnaryOp::Delete,
                ..
            } => return true,
            _ => return false,
        }
    }
    false
}

/// A token after the function that starts at or after the end of its (ESTree) parent and
/// would continue an identifier: `(`, `[`, `+`, `-`, a regex or a template.
fn needs_semicolon_after(cx: &Ctx<'_, '_>, f: NodeId) -> bool {
    let ast = cx.ast();
    let src = cx.src();
    let next = skip_trivia_forward(src, cx.span(f).end as usize);
    let Some(&c) = src.as_bytes().get(next) else {
        return false;
    };
    let Some(parent) = parent_skip_parens(ast, cx.file.parents, f) else {
        return false;
    };
    if (next as u32) < ast.span(parent).end {
        return false;
    }
    match c {
        b'(' | b'[' | b'+' | b'-' | b'`' => true,
        b'/' => !matches!(src.as_bytes().get(next + 1), Some(b'/' | b'*')),
        _ => false,
    }
}
