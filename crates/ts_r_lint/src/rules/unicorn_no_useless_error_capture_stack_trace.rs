//! `unicorn/no-useless-error-capture-stack-trace`: in the constructor of a class extending a
//! global built-in error (`Error`, `TypeError`, ... `SuppressedError`), calling the global
//! `Error.captureStackTrace(this, X)` repeats what the built-in constructor did, when `X` is
//! `new.target`, `this.constructor` or the class's own name. The call must sit directly in
//! the constructor (an arrow or nested function or class in between breaks it). The fix
//! deletes the expression statement when it is in a block and has no comment inside, leaving
//! `;` when the previous statement ends without one.

use super::prelude::*;
use super::unicorn_ast::{ends_without_semicolon, method_call, superclass};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-useless-error-capture-stack-trace",
        "Disallow unnecessary `Error.captureStackTrace(…)`.",
        NO_PRESET,
        Category::Suspicious,
    )
    .fixable(),
    create: create_default::<NoUselessErrorCaptureStackTrace>,
};

#[derive(Debug, Default)]
struct NoUselessErrorCaptureStackTrace;

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

impl Rule for NoUselessErrorCaptureStackTrace {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, node) else {
            return;
        };
        if call.name != "captureStackTrace"
            || call.optional_member
            || call.args.len() != 2
            || call
                .args
                .iter()
                .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
            || !cx.is_global_ref(skip_parens(ast, call.obj), "Error")
        {
            return;
        }
        // The nearest function or class must be the constructor of an error subclass.
        let Some(scope) = ancestors_inclusive(cx.file.parents, node)
            .skip(1)
            .find(|&a| {
                is_function(ast.kind(a))
                    || matches!(ast.kind(a), NodeKind::ClassDecl | NodeKind::ClassExpr)
            })
        else {
            return;
        };
        let Some(method) = cx.parent(scope) else {
            return;
        };
        let Node::MethodDef {
            kind: MethodKind::Constructor,
            value,
            ..
        } = *ast.node(method)
        else {
            return;
        };
        let Some(class) = cx.parent(method) else {
            return;
        };
        if value != scope || !matches!(ast.kind(class), NodeKind::ClassDecl | NodeKind::ClassExpr) {
            return;
        }
        let Some(sup) = superclass(ast, class) else {
            return;
        };
        if !matches!(ast.node(sup), Node::Ident { .. })
            || !BUILTIN_ERRORS
                .iter()
                .any(|e| cx.is_global_ref(sup, e) && ident_name(ast, sup) == Some(e))
        {
            return;
        }
        if !matches!(ast.node(skip_parens(ast, call.args[0])), Node::This {})
            || !is_class_reference(cx, skip_parens(ast, call.args[1]), class)
        {
            return;
        }
        let fix = cx.parent(node).and_then(|stmt| {
            let block = cx.parent(stmt)?;
            let span = cx.span(stmt);
            (matches!(ast.node(stmt), Node::ExprStmt { .. })
                && matches!(ast.node(block), Node::Block { .. })
                && ast.comments_in(span).iter().all(|c| c.span.end > span.end))
            .then(|| {
                let semi = prev_statement(ast, cx.file.parents, stmt)
                    .is_some_and(|p| ends_without_semicolon(cx, p));
                Fix::replace(span, if semi { ";" } else { "" })
            })
        });
        cx.report_node(node, "Unnecessary `Error.captureStackTrace(…)` call.")
            .id("no-useless-error-capture-stack-trace/error")
            .fix_opt(fix);
    }
}

/// `new.target`, `this.constructor`, or a name bound only to `class` itself.
fn is_class_reference(cx: &Ctx<'_, '_>, id: NodeId, class: NodeId) -> bool {
    let ast = cx.ast();
    match *ast.node(id) {
        Node::MetaProperty {
            kind: ts_r_ast::MetaPropKind::NewTarget,
        } => true,
        Node::Member {
            obj,
            optional: false,
            prop,
        } => {
            ident_name(ast, prop) == Some("constructor")
                && matches!(ast.node(skip_parens(ast, obj)), Node::This {})
        }
        Node::Ident { .. } => {
            let (Node::ClassDecl { name: Some(_), .. } | Node::ClassExpr { name: Some(_), .. }) =
                *ast.node(class)
            else {
                return false;
            };
            let sem = cx.semantic();
            let Some(Resolution::Symbol(s)) =
                sem.reference_at(id).map(|r| sem.reference(r).resolution)
            else {
                return false;
            };
            let decls = &sem.symbol(s).decls;
            decls.len() == 1 && decls[0].kind == DeclKind::Class && decls[0].node == class
        }
        _ => false,
    }
}
