//! `no-ex-assign`: disallow reassigning exceptions in `catch` clauses.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-ex-assign",
        "Disallow reassigning exceptions in `catch` clauses",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoExAssign>,
};

#[derive(Debug, Default)]
struct NoExAssign;

impl Rule for NoExAssign {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let hits = super::no_const_assign::writes_to(cx, |s| {
            s.decls.first().map(|d| d.kind) == Some(DeclKind::CatchParam)
        });
        for (node, _) in hits {
            cx.report_node(node, "Do not assign to the exception parameter.")
                .id("unexpected");
        }
    }
}
