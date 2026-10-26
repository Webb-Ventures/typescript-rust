//! `unicorn/no-this-outside-of-class`: `this` is reported unless the nearest enclosing
//! non-arrow function is a class method (constructor, accessor) or declares a `this`
//! parameter, or the nearest such scope is a class static block or field initializer.
//! Object-literal methods, plain functions and the top level are reported; class keys and
//! `extends` clauses belong to the scope around the class. `typeof this` in a type is fine.

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-this-outside-of-class",
        "Disallow `this` outside of classes.",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create_default::<NoThisOutsideOfClass>,
};

#[derive(Debug, Default)]
struct NoThisOutsideOfClass;

fn is_class(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::ClassDecl { .. } | Node::ClassExpr { .. }
    )
}

fn has_this_param(ast: &Ast, params: ts_r_ast::NodeList) -> bool {
    ast.list(params).iter().any(|&p| {
        matches!(*ast.node(p), Node::Param { pattern, .. } if ident_name(ast, pattern) == Some("this"))
    })
}

fn in_type_query(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = node;
    while let Some(p) = cx.parent(cur) {
        match ast.node(p) {
            Node::QualifiedName { .. } => cur = p,
            Node::TypeQuery { expr_name, .. } => return *expr_name == cur,
            _ => return false,
        }
    }
    false
}

fn allowed(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let mut child = node;
    while let Some(parent) = cx.parent(child) {
        match *ast.node(parent) {
            Node::FunctionDecl { params, .. } => return has_this_param(ast, params),
            Node::FunctionExpr { params, .. } => {
                let method = cx.parent(parent).is_some_and(|m| {
                    matches!(*ast.node(m), Node::MethodDef { value, .. } if value == parent)
                        && cx.parent(m).is_some_and(|c| is_class(ast, c))
                });
                return method || has_this_param(ast, params);
            }
            Node::StaticBlock { .. } => return true,
            Node::PropertyDef { value, .. }
                if value == Some(child) && cx.parent(parent).is_some_and(|c| is_class(ast, c)) =>
            {
                return true;
            }
            _ => {}
        }
        child = parent;
    }
    false
}

impl Rule for NoThisOutsideOfClass {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::This]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if in_type_query(cx, node) || allowed(cx, node) {
            return;
        }
        cx.report(cx.span(node), "Do not use `this` outside of classes.")
            .id("no-this-outside-of-class");
    }
}
