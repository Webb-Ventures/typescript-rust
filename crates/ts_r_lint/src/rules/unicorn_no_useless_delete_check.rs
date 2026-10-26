//! `unicorn/no-useless-delete-check`: an `if` (no `else`, not an `else if`) whose only
//! statement deletes what its test checked for.
//!
//! - `if (key in object) delete object[key];` (or `.name`) on a known object: a static
//!   object value, a `const` initialized with an object, array, function or class literal,
//!   or a binding annotated `object`, a function or a constructor type (a union of those).
//!   The key must match: the same reference that can't need repeated coercion (not an
//!   object, a function or class, or a `const` from `new` or an opaque call), or the same
//!   static key. Test operands and computed keys must be free of side effects, getters
//!   included. A suggestion, since a `Proxy` can observe the `delete`.
//! - `if (map.has(k)) map.delete(k);` on a `const` initialized with `new Map/Set/WeakMap/
//!   WeakSet` (the globals), one plain argument each, the same receiver and argument, no
//!   side effects in it: a fix.
//!
//! The `if` becomes the statement (with a `;` added when missing, and one in front when it
//! would continue the previous line); nothing is offered when the `if` holds a comment.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_static::{
    Static, const_definition, has_side_effect_with_getters, mutable_member_access, static_value,
    static_value_no_side_effects,
};
use super::unicorn_text::{has_unparenthesized_optional_chain, needs_semicolon};
use super::unicorn_types::binding_annotation;
use super::unicorn_util::{is_same_reference, skip_wrappers as unwrap_ts};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-useless-delete-check",
        "Disallow unnecessary existence checks before deletion.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<NoUselessDeleteCheck>,
};

#[derive(Debug, Default)]
struct NoUselessDeleteCheck;

fn is_object_value(v: &Static) -> bool {
    matches!(
        v,
        Static::Array(_) | Static::Object(_) | Static::RegExp { .. } | Static::Builtin(_)
    )
}

/// The declarations of the name `id` refers to.
fn decls_of<'a>(cx: &'a Ctx<'_, '_>, id: NodeId) -> &'a [crate::semantic::Decl] {
    let sem = cx.semantic();
    match sem.symbol_of_ident(id) {
        Some(s) => &sem.symbol(s).decls,
        None => &[],
    }
}

fn is_function_or_class_reference(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    matches!(cx.node(id), Node::Ident { .. })
        && decls_of(cx, id).iter().any(|d| {
            matches!(
                d.kind,
                DeclKind::Function
                    | DeclKind::Class
                    | DeclKind::FunctionExprName
                    | DeclKind::ClassExprName
            )
        })
}

fn is_object_type(ast: &Ast, mut ty: NodeId, depth: u32) -> bool {
    for _ in 0..1000 {
        match *ast.node(ty) {
            Node::ParenType { ty: inner } => ty = inner,
            Node::KeywordType {
                kind: KeywordTypeKind::Object,
            }
            | Node::FunctionType { .. }
            | Node::ConstructorType { .. } => return true,
            Node::UnionType { types } => {
                return depth < 100
                    && ast
                        .list(types)
                        .iter()
                        .all(|&t| is_object_type(ast, t, depth + 1));
            }
            _ => return false,
        }
    }
    false
}

fn has_object_annotation(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    if !matches!(cx.node(id), Node::Ident { .. }) {
        return false;
    }
    let Some(decl) = decls_of(cx, id).first() else {
        return false;
    };
    binding_annotation(cx, decl.name)
        .0
        .is_some_and(|t| is_object_type(cx.ast(), t, 0))
}

fn is_known_object(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    if let Some(v) = static_value(cx, id) {
        return is_object_value(&v);
    }
    let ast = cx.ast();
    let inner = unwrap_ts(ast, id);
    if let Some((_, init)) = const_definition(cx, inner) {
        let init = unwrap_ts(ast, init);
        if let Some(v) = static_value(cx, init) {
            return is_object_value(&v);
        }
        if matches!(
            ast.node(init),
            Node::ArrayLit { .. }
                | Node::Arrow { .. }
                | Node::ClassExpr { .. }
                | Node::FunctionExpr { .. }
                | Node::ObjectLit { .. }
        ) {
            return true;
        }
    }
    !is_function_or_class_reference(cx, inner) && has_object_annotation(cx, inner)
}

fn static_key(cx: &Ctx<'_, '_>, id: NodeId) -> Option<String> {
    let result = static_value_no_side_effects(cx, id);
    if result.is_none() && mutable_member_access(cx, id, &mut Vec::new()) {
        return None;
    }
    let v = result.or_else(|| static_value(cx, id))?;
    if is_object_value(&v) {
        return None;
    }
    v.to_js_string()
}

fn needs_repeated_coercion(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let inner = unwrap_ts(ast, id);
    if is_known_object(cx, id) || is_function_or_class_reference(cx, inner) {
        return true;
    }
    let Some((_, init)) = const_definition(cx, inner) else {
        return false;
    };
    let init = unwrap_ts(ast, init);
    matches!(ast.node(init), Node::New { .. })
        || (matches!(ast.node(init), Node::Call { .. }) && static_key(cx, init).is_none())
}

