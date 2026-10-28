//! `unicorn/no-top-level-assignment-in-function`: a write (not the declaration's own
//! initialiser) to a variable declared in the file's top-level scope (global or module;
//! `/* global */` comments declare nothing) from inside a function, arrow or method is
//! reported at the written identifier. Class field initialisers and static blocks are not
//! functions.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-top-level-assignment-in-function",
        "Disallow assigning to top-level variables from inside functions.",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create_default::<NoTopLevelAssignmentInFunction>,
};

#[derive(Debug, Default)]
struct NoTopLevelAssignmentInFunction;

impl Rule for NoTopLevelAssignmentInFunction {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let sem = cx.semantic();
        let mut hits: Vec<(NodeId, String)> = Vec::new();
        for scope in sem.scopes() {
            if !matches!(scope.kind, ScopeKind::Global | ScopeKind::Module)
                || !matches!(ast.node(scope.node), Node::Program { .. })
            {
                continue;
            }
            for &s in &scope.symbols {
                let sym = sem.symbol(s);
                if sym.decls.is_empty() || sym.flags.contains(SymbolFlags::COMMENT_GLOBAL) {
                    continue;
                }
                for &r in &sym.refs {
                    let r = sem.reference(r);
                    if !r.is_write() || r.flags.contains(RefFlags::INIT) {
                        continue;
                    }
                    let mut cur = r.node;
                    let inside = loop {
                        let Some(p) = cx.parent(cur) else {
                            break false;
                        };
                        if is_function(ast.kind(p)) {
                            break true;
                        }
                        cur = p;
                    };
                    if inside {
                        hits.push((r.node, sem.name(s).to_owned()));
                    }
                }
            }
        }
        for (node, name) in hits {
            cx.report_node(
                node,
                format!("Do not assign to top-level variable `{name}` from inside a function."),
            )
            .id("no-top-level-assignment-in-function");
        }
    }
}
