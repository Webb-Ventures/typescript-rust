//! `no-class-assign`: disallow reassigning class members.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-class-assign",
        "Disallow reassigning class members",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoClassAssign>,
};

#[derive(Debug, Default)]
struct NoClassAssign;

impl Rule for NoClassAssign {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let hits = super::no_const_assign::writes_to(cx, |s| {
            s.decls
                .iter()
                .any(|d| matches!(d.kind, DeclKind::Class | DeclKind::ClassExprName))
        });
        for (node, name) in hits {
            cx.report_node(node, format!("'{name}' is a class."))
                .id("class");
        }
    }
}
