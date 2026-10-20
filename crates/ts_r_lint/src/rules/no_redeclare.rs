//! `no-redeclare`: disallow variable redeclaration.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-redeclare",
        "Disallow variable redeclaration",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create::<NoRedeclare>,
};

#[derive(Debug, Default)]
struct NoRedeclare {
    builtin_globals: bool,
    /// `@typescript-eslint/no-redeclare`: `Some(ignoreDeclarationMerge)`.
    ts: Option<bool>,
}

/// `@typescript-eslint/no-redeclare`: also checks type declarations.
pub(crate) fn create_ts(opts: &[Value]) -> Result<Box<dyn Rule>, String> {
    let o = obj_opt(opts, 0);
    Ok(Box::new(NoRedeclare {
        builtin_globals: bool_field(o, "builtinGlobals", true),
        ts: Some(bool_field(o, "ignoreDeclarationMerge", true)),
    }))
}

/// Type-level declarations counted by the TypeScript variant.
fn is_ts_decl(ast: &ts_r_ast::Ast, d: &crate::semantic::Decl) -> bool {
    is_value_decl(ast, d)
        || matches!(
            d.kind,
            DeclKind::TypeAlias
                | DeclKind::Interface
                | DeclKind::Enum
                | DeclKind::Namespace
                | DeclKind::ImportEquals
        )
}

/// Drops declarations that TypeScript merges: namespaces with classes, functions, enums and
/// interfaces, and interfaces with classes.
fn drop_merges(decls: &mut Vec<&crate::semantic::Decl>) {
    let rest_ok = |decls: &[&crate::semantic::Decl], skip: DeclKind, ok: &[DeclKind]| {
        decls
            .iter()
            .filter(|d| d.kind != skip)
            .all(|d| ok.contains(&d.kind))
    };
    if rest_ok(
        decls,
        DeclKind::Namespace,
        &[
            DeclKind::Class,
            DeclKind::Function,
            DeclKind::Enum,
            DeclKind::Interface,
        ],
    ) {
        decls.retain(|d| d.kind != DeclKind::Namespace);
    }
    if rest_ok(decls, DeclKind::Interface, &[DeclKind::Class]) {
        decls.retain(|d| d.kind != DeclKind::Interface);
    }
}

impl FromOptions for NoRedeclare {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoRedeclare {
            builtin_globals: bool_field(obj_opt(opts, 0), "builtinGlobals", true),
            ts: None,
        })
    }
}

/// Declarations that ESLint's scope analysis treats as value redeclarations.
pub(crate) fn is_value_decl(ast: &ts_r_ast::Ast, d: &crate::semantic::Decl) -> bool {
    match d.kind {
        DeclKind::Var | DeclKind::Let | DeclKind::Const | DeclKind::Using | DeclKind::Class => true,
        DeclKind::Param | DeclKind::CatchParam => true,
        DeclKind::Function => !matches!(ast.node(d.node), Node::FunctionDecl { body: None, .. }),
        DeclKind::ImportDefault | DeclKind::ImportNamespace | DeclKind::ImportNamed => true,
        _ => false,
    }
}

const REDECLARED: &str = "is already defined.";
const BUILTIN: &str = "is already defined as a built-in global variable.";
const BY_SYNTAX: &str = "is already defined by a variable declaration.";

impl Rule for NoRedeclare {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let sem = cx.semantic();
        let ast = cx.ast();
        let env = cx.env();
        // (span, name, message, id)
        let mut hits: Vec<(Span, String, &str, &'static str)> = Vec::new();
        let comments: Vec<_> = cx.file.comment_globals.iter().filter(|g| !g.off).collect();
        let mut handled_comment_names: Vec<&str> = Vec::new();
        for sym in sem.symbols() {
            let name = ast.atom(sym.name);
            let mut decls: Vec<_> = match self.ts {
                // On TypeScript code ESLint's scope analysis also defines type aliases,
                // interfaces, enums and namespaces as variables.
                None => sym
                    .decls
                    .iter()
                    .filter(|d| {
                        is_value_decl(ast, d)
                            || (env.typescript
                                && matches!(
                                    d.kind,
                                    DeclKind::TypeAlias
                                        | DeclKind::Interface
                                        | DeclKind::Enum
                                        | DeclKind::Namespace
                                ))
                    })
                    .collect(),
                Some(_) => sym.decls.iter().filter(|d| is_ts_decl(ast, d)).collect(),
            };
            if self.ts == Some(true) {
                drop_merges(&mut decls);
            }
            let global = sem.scope(sym.scope).kind == ScopeKind::Global;
            let cmts: Vec<Span> = if global {
                comments
                    .iter()
                    .filter(|g| g.name == name)
                    .map(|g| g.span)
                    .collect()
            } else {
                Vec::new()
            };
            if global && !cmts.is_empty() {
                handled_comment_names.push(name);
            }
            let syntax: Vec<Span> = decls.iter().map(|d| cx.span(d.name)).collect();
            if global && self.builtin_globals && env.is_builtin_global(name) {
                for s in cmts.iter().chain(syntax.iter()) {
                    hits.push((*s, name.to_owned(), BUILTIN, "redeclaredAsBuiltin"));
                }
                continue;
            }
            if self.builtin_globals && !cmts.is_empty() && !syntax.is_empty() {
                // The TypeScript variant reports the declarations instead of the comment.
                if self.ts.is_some() {
                    for s in &syntax {
                        hits.push((*s, name.to_owned(), BY_SYNTAX, "redeclaredBySyntax"));
                    }
                    continue;
                }
                for s in &cmts {
                    hits.push((*s, name.to_owned(), BY_SYNTAX, "redeclaredBySyntax"));
                }
                for s in syntax.iter().skip(1) {
                    hits.push((*s, name.to_owned(), REDECLARED, "redeclared"));
                }
                continue;
            }
            let all: Vec<Span> = cmts.iter().chain(syntax.iter()).copied().collect();
            for s in all.iter().skip(1) {
                hits.push((*s, name.to_owned(), REDECLARED, "redeclared"));
            }
        }
        // Comment globals with no symbol (e.g. names that are also configured globals).
        let mut seen: Vec<&str> = Vec::new();
        for g in &comments {
            let name = g.name.as_str();
            if handled_comment_names.contains(&name) || seen.contains(&name) {
                continue;
            }
            seen.push(name);
            let cmts: Vec<Span> = comments
                .iter()
                .filter(|c| c.name == name)
                .map(|c| c.span)
                .collect();
            if self.builtin_globals && env.is_builtin_global(name) {
                for s in &cmts {
                    hits.push((*s, name.to_owned(), BUILTIN, "redeclaredAsBuiltin"));
                }
            } else {
                for s in cmts.iter().skip(1) {
                    hits.push((*s, name.to_owned(), REDECLARED, "redeclared"));
                }
            }
        }
        for (span, name, msg, id) in hits {
            cx.report(span, format!("'{name}' {msg}")).id(id);
        }
    }
}
