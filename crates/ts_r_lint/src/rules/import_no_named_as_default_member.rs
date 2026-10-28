//! `import/no-named-as-default-member`: reading a property of a default import
//! (`foo.bar`, `const { bar } = foo`) that the module also exports by name (declared in
//! the module itself, not re-exported).

use super::import_util::{exports_of, program_statements, report_parse_errors, string_literal};
use super::prelude::*;
use rustc_hash::FxHashMap;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-named-as-default-member",
        "Forbid use of exported name as property of default export.",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create_default::<NoNamedAsDefaultMember>,
};

#[derive(Debug, Default)]
struct NoNamedAsDefaultMember;

impl Rule for NoNamedAsDefaultMember {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        // Default imports: local name -> (map, specifier). Upstream collects them from any
        // import declaration as it walks; imports are top-level.
        let mut imports = FxHashMap::default();
        for &s in program_statements(ast) {
            let Node::ImportDecl {
                specifiers, source, ..
            } = *ast.node(s)
            else {
                continue;
            };
            for &sp in ast.list(specifiers) {
                let Node::ImportDefaultSpecifier { local } = *ast.node(sp) else {
                    continue;
                };
                let (Some(name), Some(spec)) =
                    (ident_name(ast, local), string_literal(ast, source))
                else {
                    continue;
                };
                let Some(m) = exports_of(cx, spec) else {
                    continue;
                };
                if !m.errors.is_empty() {
                    report_parse_errors(cx, &m, source, spec);
                    continue;
                }
                imports.insert(name.to_owned(), (m, spec.to_owned()));
            }
        }
        if imports.is_empty() {
            return;
        }
        let mut lookups: Vec<(&str, &str, NodeId)> = Vec::new();
        super::import_util::walk(ast, |id| match *ast.node(id) {
            Node::Member { obj, prop, .. } => {
                if let (Some(o), Some(p)) = (ident_name(ast, obj), ident_name(ast, prop)) {
                    lookups.push((o, p, id));
                }
            }
            Node::Index { obj, index, .. } => {
                if let (Some(o), Some(p)) = (ident_name(ast, obj), ident_name(ast, index)) {
                    lookups.push((o, p, id));
                }
            }
            Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } => {
                if let (Node::ObjectPattern { props }, Some(o)) =
                    (ast.node(name), ident_name(ast, init))
                {
                    for &p in ast.list(*props) {
                        if let Node::BindingProperty { key, .. } = *ast.node(p)
                            && let Some(k) = ident_name(ast, key)
                        {
                            lookups.push((o, k, key));
                        }
                    }
                }
            }
            _ => {}
        });
        for (obj, prop, at) in lookups {
            if prop == "default" {
                continue;
            }
            let Some((m, spec)) = imports.get(obj) else {
                continue;
            };
            if !m.namespace_has(prop) {
                continue;
            }
            let span = cx.span(at);
            cx.report(
                span,
                format!(
                    "Caution: `{obj}` also has a named export `{prop}`. Check if you meant to write `import {{{prop}}} from '{spec}'` instead."
                ),
            );
        }
    }
}
