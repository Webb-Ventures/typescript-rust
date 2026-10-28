//! `import/prefer-default-export`: a module whose only export is one named export
//! (`target: "single"`, the default) or that has named exports but no default (`target:
//! "any"`). A default export, `export *` or an exported type alias or interface turns the
//! rule off for the file. Each binding of an exported destructuring counts; the last named
//! export (specifier or declaration) is reported.

use super::import_util::walk;
use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/prefer-default-export",
        "Prefer a default export if module exports a single name or multiple names.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<PreferDefaultExport>,
};

#[derive(Debug)]
struct PreferDefaultExport {
    any: bool,
}

impl FromOptions for PreferDefaultExport {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(PreferDefaultExport {
            any: str_field(obj_opt(opts, 0), "target") == Some("any"),
        })
    }
}

/// How many names a declared pattern counts for (holes, defaults and rests count one).
fn pattern_count(ast: &Ast, id: NodeId, depth: u32) -> usize {
    if depth > 200 {
        return 1;
    }
    match *ast.node(id) {
        Node::ObjectPattern { props } => ast
            .list(props)
            .iter()
            .map(|&p| match *ast.node(p) {
                Node::BindingProperty {
                    value: Some(v),
                    init: None,
                    ..
                } => pattern_count(ast, v, depth + 1),
                _ => 1,
            })
            .sum(),
        Node::ArrayPattern { elements } => ast
            .list(elements)
            .iter()
            .map(|&e| pattern_count(ast, e, depth + 1))
            .sum(),
        _ => 1,
    }
}

#[derive(Default)]
struct State {
    count: usize,
    has_default: bool,
    has_star: bool,
    has_type: bool,
    last: Option<NodeId>,
}

fn visit(ast: &Ast, node: NodeId, st: &mut State) {
    match *ast.node(node) {
        Node::ExportSpecifier {
            local, exported, ..
        } => {
            let name = exported.unwrap_or(local);
            let name = match *ast.node(name) {
                Node::StringLit { value } => ast.atom(value),
                _ => ast.name_of(name).unwrap_or(""),
            };
            if name == "default" {
                st.has_default = true;
            } else {
                st.count += 1;
                st.last = Some(node);
            }
        }
        Node::ExportDefaultExpr { .. } => st.has_default = true,
        Node::ExportAll { .. } => st.has_star = true,
        _ => {}
    }
    let n = ast.node(node);
    if !n.is_statement() || !n.modifiers().contains(Modifiers::EXPORT) {
        return;
    }
    if n.modifiers().contains(Modifiers::DEFAULT) {
        st.has_default = true;
        return;
    }
    match *n {
        Node::TypeAlias { .. } | Node::Interface { .. } => {
            st.count += 1;
            st.has_type = true;
            return;
        }
        Node::VarDecl { decls, .. } => {
            for &d in ast.list(decls) {
                if let Node::VarDeclarator { name, .. } = *ast.node(d) {
                    st.count += pattern_count(ast, name, 0);
                }
            }
        }
        Node::ImportEquals { .. } => return,
        _ => st.count += 1,
    }
    st.last = Some(node);
}

impl Rule for PreferDefaultExport {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let mut st = State::default();
        walk(ast, |n| visit(ast, n, &mut st));
        if st.has_default || st.has_star || st.has_type {
            return;
        }
        let Some(last) = st.last else { return };
        if !self.any && st.count == 1 {
            cx.report_node(last, "Prefer default export on a file with single export.");
        } else if self.any && st.count > 0 {
            cx.report_node(
                last,
                "Prefer default export to be present on every file that has export.",
            );
        }
    }
}
