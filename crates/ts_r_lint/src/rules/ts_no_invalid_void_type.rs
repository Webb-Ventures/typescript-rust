//! `@typescript-eslint/no-invalid-void-type`: disallow `void` outside return types and
//! generic type arguments.

use super::prelude::*;
use crate::ast_util::compact_text;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-invalid-void-type",
        "Disallow `void` type outside of generic or return types",
        TS_STRICT,
        Category::Suspicious,
    ),
    create: create::<NoInvalidVoidType>,
};

#[derive(Debug)]
enum Generics {
    All,
    None,
    Only(Vec<String>),
}

#[derive(Debug)]
struct NoInvalidVoidType {
    generics: Generics,
    this_param: bool,
}

impl FromOptions for NoInvalidVoidType {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let generics = match o.get("allowInGenericTypeArguments") {
            Some(Value::Bool(false)) => Generics::None,
            Some(Value::Array(a)) => Generics::Only(
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(compact_text)
                    .collect(),
            ),
            _ => Generics::All,
        };
        Ok(NoInvalidVoidType {
            generics,
            this_param: bool_field(o, "allowAsThisParameter", false),
        })
    }
}

fn is_void(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::KeywordType {
            kind: KeywordTypeKind::Void
        }
    )
}

impl NoInvalidVoidType {
    /// `Ok(())` when `name` may take `void` as a type argument, else the message.
    fn generic_ok(&self, name: &str) -> Result<(), Option<String>> {
        match &self.generics {
            Generics::All => Ok(()),
            Generics::None => Err(None),
            Generics::Only(list) => {
                let n = compact_text(name);
                if list.contains(&n) {
                    Ok(())
                } else {
                    Err(Some(format!("{n} may not have void as a type argument.")))
                }
            }
        }
    }

    fn general(&self) -> (&'static str, &'static str) {
        match (!matches!(self.generics, Generics::None), self.this_param) {
            (true, true) => (
                "invalidVoidNotReturnOrThisParamOrGeneric",
                "void is only valid as a return type or generic type argument or the type of a `this` parameter.",
            ),
            (true, false) => (
                "invalidVoidNotReturnOrGeneric",
                "void is only valid as a return type or generic type argument.",
            ),
            (false, true) => (
                "invalidVoidNotReturnOrThisParam",
                "void is only valid as return type or type of `this` parameter.",
            ),
            (false, false) => (
                "invalidVoidNotReturn",
                "void is only valid as a return type.",
            ),
        }
    }

    /// A union member that may sit beside `void`: `never`, `void` or an allowed generic
    /// with a `void` argument.
    fn union_member_ok(&self, cx: &Ctx<'_, '_>, id: NodeId) -> bool {
        let ast = cx.ast();
        match *ast.node(id) {
            Node::KeywordType {
                kind: KeywordTypeKind::Never | KeywordTypeKind::Void,
            } => true,
            Node::TypeRef {
                name,
                type_args: Some(ta),
            } => {
                let Node::TypeArgs { args } = *ast.node(ta) else {
                    return false;
                };
                ast.list(args).iter().any(|&a| is_void(ast, a))
                    && self.generic_ok(cx.text(name)).is_ok()
            }
            _ => false,
        }
    }
}

/// The parent of `id`, skipping parenthesized types.
fn type_parent(cx: &Ctx<'_, '_>, mut id: NodeId) -> Option<NodeId> {
    loop {
        let p = cx.parent(id)?;
        if matches!(cx.node(p), Node::ParenType { .. }) {
            id = p;
        } else {
            return Some(p);
        }
    }
}

/// `child` is the return type of signature/function `p`.
fn is_return_of(cx: &Ctx<'_, '_>, p: NodeId, child: NodeId) -> bool {
    let rt = match *cx.node(p) {
        Node::FunctionDecl { return_type, .. }
        | Node::FunctionExpr { return_type, .. }
        | Node::Arrow { return_type, .. }
        | Node::MethodSignature { return_type, .. }
        | Node::CallSignature { return_type, .. }
        | Node::ConstructSignature { return_type, .. } => return_type,
        Node::FunctionType { return_type, .. } | Node::ConstructorType { return_type, .. } => {
            Some(return_type)
        }
        _ => None,
    };
    rt.is_some_and(|r| skip_paren_type(cx, r) == child)
}

fn skip_paren_type(cx: &Ctx<'_, '_>, mut id: NodeId) -> NodeId {
    while let Node::ParenType { ty } = *cx.node(id) {
        id = ty;
    }
    id
}

