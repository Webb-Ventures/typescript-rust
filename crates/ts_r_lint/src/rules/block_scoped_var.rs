//! `block-scoped-var`: a `var` used outside the block it is declared in.

use super::prelude::*;
use crate::semantic::DeclKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "block-scoped-var",
        "Enforce the use of variables within the scope they are defined",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create_default::<BlockScopedVar>,
};

#[derive(Debug, Default)]
struct BlockScopedVar;

impl Rule for BlockScopedVar {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let sem = cx.semantic();
        let ast = cx.ast();
        let mut hits: Vec<(NodeId, String)> = Vec::new();
        for s in sem.symbols() {
            for d in &s.decls {
                if d.kind != DeclKind::Var
                    || !matches!(
                        cx.node(d.parent),
                        Node::VarDecl {
                            kind: VarKind::Var,
                            ..
                        }
                    )
                {
                    continue;
                }
                // The innermost block-like node around the declaration.
                let mut n = d.parent;
                let range = loop {
                    let Some(p) = cx.parent(n) else {
                        break cx.span(n);
                    };
                    if matches!(
                        cx.node(p),
                        Node::Block { .. }
                            | Node::For { .. }
                            | Node::ForIn { .. }
                            | Node::ForOf { .. }
                            | Node::Switch { .. }
                            | Node::CatchClause { .. }
                            | Node::StaticBlock { .. }
                            | Node::Program { .. }
                    ) {
                        break cx.span(p);
                    }
                    n = p;
                };
                let lc = cx
                    .file
                    .lines
                    .line_col_utf16(cx.src(), cx.span(d.name).start);
                for &r in &s.refs {
                    let id = sem.reference(r).node;
                    let rs = cx.span(id);
                    if rs.start < range.start || rs.end > range.end {
                        let name = ident_name(ast, id).unwrap_or_default();
                        hits.push((
                            id,
                            format!(
                                "'{name}' declared on line {} column {} is used outside of binding context.",
                                lc.line + 1,
                                lc.col + 1
                            ),
                        ));
                    }
                }
            }
        }
        for (id, msg) in hits {
            cx.report_node(id, msg).id("outOfScope");
        }
    }
}
