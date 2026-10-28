//! `unicorn/consistent-assert`: the assert function imported from `assert` / `assert/strict`
//! (with or without `node:`) as a default import, `{default as x}`, or (from `assert`)
//! `{strict}`, called directly, should be called as `x.ok(…)`. Type-only imports are
//! ignored. Each call's callee is reported and gets `.ok` appended.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/consistent-assert",
        "Enforce consistent assertion style with `node:assert`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<ConsistentAssert>,
};

#[derive(Debug, Default)]
struct ConsistentAssert;

impl Rule for ConsistentAssert {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ImportDecl]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::ImportDecl {
            type_only: false,
            specifiers,
            source,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        let Node::StringLit { value } = *ast.node(source) else {
            return;
        };
        let module = ast.atom(value);
        let module = module.strip_prefix("node:").unwrap_or(module);
        if module != "assert" && module != "assert/strict" {
            return;
        }
        let mut hits = Vec::new();
        for &spec in ast.list(specifiers) {
            let local = match *ast.node(spec) {
                Node::ImportDefaultSpecifier { local } => local,
                Node::ImportSpecifier {
                    type_only: false,
                    imported,
                    local,
                } => {
                    let imported = imported.unwrap_or(local);
                    match ident_name(ast, imported) {
                        Some("default") => local,
                        Some("strict") if module == "assert" => local,
                        _ => continue,
                    }
                }
                _ => continue,
            };
            let sem = cx.semantic();
            let Some(sym) = sem.symbol_of_decl(local) else {
                continue;
            };
            for &r in &sem.symbol(sym).refs {
                let ident = sem.reference(r).node;
                let mut up = ident;
                while let Some(p) = cx.parent(up)
                    && matches!(ast.node(p), Node::Paren { .. })
                {
                    up = p;
                }
                if let Some(p) = cx.parent(up)
                    && let Node::Call { callee, .. } = *ast.node(p)
                    && callee == up
                {
                    hits.push(ident);
                }
            }
        }
        for ident in hits {
            let name = cx.text(ident);
            let fix = Fix::insert(cx.span(ident).end, ".ok");
            cx.report_node(ident, format!("Prefer `{name}.ok(…)` over `{name}(…)`."))
                .id("consistent-assert/error")
                .fix(fix);
        }
    }
}
