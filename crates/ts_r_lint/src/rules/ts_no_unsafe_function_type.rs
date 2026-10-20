//! `@typescript-eslint/no-unsafe-function-type`: disallow the built-in `Function` type.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-unsafe-function-type",
        "Disallow using the unsafe built-in Function type",
        TS_REC,
        Category::Suspicious,
    ),
    create: create_default::<NoUnsafeFunctionType>,
};

#[derive(Debug, Default)]
struct NoUnsafeFunctionType;

impl Rule for NoUnsafeFunctionType {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::TypeRef, NodeKind::ExprWithTypeArgs]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Some((name, _)) = global_type_name(cx, node) else {
            return;
        };
        if cx.text(name) != "Function" {
            return;
        }
        cx.report_node(
            name,
            "The `Function` type accepts any function-like value.\nPrefer explicitly defining any function parameters and return type.",
        )
        .id("bannedFunctionType");
    }
}

/// The identifier naming a global type in a type reference or an `implements` /
/// interface `extends` clause, with whether it is a plain type reference.
pub(crate) fn global_type_name(cx: &Ctx<'_, '_>, node: NodeId) -> Option<(NodeId, bool)> {
    let (name, is_ref) = match *cx.node(node) {
        Node::TypeRef { name, .. } => (name, true),
        Node::ExprWithTypeArgs { expr, .. } => {
            let p = cx.parent(node)?;
            let ok = match *cx.node(p) {
                Node::HeritageClause { kind, .. } => kind == ts_r_ast::HeritageKind::Implements,
                Node::Interface { .. } => true,
                _ => false,
            };
            if !ok {
                return None;
            }
            (expr, false)
        }
        _ => return None,
    };
    if !matches!(cx.node(name), Node::Ident { .. }) {
        return None;
    }
    let sem = cx.semantic();
    if let Some(r) = sem.reference_at(name)
        && matches!(sem.reference(r).resolution, Resolution::Symbol(_))
    {
        return None;
    }
    Some((name, is_ref))
}
