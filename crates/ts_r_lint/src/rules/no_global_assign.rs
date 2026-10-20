//! `no-global-assign`: disallow assignments to native objects or read-only global variables.

use super::prelude::*;
use crate::env::GlobalAccess;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-global-assign",
        "Disallow assignments to native objects or read-only global variables",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create::<NoGlobalAssign>,
};

#[derive(Debug, Default)]
struct NoGlobalAssign {
    exceptions: Vec<String>,
}

impl FromOptions for NoGlobalAssign {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoGlobalAssign {
            exceptions: str_list(obj_opt(opts, 0), "exceptions"),
        })
    }
}

impl Rule for NoGlobalAssign {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let sem = cx.semantic();
        let mut hits = Vec::new();
        for r in sem.references() {
            if !r.is_write() || r.flags.contains(RefFlags::INIT) {
                continue;
            }
            let name = cx.ast().atom(r.name);
            let readonly = match r.resolution {
                Resolution::Global => cx.env().globals.get(name) == Some(GlobalAccess::Readonly),
                Resolution::Symbol(s) => {
                    let sym = sem.symbol(s);
                    sym.flags.contains(SymbolFlags::COMMENT_GLOBAL)
                        && cx
                            .file
                            .comment_globals
                            .iter()
                            .rev()
                            .find(|g| g.name == name && !g.off)
                            .is_some_and(|g| !g.writable)
                }
                Resolution::Unresolved => false,
            };
            if readonly && !self.exceptions.iter().any(|e| e == name) {
                hits.push((r.node, name));
            }
        }
        for (node, name) in hits {
            cx.report_node(
                node,
                format!("Read-only global '{name}' should not be modified."),
            )
            .id("globalShouldNotBeModified");
        }
    }
}
