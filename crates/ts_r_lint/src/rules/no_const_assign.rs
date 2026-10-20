//! `no-const-assign`: disallow reassigning `const`, `using` and `await using` variables.

use super::prelude::*;
use crate::semantic::Symbol;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-const-assign",
        "Disallow reassigning `const`, `using`, and `await using` variables",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoConstAssign>,
};

#[derive(Debug, Default)]
struct NoConstAssign;

/// Every non-initializing write to a symbol matching `pred`, in source order:
/// `(identifier, name)`.
pub(crate) fn writes_to<'a>(
    cx: &Ctx<'_, 'a>,
    pred: impl Fn(&Symbol) -> bool,
) -> Vec<(NodeId, String)> {
    let sem = cx.semantic();
    let mut out = Vec::new();
    for sym in sem.symbols() {
        if !pred(sym) {
            continue;
        }
        for &r in &sym.refs {
            let r = sem.reference(r);
            if r.is_write() && !r.flags.contains(RefFlags::INIT) {
                out.push((r.node, cx.ast().atom(sym.name).to_owned()));
            }
        }
    }
    out.sort_by_key(|(n, _)| cx.span(*n).start);
    out
}

impl Rule for NoConstAssign {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let hits = writes_to(cx, |s| {
            s.flags
                .intersects(SymbolFlags::CONST.union(SymbolFlags::USING))
        });
        for (node, name) in hits {
            cx.report_node(node, format!("'{name}' is constant."))
                .id("const");
        }
    }
}
