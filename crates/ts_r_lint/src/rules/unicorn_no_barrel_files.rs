//! `unicorn/no-barrel-files`: a module whose top level is only re-exports, reported on the
//! whole program. Allowed statements are imports with bindings (a bare `import "x"` makes it
//! not a barrel), `export * from`, `export {...} from`, `export {...}` of imported bindings
//! only, and `export default` of an imported binding (through TS wrappers). At least one
//! re-export is needed (`export {}` alone does not count).

use super::prelude::*;
use super::unicorn_conditions::unwrap_ts_and_parens;
use rustc_hash::FxHashSet;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-barrel-files",
        "Disallow barrel files.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoBarrelFiles>,
};

#[derive(Debug, Default)]
struct NoBarrelFiles;

fn is_barrel(cx: &Ctx<'_, '_>, stmts: &[NodeId]) -> bool {
    let ast = cx.ast();
    let mut imported: FxHashSet<&str> = FxHashSet::default();
    for &s in stmts {
        if let Node::ImportDecl { specifiers, .. } = *ast.node(s) {
            for &spec in ast.list(specifiers) {
                let (Node::ImportDefaultSpecifier { local }
                | Node::ImportNamespaceSpecifier { local }
                | Node::ImportSpecifier { local, .. }) = *ast.node(spec)
                else {
                    continue;
                };
                imported.extend(ident_name(ast, local));
            }
        }
    }
    let mut re_export = false;
    for &s in stmts {
        match *ast.node(s) {
            Node::ImportDecl { specifiers, .. } => {
                if ast.list(specifiers).is_empty() {
                    return false;
                }
            }
            Node::ExportAll { .. } => re_export = true,
            Node::ExportNamed {
                specifiers, source, ..
            } => {
                let specs = ast.list(specifiers);
                if source.is_none()
                    && specs.iter().any(|&sp| match *ast.node(sp) {
                        Node::ExportSpecifier { local, .. } => {
                            !ident_name(ast, local).is_some_and(|n| imported.contains(n))
                        }
                        _ => true,
                    })
                {
                    return false;
                }
                re_export |= !specs.is_empty();
            }
            Node::ExportDefaultExpr { expr } => {
                let e = unwrap_ts_and_parens(ast, expr);
                if !(matches!(ast.node(e), Node::Ident { .. })
                    && ident_name(ast, e).is_some_and(|n| imported.contains(n)))
                {
                    return false;
                }
                re_export = true;
            }
            _ => return false,
        }
    }
    re_export
}

impl Rule for NoBarrelFiles {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Program]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Program { stmts } = *cx.node(node) else {
            return;
        };
        if is_barrel(cx, cx.ast().list(stmts)) {
            cx.report_node(node, "Barrel files are not allowed.")
                .id("no-barrel-files");
        }
    }
}
