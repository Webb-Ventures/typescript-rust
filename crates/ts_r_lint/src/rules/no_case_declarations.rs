//! `no-case-declarations`: disallow lexical declarations in case clauses.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-case-declarations",
        "Disallow lexical declarations in case clauses",
        ESLINT_REC,
        Category::Correctness,
    )
    .suggestions(),
    create: create_default::<NoCaseDeclarations>,
};

#[derive(Debug, Default)]
struct NoCaseDeclarations;

impl Rule for NoCaseDeclarations {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::SwitchCase]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::SwitchCase { body, .. } = *cx.node(node) else {
            return;
        };
        let stmts = ast.list(body);
        for &s in stmts {
            let lexical = match *ast.node(s) {
                Node::VarDecl {
                    kind, modifiers, ..
                } => kind != VarKind::Var && !modifiers.contains(Modifiers::DECLARE),
                Node::FunctionDecl { body, .. } => body.is_some(),
                Node::ClassDecl { modifiers, .. } => !modifiers.contains(Modifiers::DECLARE),
                _ => false,
            };
            if !lexical {
                continue;
            }
            let first = ast.span(stmts[0]).start;
            let last = ast.span(stmts[stmts.len() - 1]).end;
            let fix = Fix::insert(first, "{ ").and(Span::at(last), " }");
            cx.report_node(s, "Unexpected lexical declaration in case block.")
                .id("unexpected")
                .suggest("addBrackets", "Add {} brackets around the case block.", fix);
        }
    }
}
