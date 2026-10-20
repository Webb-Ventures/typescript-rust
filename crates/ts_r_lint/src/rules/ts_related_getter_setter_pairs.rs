//! `@typescript-eslint/related-getter-setter-pairs`: a getter's type is assignable to its
//! setter's parameter type.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/related-getter-setter-pairs",
        "Enforce that `get()` types should be assignable to their equivalent `set()` type",
        TS_STRICT_TC,
        Category::Suspicious,
    )
    .typed(),
    create: create_default::<RelatedGetterSetterPairs>,
};

#[derive(Debug, Default)]
struct RelatedGetterSetterPairs;

struct Accessor {
    name: String,
    is_static: bool,
    /// Getter: the return type node. Setter: the parameter's type node.
    ty: NodeId,
}

impl Rule for RelatedGetterSetterPairs {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::ClassDecl,
            NodeKind::ClassExpr,
            NodeKind::Interface,
            NodeKind::TypeLiteral,
        ]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let members = match *cx.node(node) {
            Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => members,
            Node::Interface { body, .. } => body,
            Node::TypeLiteral { members } => members,
            _ => return,
        };
        let ast = cx.ast();
        let src = cx.src();
        let mut getters = Vec::new();
        let mut setters = Vec::new();
        for &m in ast.list(members) {
            let (kind, key, mods, params, return_type) = match *cx.node(m) {
                Node::MethodDef {
                    kind,
                    key,
                    modifiers,
                    value,
                    ..
                } => match *cx.node(value) {
                    Node::FunctionExpr {
                        params,
                        return_type,
                        ..
                    } => (kind, key, modifiers, params, return_type),
                    _ => continue,
                },
                Node::MethodSignature {
                    kind,
                    key,
                    modifiers,
                    params,
                    return_type,
                    ..
                } => (kind, key, modifiers, params, return_type),
                _ => continue,
            };
            let Some(name) = static_key_name(ast, src, key) else {
                continue;
            };
            let is_static = mods.contains(Modifiers::STATIC);
            match kind {
                MethodKind::Get => {
                    if let Some(rt) = return_type {
                        getters.push(Accessor {
                            name: name.into_owned(),
                            is_static,
                            ty: rt,
                        });
                    }
                }
                MethodKind::Set => {
                    let [p] = ast.list(params)[..] else {
                        continue;
                    };
                    if let Node::Param {
                        type_ann: Some(t), ..
                    } = *cx.node(p)
                    {
                        setters.push(Accessor {
                            name: name.into_owned(),
                            is_static,
                            ty: t,
                        });
                    }
                }
                _ => {}
            }
        }
        let ti = cx.types();
        for g in &getters {
            let Some(s) = setters
                .iter()
                .find(|s| s.name == g.name && s.is_static == g.is_static)
            else {
                continue;
            };
            let (Some(gt), Some(st)) = (ti.type_of_type_node(g.ty), ti.type_of_type_node(s.ty))
            else {
                continue;
            };
            if has_unresolved_part(ti, gt) || has_unresolved_part(ti, st) {
                continue;
            }
            if !ti.is_assignable(gt, st) {
                cx.report_node(
                    g.ty,
                    "`get()` type should be assignable to its equivalent `set()` type.",
                )
                .id("mismatch");
            }
        }
    }
}
