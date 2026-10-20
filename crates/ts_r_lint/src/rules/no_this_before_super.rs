//! `no-this-before-super`: disallow `this`/`super` before calling `super()` in
//! constructors.

use super::constructor_super::{Heritage, class_heritage, is_super_call};
use super::prelude::*;
use crate::cfg::Cfg;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-this-before-super",
        "Disallow `this`/`super` before calling `super()` in constructors",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoThisBeforeSuper>,
};

#[derive(Debug, Default)]
struct NoThisBeforeSuper;

const NOT_CALLED: u8 = 1;
const CALLED: u8 = 2;

impl Rule for NoThisBeforeSuper {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::MethodDef]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::MethodDef {
            kind: MethodKind::Constructor,
            value,
            ..
        } = *ast.node(node)
        else {
            return;
        };
        if !matches!(ast.node(value), Node::FunctionExpr { body: Some(_), .. })
            || class_heritage(cx, node) != Heritage::Constructor
        {
            return;
        }
        let cfg = Cfg::function(ast, value);
        let states = cfg.forward(
            NOT_CALLED,
            |a, b| a | b,
            |s, e| if is_super_call(ast, e) { CALLED } else { *s },
        );
        let mut found: Vec<NodeId> = Vec::new();
        for (i, b) in cfg.blocks.iter().enumerate() {
            let Some(mut s) = states[i] else { continue };
            for &e in &b.events {
                if is_super_call(ast, e) {
                    s = CALLED;
                    continue;
                }
                let hit = match ast.node(e) {
                    Node::This {} => true,
                    Node::Super {} => !cx.parent(e).is_some_and(
                        |p| matches!(*ast.node(p), Node::Call { callee, .. } if callee == e),
                    ),
                    _ => false,
                };
                if hit && s & NOT_CALLED != 0 && !found.contains(&e) {
                    found.push(e);
                }
            }
        }
        found.sort_by_key(|&n| cx.span(n).start);
        for n in found {
            let what = if matches!(ast.node(n), Node::This {}) {
                "this"
            } else {
                "super"
            };
            cx.report_node(n, format!("'{what}' is not allowed before 'super()'."))
                .id("noBeforeSuper");
        }
    }
}
