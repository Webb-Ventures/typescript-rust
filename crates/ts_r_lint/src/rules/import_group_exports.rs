//! `import/group-exports`: more than one named export declaration in a file, more than
//! one `export ... from` for the same module, or more than one CommonJS export
//! assignment. Every declaration of an offending group is reported.
//!
//! - ES modules: `ExportNamedDeclaration`s without a source form one group (type-only ones,
//!   `exportKind: "type"`, another); those with a source are grouped by its value.
//!   Declarations nested in TypeScript namespaces count with the file's.
//! - CommonJS: assignments (any operator, anywhere) whose target's member chain is
//!   `module.exports`, `module.exports.x` or `exports.x`. The chain is the property names
//!   from the innermost identifier out (a computed key counts by its identifier's name, a
//!   literal key as nothing), so deeper assignments (`module.exports.a.b = ...`) are
//!   ignored.

use super::import_util::{ExportDecl, export_decl, string_literal, walk};
use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/group-exports",
        "Prefer named exports to be grouped together in a single export declaration",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<GroupExports>,
};

#[derive(Debug, Default)]
struct GroupExports;

const NAMED: &str = "Multiple named export declarations; consolidate all named exports into a single export declaration";
const COMMONJS: &str = "Multiple CommonJS exports; consolidate all exports into a single assignment to `module.exports`";

/// The property names of a member chain, outermost last (`module.exports.x` gives
/// `[module, exports, x]`); `None` for a key without a name.
fn accessor_chain(ast: &Ast, mut node: NodeId) -> Vec<Option<&str>> {
    let mut rev = Vec::new();
    loop {
        let (obj, prop) = match *ast.node(node) {
            Node::Member { obj, prop, .. } => (obj, ident_name(ast, prop)),
            Node::Index { obj, index, .. } => (obj, ident_name(ast, skip_parens(ast, index))),
            _ => break,
        };
        rev.push(prop);
        let obj = skip_parens(ast, obj);
        if let Some(name) = ident_name(ast, obj) {
            rev.push(Some(name));
            break;
        }
        node = obj;
    }
    rev.reverse();
    rev
}

fn is_commonjs_export(ast: &Ast, left: NodeId) -> bool {
    let left = skip_parens(ast, left);
    if !matches!(ast.node(left), Node::Member { .. } | Node::Index { .. }) {
        return false;
    }
    let chain = accessor_chain(ast, left);
    match chain.as_slice() {
        [Some("module"), Some("exports"), ..] => chain.len() <= 3,
        [Some("exports"), _] => true,
        _ => false,
    }
}

impl Rule for GroupExports {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let mut values = Vec::new();
        let mut types = Vec::new();
        // (type-only, source) -> declarations, in first-seen order.
        let mut sourced: Vec<Vec<NodeId>> = Vec::new();
        let mut by_source: rustc_hash::FxHashMap<(bool, &str), usize> = Default::default();
        let mut commonjs = Vec::new();
        walk(ast, |id| match *ast.node(id) {
            Node::Assign { left, .. } => {
                if is_commonjs_export(ast, left) {
                    commonjs.push(id);
                }
            }
            _ => {
                let Some(ExportDecl::Named { type_kind }) = export_decl(ast, id) else {
                    return;
                };
                let source = match *ast.node(id) {
                    Node::ExportNamed {
                        source: Some(s), ..
                    } => Some(string_literal(ast, s).unwrap_or("")),
                    _ => None,
                };
                match source {
                    None if type_kind => types.push(id),
                    None => values.push(id),
                    Some(src) => {
                        let i = *by_source.entry((type_kind, src)).or_insert_with(|| {
                            sourced.push(Vec::new());
                            sourced.len() - 1
                        });
                        sourced[i].push(id);
                    }
                }
            }
        });
        let mut report = |list: &[NodeId], message: &str| {
            if list.len() > 1 {
                for &n in list {
                    cx.report_node(n, message);
                }
            }
        };
        report(&values, NAMED);
        report(&types, NAMED);
        for list in &sourced {
            report(list, NAMED);
        }
        report(&commonjs, COMMONJS);
    }
}
