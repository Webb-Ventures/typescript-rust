//! `no-func-assign`: disallow reassigning `function` declarations.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-func-assign",
        "Disallow reassigning `function` declarations",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoFuncAssign>,
};

#[derive(Debug, Default)]
struct NoFuncAssign;

impl Rule for NoFuncAssign {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let hits = super::no_const_assign::writes_to(cx, |s| {
            matches!(
                s.decls.first().map(|d| d.kind),
                Some(DeclKind::Function | DeclKind::FunctionExprName)
            )
        });
        for (node, name) in hits {
            cx.report_node(node, format!("'{name}' is a function."))
                .id("isAFunction");
        }
    }
}
