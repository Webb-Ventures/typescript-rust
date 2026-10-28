//! `unicorn/no-useless-re-export`: when every unnamed `export * from` in the file names the
//! same module request (source plus import attributes, as written), a non-default
//! `export { name } from` that same request (unaliased) repeats it. A type-only `export *`
//! covers only type exports. Reported per specifier.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-useless-re-export",
        "Disallow redundant re-exports.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoUselessReExport>,
};

#[derive(Debug, Default)]
struct NoUselessReExport;

fn name(cx: &Ctx<'_, '_>, id: NodeId) -> Option<String> {
    let ast = cx.ast();
    match *ast.node(id) {
        Node::Ident { name } => Some(ast.atom(name).to_owned()),
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        _ => None,
    }
}

fn request(cx: &Ctx<'_, '_>, source: NodeId, attributes: Option<NodeId>) -> Option<String> {
    let ast = cx.ast();
    let mut out = name(cx, source)?;
    if let Some(a) = attributes
        && let Node::ImportAttributes { entries, .. } = *ast.node(a)
    {
        for &e in ast.list(entries) {
            out.push('\0');
            out.push_str(cx.span(e).text(cx.src()));
        }
    }
    Some(out)
}

impl Rule for NoUselessReExport {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Program]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Program { stmts: body } = *cx.node(node) else {
            return;
        };
        let mut wildcards: Vec<(String, bool)> = Vec::new();
        let mut named = Vec::new();
        for &stmt in ast.list(body) {
            match *ast.node(stmt) {
                Node::ExportAll {
                    type_only,
                    exported: None,
                    source,
                    attributes,
                } => {
                    if let Some(r) = request(cx, source, attributes) {
                        wildcards.push((r, type_only));
                    }
                }
                Node::ExportNamed {
                    source: Some(_), ..
                } => named.push(stmt),
                _ => {}
            }
        }
        let Some((first, _)) = wildcards.first() else {
            return;
        };
        if wildcards.iter().any(|(r, _)| r != first) {
            return;
        }
        let mut reports = Vec::new();
        for decl in named {
            let Node::ExportNamed {
                type_only,
                specifiers,
                source: Some(source),
                attributes,
            } = *ast.node(decl)
            else {
                continue;
            };
            let Some(req) = request(cx, source, attributes) else {
                continue;
            };
            if &req != first {
                continue;
            }
            for &spec in ast.list(specifiers) {
                let Node::ExportSpecifier {
                    type_only: spec_type,
                    local,
                    exported: None,
                } = *ast.node(spec)
                else {
                    continue;
                };
                if name(cx, local).is_none_or(|n| n == "default") {
                    continue;
                }
                let type_export = type_only || spec_type;
                if wildcards.iter().any(|(_, t)| !t || type_export) {
                    reports.push(cx.span(spec));
                }
            }
        }
        for span in reports {
            cx.report(
                span,
                "This export is redundant because the same module is already exported with `export *`.",
            )
            .id("no-useless-re-export");
        }
    }
}
