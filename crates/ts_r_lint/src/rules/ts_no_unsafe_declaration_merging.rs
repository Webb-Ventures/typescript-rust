//! `@typescript-eslint/no-unsafe-declaration-merging`: disallow unsafe declaration
//! merging between classes and interfaces.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-unsafe-declaration-merging",
        "Disallow unsafe declaration merging",
        TS_REC,
        Category::Correctness,
    ),
    create: create_default::<NoUnsafeDeclarationMerging>,
};

#[derive(Debug, Default)]
struct NoUnsafeDeclarationMerging;

impl Rule for NoUnsafeDeclarationMerging {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let sem = cx.semantic();
        let mut found = Vec::new();
        for s in sem.symbols() {
            let class = s.decls.iter().any(|d| {
                d.kind == DeclKind::Class && matches!(cx.node(d.node), Node::ClassDecl { .. })
            });
            let iface = s.decls.iter().any(|d| d.kind == DeclKind::Interface);
            if !(class && iface) {
                continue;
            }
            for d in &s.decls {
                if matches!(d.kind, DeclKind::Class | DeclKind::Interface) {
                    found.push(d.name);
                }
            }
        }
        for n in found {
            cx.report_node(
                n,
                "Unsafe declaration merging between classes and interfaces.",
            )
            .id("unsafeMerging");
        }
    }
}
