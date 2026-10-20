//! `no-shadow-restricted-names`: disallow identifiers from shadowing restricted names.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-shadow-restricted-names",
        "Disallow identifiers from shadowing restricted names",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create::<NoShadowRestrictedNames>,
};

#[derive(Debug, Default)]
struct NoShadowRestrictedNames {
    global_this: bool,
}

impl FromOptions for NoShadowRestrictedNames {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoShadowRestrictedNames {
            global_this: bool_field(obj_opt(opts, 0), "reportGlobalThis", true),
        })
    }
}

impl Rule for NoShadowRestrictedNames {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let sem = cx.semantic();
        let mut hits: Vec<(Span, String)> = Vec::new();
        for sym in sem.symbols() {
            let name = ast_name(cx, sym);
            let restricted = matches!(
                name.as_str(),
                "NaN" | "Infinity" | "undefined" | "eval" | "arguments"
            ) || (self.global_this && name == "globalThis");
            if !restricted || sym.flags.contains(SymbolFlags::COMMENT_GLOBAL) {
                continue;
            }
            for d in &sym.decls {
                if matches!(d.kind, DeclKind::Arguments | DeclKind::CommentGlobal) {
                    continue;
                }
                if matches!(
                    d.kind,
                    DeclKind::TypeAlias | DeclKind::Interface | DeclKind::TypeParam
                ) {
                    continue;
                }
                // `var undefined;` without an initializer (and never assigned) is allowed.
                if name == "undefined"
                    && matches!(d.kind, DeclKind::Var | DeclKind::Let)
                    && matches!(ast.node(d.node), Node::VarDeclarator { init: None, name: n, .. } if *n == d.name)
                    && !sym.refs.iter().any(|r| sem.reference(*r).is_write())
                {
                    continue;
                }
                hits.push((ast.span(d.name), name.clone()));
            }
        }
        hits.sort_by_key(|h| h.0.start);
        hits.dedup_by_key(|h| h.0.start);
        for (span, name) in hits {
            cx.report(span, format!("Shadowing of global property '{name}'."))
                .id("shadowingRestrictedName");
        }
    }
}

fn ast_name(cx: &Ctx<'_, '_>, sym: &crate::semantic::Symbol) -> String {
    sym.decls
        .first()
        .and_then(|d| ident_name(cx.ast(), d.name))
        .unwrap_or("")
        .to_owned()
}
