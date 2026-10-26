//! `no-useless-rename`: disallow renaming imports, exports and destructured bindings to the
//! same name.

use super::prelude::*;
use ts_r_ast::{Ast, Span};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-useless-rename",
        "Disallow renaming import, export, and destructured assignments to the same name",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<NoUselessRename>,
};

#[derive(Debug, Default)]
struct NoUselessRename {
    ignore_destructuring: bool,
    ignore_import: bool,
    ignore_export: bool,
}

impl FromOptions for NoUselessRename {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let field = |k| bool_field(o, k, false);
        Ok(NoUselessRename {
            ignore_destructuring: field("ignoreDestructuring"),
            ignore_import: field("ignoreImport"),
            ignore_export: field("ignoreExport"),
        })
    }
}

impl Rule for NoUselessRename {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::BindingProperty,
            NodeKind::Property,
            NodeKind::ImportSpecifier,
            NodeKind::ExportSpecifier,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::BindingProperty {
                key,
                value: Some(value),
                init,
            } => {
                if self.ignore_destructuring {
                    return;
                }
                let Some(key_name) = key_text(ast, key) else {
                    return;
                };
                if bound_name(ast, value) != Some(key_name) {
                    return;
                }
                let end = init.map_or(ast.span(value).end, |i| ast.span(i).end);
                let replacement = Span::new(ast.span(value).start, end);
                self.report(
                    cx,
                    node,
                    "Destructuring assignment",
                    key_name,
                    Some(replacement),
                );
            }
            Node::Property { key, value, .. } => {
                if self.ignore_destructuring {
                    return;
                }
                let Some(obj) = cx.parent(node) else { return };
                if !matches!(cx.node(obj), Node::ObjectLit { .. })
                    || !is_assignment_target(ast, cx.file.parents, obj)
                {
                    return;
                }
                let Some(key_name) = key_text(ast, key) else {
                    return;
                };
                let (target, replacement) = match *ast.node(value) {
                    Node::Assign {
                        left,
                        op: AssignOp::Assign,
                        ..
                    } => {
                        let parenthesised = matches!(ast.node(left), Node::Paren { .. });
                        (left, (!parenthesised).then(|| ast.span(value)))
                    }
                    _ => (value, Some(ast.span(skip_parens(ast, value)))),
                };
                if bound_name(ast, skip_parens(ast, target)) != Some(key_name) {
                    return;
                }
                self.report(cx, node, "Destructuring assignment", key_name, replacement);
            }
            Node::ImportSpecifier {
                imported: Some(imported),
                local,
                ..
            } => {
                if self.ignore_import {
                    return;
                }
                let Some(name) = module_export_name(ast, imported) else {
                    return;
                };
                if bound_name(ast, local) != Some(name) {
                    return;
                }
                self.report(cx, node, "Import", name, Some(ast.span(local)));
            }
            Node::ExportSpecifier {
                local,
                exported: Some(exported),
                ..
            } => {
                if self.ignore_export {
                    return;
                }
                let Some(name) = module_export_name(ast, local) else {
                    return;
                };
                if module_export_name(ast, exported) != Some(name) {
                    return;
                }
                self.report(cx, node, "Export", name, Some(ast.span(local)));
            }
            _ => {}
        }
    }
}

impl NoUselessRename {
    fn report(
        &self,
        cx: &mut Ctx<'_, '_>,
        node: NodeId,
        what: &str,
        name: &str,
        replacement: Option<Span>,
    ) {
        let ast = cx.ast();
        let span = cx.span(node);
        // Removing the key would drop comments that sit outside the kept part.
        let fix = replacement
            .filter(|r| ast.comments_in(span).len() <= ast.comments_in(*r).len())
            .map(|r| Fix::replace(span, r.text(cx.src()).to_owned()));
        cx.report(span, format!("{what} {name} unnecessarily renamed."))
            .id("unnecessarilyRenamed")
            .fix_opt(fix);
    }
}

/// An identifier or string-literal key's name.
fn key_text(ast: &Ast, key: NodeId) -> Option<&str> {
    match *ast.node(key) {
        Node::Ident { name } => Some(ast.atom(name)),
        Node::StringLit { value } => Some(ast.atom(value)),
        _ => None,
    }
}

fn bound_name(ast: &Ast, id: NodeId) -> Option<&str> {
    match *ast.node(id) {
        Node::Ident { name } => Some(ast.atom(name)),
        _ => None,
    }
}

fn module_export_name(ast: &Ast, id: NodeId) -> Option<&str> {
    key_text(ast, id)
}
