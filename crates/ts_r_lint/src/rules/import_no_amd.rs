//! `import/no-amd`: AMD `require([...], f)` / `define([...], f)` calls at module scope
//! (the call's innermost scope is the module scope, so only ES module files report).

use super::import_util::scope_kind;
use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-amd",
        "Forbid AMD `require` and `define` calls.",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create_default::<NoAmd>,
};

#[derive(Debug, Default)]
struct NoAmd;

impl Rule for NoAmd {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call { callee, args, .. } = *cx.node(node) else {
            return;
        };
        let Some(name @ ("require" | "define")) = ident_name(ast, skip_parens(ast, callee)) else {
            return;
        };
        let &[deps, _] = ast.list(args) else { return };
        if ast.kind(skip_parens(ast, deps)) != NodeKind::ArrayLit
            || scope_kind(cx, node) != ScopeKind::Module
        {
            return;
        }
        cx.report_node(node, format!("Expected imports instead of AMD {name}()."));
    }
}
