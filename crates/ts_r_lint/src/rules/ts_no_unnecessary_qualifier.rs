//! `@typescript-eslint/no-unnecessary-qualifier`: `A.x` / `A.T` inside namespace (or enum)
//! `A`, where `x` alone already names the same symbol.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-unnecessary-qualifier",
        "Disallow unnecessary namespace qualifiers",
        NO_PRESET,
        Category::Style,
    )
    .typed()
    .fixable(),
    create: create_default::<NoUnnecessaryQualifier>,
};

#[derive(Debug, Default)]
struct NoUnnecessaryQualifier;

/// `A`, `A.B`, ... (identifiers joined by non-computed accesses).
fn is_entity_name(ast: &ts_r_ast::Ast, n: NodeId) -> bool {
    match *ast.node(n) {
        Node::Ident { .. } => true,
        Node::Member { obj, prop, .. } => {
            matches!(ast.node(prop), Node::Ident { .. })
                && !chain_has_optional(ast, n)
                && is_entity_name(ast, obj)
        }
        _ => false,
    }
}

/// The qualifier and name of a namespace access.
fn parts_of(ast: &ts_r_ast::Ast, n: NodeId) -> Option<(NodeId, NodeId)> {
    match *ast.node(n) {
        Node::QualifiedName { left, right } => Some((left, right)),
        Node::Member { obj, prop, .. }
            if matches!(ast.node(prop), Node::Ident { .. }) && is_entity_name(ast, obj) =>
        {
            Some((obj, prop))
        }
        _ => None,
    }
}

impl NoUnnecessaryQualifier {
    fn unnecessary(cx: &Ctx<'_, '_>, node: NodeId, qualifier: NodeId, name: NodeId) -> bool {
        let ast = cx.ast();
        let ti = cx.types();
        let last = match *ast.node(qualifier) {
            Node::QualifiedName { right, .. } => right,
            Node::Member { prop, .. } => prop,
            _ => qualifier,
        };
        let Some(mut ns) = ti.symbol_at(last) else {
            return false;
        };
        // Enclosing `namespace` / `enum` bodies (and module augmentations) around `node`.
        let at = cx.span(node).start;
        let enclosing: Vec<Span> = cx
            .file
            .parents
            .ancestors(node)
            .filter(|&a| matches!(ast.node(a), Node::Module { .. } | Node::Enum { .. }))
            .map(|a| cx.span(a))
            .filter(|&e| head_end(cx, e) <= at)
            .collect();
        let mut in_scope = false;
        for _ in 0..8 {
            in_scope = ti.symbol_decls(ns).iter().any(|d| {
                d.in_this_file && {
                    let s = cx.span(d.node);
                    enclosing.iter().any(|&e| {
                        e.start <= s.start && s.end <= e.end && (s == e || s.end <= head_end(cx, e))
                    })
                }
            });
            if in_scope {
                break;
            }
            match ti.resolve_alias(ns) {
                Some(a) if a != ns => ns = a,
                _ => break,
            }
        }
        if !in_scope {
            return false;
        }
        let Some(text) = ident_name(ast, name) else {
            return false;
        };
        let Some(accessed) = ti.export_of(ns, text).or_else(|| ti.symbol_at(name)) else {
            return false;
        };
        let sem = cx.semantic();
        match sem.lookup(sem.enclosing_scope(qualifier), text) {
            Some(local) => sem
                .symbol(local)
                .decls
                .iter()
                .any(|d| ti.symbol_at(d.name) == Some(accessed)),
            // Members declared elsewhere (merged declarations, augmented modules) are in
            // scope inside the namespace too; enum members only inside their enum.
            None => !ti.symbol_decls(accessed).iter().any(|d| {
                ti.decl_file(d)
                    .is_some_and(|(a, _)| matches!(a.node(d.node), Node::EnumMember { .. }))
            }),
        }
    }
}

/// Where the body of a `namespace` / `enum` starts.
fn head_end(cx: &Ctx<'_, '_>, e: Span) -> u32 {
    cx.src()[e.start as usize..e.end as usize]
        .find('{')
        .map_or(e.end, |p| e.start + p as u32)
}

impl Rule for NoUnnecessaryQualifier {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::QualifiedName, NodeKind::Member]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some((qualifier, name)) = parts_of(ast, node) else {
            return;
        };
        // Only the outermost unnecessary access of a chain is reported.
        let mut cur = node;
        while let Some(p) = cx.file.parents.parent(cur) {
            match parts_of(ast, p) {
                Some((q, n)) if q == cur => {
                    if Self::unnecessary(cx, p, q, n) {
                        return;
                    }
                }
                _ => break,
            }
            cur = p;
        }
        if !Self::unnecessary(cx, node, qualifier, name) {
            return;
        }
        let text = cx.span(name).text(cx.src()).to_owned();
        let fix = Fix::delete(Span::new(cx.span(qualifier).start, cx.span(name).start));
        cx.report_node(
            qualifier,
            format!("Qualifier is unnecessary since '{text}' is in scope."),
        )
        .id("unnecessaryQualifier")
        .fix(fix);
    }
}
