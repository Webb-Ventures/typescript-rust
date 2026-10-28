//! `unicorn/no-accessor-recursion`: a getter reading its own property through `this`
//! (`this.foo`, `this["foo"]`, `const {foo} = this`) or a setter writing it. The accessor is
//! the closest non-arrow function around `this`, stopping at a class boundary; it must be a
//! `get`/`set` with a plain identifier or private-name key. A pure `this.foo = x` in a getter
//! goes to the setter, so it is not a read; `+=` and `++` are.

use super::prelude::*;
use super::unicorn_text::static_string_value;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-accessor-recursion",
        "Disallow recursive access to `this` within getters and setters.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoAccessorRecursion>,
};

#[derive(Debug, Default)]
struct NoAccessorRecursion;

/// A key as (is private, name).
fn key_of(ast: &Ast, key: NodeId) -> Option<(bool, String)> {
    match *ast.node(key) {
        Node::PrivateName { .. } => Some((true, ast.name_of(key)?.to_owned())),
        Node::Ident { .. } => Some((false, ast.name_of(key)?.to_owned())),
        _ => static_string_value(ast, key).map(|s| (false, s)),
    }
}

/// The getter or setter whose body `this` is in: (kind, key).
fn accessor(cx: &Ctx<'_, '_>, this: NodeId) -> Option<(MethodKind, (bool, String))> {
    let ast = cx.ast();
    let sem = cx.semantic();
    let mut s = Some(sem.enclosing_scope(this));
    while let Some(id) = s {
        let scope = sem.scope(id);
        match scope.kind {
            ScopeKind::Class => return None,
            ScopeKind::Function if ast.kind(scope.node) != NodeKind::Arrow => {
                let method = cx.parent(scope.node)?;
                let Node::MethodDef { kind, key, .. } = *ast.node(method) else {
                    return None;
                };
                if !matches!(kind, MethodKind::Get | MethodKind::Set)
                    || !matches!(ast.kind(key), NodeKind::Ident | NodeKind::PrivateName)
                {
                    return None;
                }
                return Some((kind, key_of(ast, key)?));
            }
            _ => {}
        }
        s = scope.parent;
    }
    None
}

impl Rule for NoAccessorRecursion {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::This]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some((kind, key)) = accessor(cx, node) else {
            return;
        };
        let Some(parent) = parent_skip_parens(ast, cx.file.parents, node) else {
            return;
        };
        // `this.key` / `this["key"]`.
        let access = match *ast.node(parent) {
            Node::Member { obj, prop, .. } if skip_parens(ast, obj) == node => key_of(ast, prop),
            Node::Index { obj, index, .. } if skip_parens(ast, obj) == node => {
                static_string_value(ast, skip_parens(ast, index)).map(|s| (false, s))
            }
            _ => None,
        };
        let member_access = access.as_ref() == Some(&key);
        let write = member_access && {
            let up = parent_skip_parens(ast, cx.file.parents, parent);
            is_assignment_target(ast, cx.file.parents, parent)
                || up.is_some_and(|u| matches!(ast.node(u), Node::Update { .. }))
        };
        let destructuring = match *ast.node(parent) {
            Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } if skip_parens(ast, init) == node => match *ast.node(name) {
                Node::ObjectPattern { props } => ast.list(props).iter().any(|&p| {
                    matches!(*ast.node(p), Node::BindingProperty { key: k, .. }
                        if ast.kind(k) != NodeKind::ComputedKey && key_of(ast, k).as_ref() == Some(&key))
                }),
                _ => false,
            },
            _ => false,
        };
        let report = match kind {
            MethodKind::Get => {
                let pure_write = write
                    && parent_skip_parens(ast, cx.file.parents, parent).is_none_or(|u| match *ast
                        .node(u)
                    {
                        Node::Update { .. } => false,
                        Node::Assign { op, .. } => op == AssignOp::Assign,
                        _ => true,
                    });
                (member_access && !pure_write) || destructuring
            }
            _ => write,
        };
        if report {
            let kind = if kind == MethodKind::Get {
                "get"
            } else {
                "set"
            };
            cx.report_node(
                parent,
                format!("Disallow recursive access to `this` within {kind}ters."),
            )
            .id("no-accessor-recursion/error");
        }
    }
}