fn same_key(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
    if is_same_reference(cx, a, b) {
        return !needs_repeated_coercion(cx, a);
    }
    static_key(cx, a).is_some_and(|k| static_key(cx, b) == Some(k))
}

fn safe(ast: &Ast, id: NodeId) -> bool {
    !has_side_effect_with_getters(ast, id)
}

/// `obj.method(x)`: one non-spread argument, a plain name, no optional chaining.
fn one_argument_call(ast: &Ast, id: NodeId, method: &str) -> Option<(NodeId, NodeId)> {
    let call = method_call(ast, id)?;
    (call.name == method
        && call.plain()
        && matches!(call.args, [a] if !matches!(ast.node(*a), Node::Spread { .. })))
    .then(|| (call.obj, call.args[0]))
}

fn is_known_const_collection(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let Some((_, init)) = const_definition(cx, unwrap_ts(ast, id)) else {
        return false;
    };
    let Node::New { callee, .. } = *ast.node(unwrap_ts(ast, init)) else {
        return false;
    };
    matches!(ast.node(callee), Node::Ident { .. })
        && ["Map", "Set", "WeakMap", "WeakSet"]
            .iter()
            .any(|name| ident_name(ast, callee) == Some(name) && cx.is_global_ref(callee, name))
}

fn object_delete_matches(cx: &Ctx<'_, '_>, test: NodeId, argument: NodeId) -> bool {
    let ast = cx.ast();
    let Node::Binary {
        left,
        op: BinaryOp::In,
        right,
    } = *ast.node(test)
    else {
        return false;
    };
    let (object, key, computed) = match *ast.node(argument) {
        Node::Index {
            obj,
            index,
            optional: false,
        } => (obj, index, true),
        Node::Member {
            obj,
            prop,
            optional: false,
        } => (obj, prop, false),
        _ => return false,
    };
    if has_unparenthesized_optional_chain(cx, argument)
        || !is_same_reference(cx, right, object)
        || !is_known_object(cx, right)
        || !safe(ast, left)
        || !safe(ast, right)
        || (computed && !safe(ast, key))
    {
        return false;
    }
    if computed {
        return same_key(cx, left, key);
    }
    let Node::Ident { .. } = ast.node(key) else {
        return false;
    };
    ident_name(ast, key).is_some_and(|name| static_key(cx, left).as_deref() == Some(name))
}

fn collection_delete_matches(cx: &Ctx<'_, '_>, test: NodeId, expr: NodeId) -> bool {
    let ast = cx.ast();
    let (Some((has_obj, has_arg)), Some((delete_obj, delete_arg))) = (
        one_argument_call(ast, unwrap_ts(ast, test), "has"),
        one_argument_call(ast, unwrap_ts(ast, expr), "delete"),
    ) else {
        return false;
    };
    is_known_const_collection(cx, has_obj)
        && is_same_reference(cx, has_obj, delete_obj)
        && is_same_reference(cx, has_arg, delete_arg)
        && safe(ast, has_arg)
}

fn fix(cx: &Ctx<'_, '_>, if_stmt: NodeId, stmt: NodeId) -> Option<Fix> {
    let span = cx.span(if_stmt);
    if !cx.ast().comments_in(span).is_empty() {
        return None;
    }
    let mut text = cx.text(stmt).to_owned();
    if !text.trim_end().ends_with(';') {
        text.push(';');
    }
    if needs_semicolon(cx, span.start, &text) {
        text.insert(0, ';');
    }
    Some(Fix::replace(span, text))
}

impl Rule for NoUselessDeleteCheck {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::If]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::If {
            test,
            cons,
            alt: None,
        } = *cx.node(node)
        else {
            return;
        };
        if cx
            .parent(node)
            .is_some_and(|p| matches!(*cx.node(p), Node::If { alt: Some(a), .. } if a == node))
        {
            return;
        }
        let stmt = match *ast.node(cons) {
            Node::Block { stmts } => match ast.list(stmts) {
                [only] => *only,
                _ => return,
            },
            _ => cons,
        };
        let Node::ExprStmt { expr } = *ast.node(stmt) else {
            return;
        };
        let expr = skip_parens(ast, expr);
        let test = skip_parens(ast, test);
        let msg = "The existence check before deletion is unnecessary.";
        let id = "no-useless-delete-check";
        if let Node::Unary {
            op: UnaryOp::Delete,
            arg,
        } = *ast.node(expr)
        {
            if !object_delete_matches(cx, test, skip_parens(ast, arg)) {
                return;
            }
            let fix = fix(cx, node, stmt);
            let report = cx.report(cx.span(test), msg).id(id);
            if let Some(fix) = fix {
                report.suggest("removeCheck", "Remove the existence check.", fix);
            }
            return;
        }
        if !collection_delete_matches(cx, test, expr) {
            return;
        }
        let fix = fix(cx, node, stmt);
        cx.report(cx.span(test), msg).id(id).fix_opt(fix);
    }
}