/// The top of the type containing `id` (the last type node before a non-type parent),
/// with that parent.
fn type_root(cx: &Ctx<'_, '_>, mut id: NodeId) -> Option<(NodeId, NodeId)> {
    loop {
        let p = cx.parent(id)?;
        if is_type_node(cx.node(p)) {
            id = p;
        } else {
            return Some((id, p));
        }
    }
}

fn is_type_node(n: &Node) -> bool {
    matches!(
        n,
        Node::KeywordType { .. }
            | Node::TypeRef { .. }
            | Node::QualifiedName { .. }
            | Node::ArrayType { .. }
            | Node::TupleType { .. }
            | Node::NamedTupleMember { .. }
            | Node::OptionalType { .. }
            | Node::RestType { .. }
            | Node::UnionType { .. }
            | Node::IntersectionType { .. }
            | Node::TypeOperator { .. }
            | Node::IndexedAccessType { .. }
            | Node::ConditionalType { .. }
            | Node::ParenType { .. }
            | Node::TypeArgs { .. }
    )
}

/// A function or method implementation with overload signatures.
fn has_overloads(cx: &Ctx<'_, '_>, f: NodeId) -> bool {
    let ast = cx.ast();
    match *cx.node(f) {
        Node::FunctionDecl {
            modifiers,
            name,
            body: Some(_),
            ..
        } => {
            let Some(list) = cx
                .parent(f)
                .and_then(|p| crate::ast_util::statement_list(ast, p))
            else {
                return false;
            };
            let key = name.map(|n| cx.text(n));
            list.iter().any(|&s| match *ast.node(s) {
                Node::FunctionDecl {
                    modifiers: m,
                    name: n,
                    body: None,
                    ..
                } => match key {
                    Some(k) => n.is_some_and(|n| cx.text(n) == k),
                    None => n.is_none() && m.contains(Modifiers::DEFAULT),
                },
                _ => false,
            }) && (key.is_some() || modifiers.contains(Modifiers::DEFAULT))
        }
        Node::FunctionExpr { body: Some(_), .. } => {
            let Some(method) = cx.parent(f) else {
                return false;
            };
            let Node::MethodDef { key, .. } = *cx.node(method) else {
                return false;
            };
            let Some(class) = cx.parent(method) else {
                return false;
            };
            let members = match *cx.node(class) {
                Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => members,
                _ => return false,
            };
            let k = compact_text(cx.text(key));
            ast.list(members).iter().any(|&m| match *ast.node(m) {
                Node::MethodDef { key: mk, value, .. } if m != method => {
                    matches!(ast.node(value), Node::FunctionExpr { body: None, .. })
                        && compact_text(cx.text(mk)) == k
                }
                _ => false,
            })
        }
        _ => false,
    }
}

impl Rule for NoInvalidVoidType {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::KeywordType]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        if !is_void(ast, node) {
            return;
        }
        let Some(parent) = type_parent(cx, node) else {
            return;
        };
        let (gid, gmsg) = self.general();
        match *cx.node(parent) {
            Node::TypeArgs { .. } => {
                let owner = cx.parent(parent);
                let name = owner.and_then(|o| match *cx.node(o) {
                    Node::TypeRef { name, .. } => Some(name),
                    Node::New { callee, .. } => Some(callee),
                    _ => None,
                });
                if let Some(name) = name {
                    match self.generic_ok(cx.text(name)) {
                        Ok(()) => {}
                        Err(Some(msg)) => {
                            cx.report_node(node, msg).id("invalidVoidForGeneric");
                        }
                        Err(None) => {
                            cx.report_node(node, gmsg).id(gid);
                        }
                    }
                    return;
                }
            }
            Node::TypeParam {
                default: Some(d), ..
            } if !matches!(self.generics, Generics::None) && skip_paren_type(cx, d) == node => {
                return;
            }
            Node::Param {
                pattern,
                type_ann: Some(_),
                ..
            } if self.this_param && ident_name(ast, pattern) == Some("this") => return,
            Node::UnionType { types } => {
                if ast.list(types).iter().all(|&t| self.union_member_ok(cx, t)) {
                    return;
                }
                if matches!(self.generics, Generics::None) {
                    cx.report_node(node, gmsg).id(gid);
                    return;
                }
                // Overload implementations may widen to `T | void`.
                if let Some((_, owner)) = type_root(cx, parent)
                    && has_overloads(cx, owner)
                {
                    return;
                }
                cx.report_node(node, "void is not valid as a constituent in a union type")
                    .id("invalidVoidUnionConstituent");
                return;
            }
            _ => {}
        }
        if is_return_of(cx, parent, node) {
            return;
        }
        cx.report_node(node, gmsg).id(gid);
    }
}
