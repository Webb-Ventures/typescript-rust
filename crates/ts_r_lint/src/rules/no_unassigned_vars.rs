//! `no-unassigned-vars`: disallow `let` or `var` variables that are read but never assigned.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-unassigned-vars",
        "Disallow `let` or `var` variables that are read but never assigned",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoUnassignedVars>,
};

#[derive(Debug, Default)]
struct NoUnassignedVars;

fn in_ambient(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    ancestors_inclusive(cx.file.parents, node).any(|a| match *cx.node(a) {
        Node::VarDecl { modifiers, .. } | Node::Module { modifiers, .. } => {
            modifiers.contains(Modifiers::DECLARE)
        }
        _ => false,
    })
}

impl Rule for NoUnassignedVars {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::VarDeclarator]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::VarDeclarator {
            name, init: None, ..
        } = *cx.node(node)
        else {
            return;
        };
        if !matches!(ast.node(name), Node::Ident { .. }) {
            return;
        }
        let Some(decl) = cx.parent(node) else { return };
        let Node::VarDecl { kind, .. } = *ast.node(decl) else {
            return;
        };
        if !matches!(kind, VarKind::Let | VarKind::Var) {
            return;
        }
        if let Some(p) = cx.parent(decl)
            && matches!(ast.node(p), Node::ForIn { .. } | Node::ForOf { .. })
        {
            return;
        }
        if in_ambient(cx, node) {
            return;
        }
        let sem = cx.semantic();
        let Some(sym) = sem.symbol_of_decl(name) else {
            return;
        };
        let s = sem.symbol(sym);
        if s.flags.contains(SymbolFlags::EXPORTED) {
            return;
        }
        let mut read = false;
        for &r in &s.refs {
            let r = sem.reference(r);
            if r.is_write() {
                return;
            }
            if r.is_read() {
                read = true;
            }
        }
        if !read {
            return;
        }
        let n = ast.atom(s.name).to_owned();
        cx.report_node(
            node,
            format!("'{n}' is always 'undefined' because it's never assigned."),
        )
        .id("unassigned");
    }
}
