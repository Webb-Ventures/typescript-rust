//! `no-alert`: disallow `alert`, `confirm` and `prompt`.

use super::prelude::*;
use super::this_binding::top_level_this_is_global;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-alert",
        "Disallow the use of `alert`, `confirm`, and `prompt`",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create_default::<NoAlert>,
};

#[derive(Debug, Default)]
struct NoAlert;

fn prohibited(name: &str) -> bool {
    matches!(name, "alert" | "confirm" | "prompt")
}

/// Is `obj` the global object: an unshadowed `window` or `globalThis`, or `this` at the
/// top level of a script?
fn is_global_object(cx: &Ctx<'_, '_>, call: NodeId, obj: NodeId) -> bool {
    let ast = cx.ast();
    let obj = skip_parens(ast, obj);
    match ident_name(ast, obj) {
        Some("window") => !cx.is_shadowed(obj, "window"),
        Some("globalThis") => cx.global_available(obj, "globalThis"),
        Some(_) => false,
        None => {
            matches!(ast.node(obj), Node::This {}) && top_level_this_is_global(cx) && {
                let sem = cx.semantic();
                sem.scope(sem.enclosing_scope(call)).kind == ScopeKind::Global
            }
        }
    }
}

impl Rule for NoAlert {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Call { callee, .. } = *cx.node(node) else {
            return;
        };
        let ast = cx.ast();
        let callee = skip_parens(ast, callee);
        let name = match *ast.node(callee) {
            Node::Ident { .. } => {
                let Some(n) = ident_name(ast, callee) else {
                    return;
                };
                if !prohibited(n) || cx.is_shadowed(callee, n) {
                    return;
                }
                n.to_owned()
            }
            Node::Member { obj, .. } | Node::Index { obj, .. } => {
                let Some(n) = member_name(ast, cx.src(), callee) else {
                    return;
                };
                if !prohibited(&n) || !is_global_object(cx, node, obj) {
                    return;
                }
                n.into_owned()
            }
            _ => return,
        };
        cx.report(cx.span(node), format!("Unexpected {name}."))
            .id("unexpected");
    }
}
