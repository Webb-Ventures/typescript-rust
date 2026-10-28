//! `unicorn/error-message`: a built-in error created without a message (`new Error()`,
//! `TypeError()`), or with a message known to be an empty string or not a string, is
//! reported. The message is argument 0 (1 for `AggregateError`, 2 for `SuppressedError`);
//! a spread at or before it means the message is unknown.
//!
//! The message's value comes from static evaluation (`unicorn_static`): plain evaluation
//! when it reads nothing mutable, otherwise only values with no side effects (or, for
//! `?:` / `&&` / `||` / `??`, the control-flow-safe variant). A member of
//! `Object.freeze({...})` (inline or through a `const` that is only read there) is
//! evaluated plainly. Array and object literals are never strings.

use super::prelude::*;
use super::unicorn_static::{
    Static, const_definition, mutable_member_access, skip_ts_only, static_value,
    static_value_control_flow, static_value_no_side_effects,
};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/error-message",
        "Enforce passing a `message` value when creating a built-in error.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<ErrorMessage>,
};

#[derive(Debug, Default)]
struct ErrorMessage;

const BUILTIN_ERRORS: &[&str] = &[
    "Error",
    "EvalError",
    "RangeError",
    "ReferenceError",
    "SyntaxError",
    "TypeError",
    "URIError",
    "AggregateError",
    "SuppressedError",
];

impl Rule for ErrorMessage {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (callee, args) = match *cx.node(node) {
            Node::Call {
                callee,
                optional: false,
                args,
                ..
            } => (callee, args),
            Node::New { callee, args, .. } => (callee, args),
            _ => return,
        };
        let callee = skip_parens(ast, callee);
        let Some(name) = ident_name(ast, callee) else {
            return;
        };
        if !BUILTIN_ERRORS.contains(&name) || !cx.is_global_ref(callee, name) {
            return;
        }
        let index = match name {
            "AggregateError" => 1,
            "SuppressedError" => 2,
            _ => 0,
        };
        let args = ast.list(args);
        if args
            .iter()
            .take(index + 1)
            .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
        {
            return;
        }
        let Some(&message) = args.get(index) else {
            let msg = format!("Pass a message to the `{name}` constructor.");
            cx.report_node(node, msg).id("missing-message");
            return;
        };
        if matches!(
            ast.node(skip_parens(ast, message)),
            Node::ArrayLit { .. } | Node::ObjectLit { .. }
        ) {
            cx.report_node(message, "Error message should be a string.")
                .id("message-is-not-a-string");
            return;
        }
        match message_value(cx, message) {
            Some(Static::Str(s)) if s.is_empty() => {
                cx.report_node(message, "Error message should not be an empty string.")
                    .id("message-is-empty-string");
            }
            Some(Static::Str(_)) | None => {}
            Some(_) => {
                cx.report_node(message, "Error message should be a string.")
                    .id("message-is-not-a-string");
            }
        }
    }
}

/// `?:` or a logical expression, under any unary operators.
fn is_branch(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    let mut id = skip_ts_only(ast, id);
    for _ in 0..10_000 {
        match *ast.node(id) {
            Node::Unary { arg, .. } => id = skip_ts_only(ast, arg),
            Node::Conditional { .. } => return true,
            Node::Binary { op, .. } => {
                return matches!(
                    op,
                    BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce
                );
            }
            _ => return false,
        }
    }
    false
}

fn message_value(cx: &Ctx<'_, '_>, message: NodeId) -> Option<Static> {
    let ast = cx.ast();
    let value = skip_ts_only(ast, message);
    let branch = is_branch(ast, value);
    if branch || mutable_member_access(cx, value, &mut Vec::new()) {
        if is_frozen_object_member(cx, value) {
            return static_value(cx, value);
        }
        return if branch {
            static_value_control_flow(cx, value)
        } else {
            static_value_no_side_effects(cx, value)
        };
    }
    static_value(cx, value)
}

/// `Object.freeze({...}).key` (the object directly, or a `const` initialised from one), or
/// a `const` initialised from such a member.
fn is_frozen_object_member(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = match const_definition(cx, id) {
        Some((_, init)) => init,
        None if matches!(ast.node(id), Node::Ident { .. }) => return false,
        None => id,
    };
    let id = skip_parens(ast, id);
    let (Node::Member { obj, .. } | Node::Index { obj, .. }) = *ast.node(id) else {
        return false;
    };
    let Node::Call { callee, args, .. } = *ast.node(skip_parens(ast, obj)) else {
        return false;
    };
    let Node::Member {
        obj: object, prop, ..
    } = *ast.node(skip_parens(ast, callee))
    else {
        return false;
    };
    if ident_name(ast, skip_parens(ast, object)) != Some("Object")
        || ident_name(ast, prop) != Some("freeze")
    {
        return false;
    }
    ast.list(args)
        .first()
        .is_some_and(|&a| is_safe_freeze_argument(cx, a))
}

fn is_safe_freeze_argument(cx: &Ctx<'_, '_>, arg: NodeId) -> bool {
    let ast = cx.ast();
    let arg = skip_parens(ast, arg);
    match *ast.node(arg) {
        Node::ObjectLit { .. } => !mutable_member_access(cx, arg, &mut Vec::new()),
        Node::Ident { .. } => {
            let sem = cx.semantic();
            let Some(s) = sem.symbol_of_ident(arg) else {
                return false;
            };
            let sym = sem.symbol(s);
            let [decl] = sym.decls.as_slice() else {
                return false;
            };
            let Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } = *ast.node(decl.node)
            else {
                return false;
            };
            decl.kind == DeclKind::Const
                && name == decl.name
                && matches!(ast.node(skip_parens(ast, init)), Node::ObjectLit { .. })
                && !mutable_member_access(cx, init, &mut Vec::new())
                && sym.refs.iter().all(|&r| {
                    let r = sem.reference(r);
                    r.flags.contains(RefFlags::INIT) || r.node == arg
                })
        }
        _ => false,
    }
}
