//! `import/no-mutable-exports`: exported `var` and `let` bindings, whether exported where
//! they are declared, through an export list without a source, or as `export default x`.
//! Names in lists are looked up in the scope of the export statement only, as the plugin
//! does; the report is on the variable declaration (without its `export`).

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-mutable-exports",
        "Forbid the use of mutable exports with `var` or `let`.",
        NO_PRESET,
        Category::Pedantic,
    ),
    create: create_default::<NoMutableExports>,
};

#[derive(Debug, Default)]
struct NoMutableExports;

fn check_declaration(cx: &mut Ctx<'_, '_>, decl: NodeId) {
    let Node::VarDecl { kind, .. } = *cx.node(decl) else {
        return;
    };
    let kind = match kind {
        VarKind::Var => "var",
        VarKind::Let => "let",
        _ => return,
    };
    let span = cx.span(decl);
    let src = cx.src();
    let mut start = skip_trivia_forward(src, span.start as usize);
    if src[start..].starts_with("export") {
        start = skip_trivia_forward(src, start + "export".len());
    }
    let span = Span::new(start as u32, span.end);
    cx.report(
        span,
        format!("Exporting mutable '{kind}' binding, use 'const' instead."),
    );
}

fn check_name(cx: &mut Ctx<'_, '_>, at: NodeId, name: &str) {
    let sem = cx.semantic();
    let scope = sem.scope(sem.enclosing_scope(at));
    let mut decls = Vec::new();
    for &s in &scope.symbols {
        if sem.name(s) != name {
            continue;
        }
        for d in &sem.symbol(s).decls {
            if matches!(
                d.kind,
                DeclKind::Var | DeclKind::Let | DeclKind::Const | DeclKind::Using
            ) && cx.ast().kind(d.parent) == NodeKind::VarDecl
            {
                decls.push(d.parent);
            }
        }
    }
    for d in decls {
        check_declaration(cx, d);
    }
}

impl Rule for NoMutableExports {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::VarDecl,
            NodeKind::ExportNamed,
            NodeKind::ExportDefaultExpr,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::VarDecl { modifiers, .. } if modifiers.contains(Modifiers::EXPORT) => {
                check_declaration(cx, node);
            }
            Node::ExportNamed {
                specifiers,
                source: None,
                ..
            } => {
                for &s in ast.list(specifiers) {
                    if let Node::ExportSpecifier { local, .. } = *ast.node(s)
                        && let Some(name) = ident_name(ast, local)
                    {
                        check_name(cx, node, name);
                    }
                }
            }
            Node::ExportDefaultExpr { expr } => {
                if let Some(name) = ident_name(ast, skip_parens(ast, expr)) {
                    check_name(cx, node, name);
                }
            }
            _ => {}
        }
    }
}
