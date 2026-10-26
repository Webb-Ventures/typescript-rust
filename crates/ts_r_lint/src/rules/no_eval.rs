//! `no-eval`: disallow `eval()`, including indirect calls through the global object and
//! sloppy-mode `this`.

use super::prelude::*;
use super::this_binding::{
    ThisScope, is_default_this_binding, is_strict, this_scope, top_level_this_is_global,
};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-eval",
        "Disallow the use of `eval()`",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create::<NoEval>,
};

#[derive(Debug)]
struct NoEval {
    allow_indirect: bool,
}

impl FromOptions for NoEval {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoEval {
            allow_indirect: bool_field(obj_opt(opts, 0), "allowIndirect", false),
        })
    }
}

const MSG: &str = "`eval` can be harmful.";

/// The property node of a `x.eval` / `x["eval"]` member whose object is `obj`.
fn eval_member_prop(cx: &Ctx<'_, '_>, obj: NodeId, name: &str) -> Option<(NodeId, NodeId)> {
    let ast = cx.ast();
    let m = parent_skip_parens(ast, cx.file.parents, obj)?;
    let (o, prop) = match *ast.node(m) {
        Node::Member { obj, prop, .. } => (obj, prop),
        Node::Index { obj, index, .. } => (obj, index),
        _ => return None,
    };
    if skip_parens(ast, o) != skip_parens(ast, obj) {
        return None;
    }
    (member_name(ast, cx.src(), m).as_deref() == Some(name)).then_some((m, prop))
}

impl Rule for NoEval {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::Ident, NodeKind::This]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::Call {
                callee, optional, ..
            } => {
                let callee = skip_parens(ast, callee);
                if ident_name(ast, callee) == Some("eval") && !(self.allow_indirect && optional) {
                    cx.report(cx.span(callee), MSG).id("unexpected");
                }
            }
            _ if self.allow_indirect => {}
            Node::Ident { .. } => {
                let Some(name) = ident_name(ast, node) else {
                    return;
                };
                match name {
                    "eval" => {
                        let is_callee =
                            parent_skip_parens(ast, cx.file.parents, node).is_some_and(|p| {
                                matches!(*ast.node(p), Node::Call { callee, .. }
                                    if skip_parens(ast, callee) == node)
                            });
                        if !is_callee && cx.is_global_ref(node, "eval") {
                            cx.report(cx.span(node), MSG).id("unexpected");
                        }
                    }
                    "window" | "global" | "globalThis" => {
                        if !cx.is_global_ref(node, name) {
                            return;
                        }
                        let mut cur = node;
                        // Each step moves to a parent member.
                        while let Some((m, _)) = eval_member_prop(cx, cur, name) {
                            cur = m;
                        }
                        if let Some((_, prop)) = eval_member_prop(cx, cur, "eval") {
                            cx.report(cx.span(prop), MSG).id("unexpected");
                        }
                    }
                    _ => {}
                }
            }
            Node::This {} => {
                let Some((_, prop)) = eval_member_prop(cx, node, "eval") else {
                    return;
                };
                let global = match this_scope(cx, node) {
                    ThisScope::Program => {
                        let root = cx.file.parents.ancestors(node).last().unwrap_or(node);
                        top_level_this_is_global(cx) || !is_strict(cx, root)
                    }
                    ThisScope::Function(f) => !is_strict(cx, f) && is_default_this_binding(cx, f),
                    ThisScope::Class => false,
                };
                if global {
                    cx.report(cx.span(prop), MSG).id("unexpected");
                }
            }
            _ => {}
        }
    }
}
