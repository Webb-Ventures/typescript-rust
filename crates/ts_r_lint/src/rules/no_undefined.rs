//! `no-undefined`: every use of the name `undefined` (references and declarations).

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-undefined",
        "Disallow the use of `undefined` as an identifier",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoUndefined>,
};

#[derive(Debug, Default)]
struct NoUndefined;

impl Rule for NoUndefined {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let sem = cx.semantic();
        let mut hits: Vec<NodeId> = sem
            .references()
            .iter()
            .filter(|r| ast.atom(r.name) == "undefined" && !r.flags.contains(RefFlags::INIT))
            .map(|r| r.node)
            .collect();
        for s in sem.symbols() {
            if ast.atom(s.name) == "undefined" && !s.flags.contains(SymbolFlags::COMMENT_GLOBAL) {
                hits.extend(s.decls.iter().map(|d| d.name));
            }
        }
        hits.sort_by_key(|&n| cx.span(n).start);
        hits.dedup();
        for n in hits {
            cx.report_node(n, "Unexpected use of undefined.")
                .id("unexpectedUndefined");
        }
    }
}
