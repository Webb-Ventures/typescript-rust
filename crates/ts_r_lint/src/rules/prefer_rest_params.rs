//! `prefer-rest-params`: require rest parameters instead of `arguments`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "prefer-rest-params",
        "Require rest parameters instead of `arguments`",
        TS_ESLINT_REC,
        Category::Style,
    ),
    create: create_default::<PreferRestParams>,
};

#[derive(Debug, Default)]
struct PreferRestParams;

impl Rule for PreferRestParams {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let sem = cx.semantic();
        let ast = cx.ast();
        let mut hits = Vec::new();
        for sym in sem.symbols() {
            if !sym.flags.contains(SymbolFlags::ARGUMENTS) {
                continue;
            }
            for &r in &sym.refs {
                let r = sem.reference(r);
                if r.is_type() {
                    continue;
                }
                if let Some(p) = parent_skip_parens(ast, cx.file.parents, r.node)
                    && let Node::Member { obj, prop, .. } = *ast.node(p)
                    && skip_parens(ast, obj) == r.node
                    && matches!(ident_name(ast, prop), Some("length" | "callee"))
                {
                    continue;
                }
                hits.push(r.node);
            }
        }
        hits.sort_by_key(|n| cx.span(*n).start);
        for n in hits {
            cx.report_node(n, "Use the rest parameters instead of 'arguments'.")
                .id("preferRestParams");
        }
    }
}
