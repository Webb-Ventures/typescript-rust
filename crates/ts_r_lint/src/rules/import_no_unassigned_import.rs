//! `import/no-unassigned-import`: side-effect imports (`import "x"`, and `require("x")` as
//! a statement), except module paths matching an `allow` glob. A relative or absolute
//! specifier is resolved against the file's directory before matching, and each glob is
//! tried as written and joined to the cwd.

use super::prelude::*;
use crate::config::glob::Glob;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-unassigned-import",
        "Forbid unassigned imports.",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create::<NoUnassignedImport>,
};

#[derive(Debug)]
struct NoUnassignedImport {
    allow: Vec<Glob>,
    /// The `allow` patterns as written (also matched joined to the cwd).
    patterns: Vec<String>,
}

impl FromOptions for NoUnassignedImport {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let patterns = str_list(obj_opt(opts, 0), "allow");
        Ok(NoUnassignedImport {
            allow: patterns.iter().map(|g| Glob::new(g)).collect(),
            patterns,
        })
    }
}

/// `rel` joined to `dir` with `.` and `..` segments folded.
fn resolve_path(dir: &str, rel: &str) -> String {
    let base = if rel.starts_with('/') { "" } else { dir };
    let mut parts: Vec<&str> = Vec::new();
    for seg in base.split('/').chain(rel.split('/')) {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    format!("/{}", parts.join("/"))
}

impl NoUnassignedImport {
    fn allowed(&self, cx: &Ctx<'_, '_>, source: &str) -> bool {
        if self.allow.is_empty() {
            return false;
        }
        let path = if source.starts_with('.') || source.starts_with('/') {
            let file = cx.file.path.replace('\\', "/");
            let dir = file.rsplit_once('/').map_or("", |(d, _)| d);
            resolve_path(dir, source)
        } else {
            source.to_owned()
        };
        if self.allow.iter().any(|g| g.matches(&path)) {
            return true;
        }
        // Upstream also matches each pattern joined to the cwd.
        let Some(cwd) = cx.env().cwd.as_deref() else {
            return false;
        };
        let cwd = cwd.replace('\\', "/");
        self.patterns
            .iter()
            .any(|g| Glob::new(&resolve_path(&cwd, g)).matches(&path))
    }
}

impl Rule for NoUnassignedImport {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ImportDecl, NodeKind::ExprStmt]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::ImportDecl {
                specifiers, source, ..
            } => {
                let value = super::import_util::string_literal(ast, source).unwrap_or("");
                if ast.list(specifiers).is_empty() && !self.allowed(cx, value) {
                    cx.report_node(node, "Imported module should be assigned");
                }
            }
            Node::ExprStmt { expr } => {
                let call = skip_parens(ast, expr);
                let Node::Call { callee, args, .. } = *ast.node(call) else {
                    return;
                };
                let &[arg] = ast.list(args) else { return };
                let Node::StringLit { value } = *ast.node(arg) else {
                    return;
                };
                if ident_name(ast, callee) == Some("require") && !self.allowed(cx, ast.atom(value))
                {
                    cx.report_node(call, "Imported module should be assigned");
                }
            }
            _ => {}
        }
    }
}
