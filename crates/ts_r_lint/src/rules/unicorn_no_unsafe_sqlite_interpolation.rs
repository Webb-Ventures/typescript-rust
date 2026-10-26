//! `unicorn/no-unsafe-sqlite-interpolation`: no template substitutions or tagged templates
//! in the SQL passed to `exec()`/`prepare()` of a `node:sqlite` `DatabaseSync`.
//!
//! The call is `<db>.exec(sql)` or `<db>.prepare(sql)` (any `?.`, a computed key with a
//! static string value, no type arguments, a first argument that is not a spread). `<db>`
//! (through parentheses, `as`, `satisfies`, `<T>` and `!`) is `new <ctor>(...)` or a name
//! bound by a single `const` with such an initializer. `<ctor>` is a runtime import of
//! `DatabaseSync` from `"node:sqlite"`, or `.DatabaseSync` / `["DatabaseSync"]` on the
//! module's namespace, default import or `{default as x}`; `const` aliases are followed.
//! `require()`, `import()`, `import x =` and `let` bindings are not. Names resolve to value
//! bindings: a type alias or interface of the same name is skipped over.
//!
//! The SQL (through the same wrappers, and `const` aliases) is unsafe when it is a template
//! with a substitution or any tagged template. Reported on the argument (without
//! parentheses).

use super::prelude::*;
use super::unicorn_static::{Static, static_value};
use crate::semantic::{DeclKind, SymbolId};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unsafe-sqlite-interpolation",
        "Disallow interpolating values into `node:sqlite` SQL strings.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoUnsafeSqliteInterpolation>,
};

#[derive(Debug, Default)]
struct NoUnsafeSqliteInterpolation;

/// How deep `const` aliases are followed.
const MAX_DEPTH: u32 = 32;

fn skip_wrappers(cx: &Ctx<'_, '_>, mut id: NodeId) -> NodeId {
    loop {
        match *cx.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. } => id = expr,
            _ => return id,
        }
    }
}

fn is_type_decl(kind: DeclKind) -> bool {
    matches!(
        kind,
        DeclKind::TypeAlias | DeclKind::Interface | DeclKind::TypeParam
    )
}

/// The value binding `ident` refers to: type-only symbols of the same name are skipped.
fn value_symbol(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<SymbolId> {
    let sem = cx.semantic();
    let name = ident_name(cx.ast(), ident)?;
    let mut sym = sem.symbol_of_ident(ident)?;
    for _ in 0..MAX_DEPTH {
        let s = sem.symbol(sym);
        if s.decls.iter().any(|d| !is_type_decl(d.kind)) {
            return Some(sym);
        }
        let parent = sem.scope(s.scope).parent?;
        sym = sem.lookup(parent, name)?;
    }
    None
}

enum Binding {
    /// A `const` initializer.
    Const(NodeId),
    /// A runtime import from `"node:sqlite"`: the imported name (`None` for a namespace or
    /// default import).
    Import(Option<String>),
}

fn binding(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<Binding> {
    let ast = cx.ast();
    if !matches!(ast.node(ident), Node::Ident { .. }) {
        return None;
    }
    let sym = value_symbol(cx, ident)?;
    let sem = cx.semantic();
    let mut decls = sem
        .symbol(sym)
        .decls
        .iter()
        .filter(|d| !is_type_decl(d.kind));
    let decl = decls.next()?;
    if decls.next().is_some() {
        return None;
    }
    match decl.kind {
        DeclKind::Const => {
            let Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } = *ast.node(decl.node)
            else {
                return None;
            };
            (name == decl.name).then_some(Binding::Const(init))
        }
        DeclKind::ImportDefault | DeclKind::ImportNamespace | DeclKind::ImportNamed => {
            let Node::ImportDecl {
                type_only: false,
                source,
                ..
            } = *ast.node(decl.parent)
            else {
                return None;
            };
            let Node::StringLit { value } = *ast.node(source) else {
                return None;
            };
            if ast.atom(value) != "node:sqlite" {
                return None;
            }
            match *ast.node(decl.node) {
                Node::ImportSpecifier {
                    type_only: false,
                    imported,
                    local,
                } => {
                    let imported = imported.unwrap_or(local);
                    let name = match *ast.node(imported) {
                        Node::StringLit { value } => ast.atom(value).to_owned(),
                        _ => ident_name(ast, imported)?.to_owned(),
                    };
                    Some(Binding::Import((name != "default").then_some(name)))
                }
                Node::ImportSpecifier { .. } => None,
                _ => Some(Binding::Import(None)),
            }
        }
        _ => None,
    }
}

/// The `node:sqlite` module object.
fn is_sqlite_namespace(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> bool {
    let id = skip_wrappers(cx, id);
    match binding(cx, id) {
        Some(Binding::Import(None)) => true,
        Some(Binding::Const(init)) if depth < MAX_DEPTH => is_sqlite_namespace(cx, init, depth + 1),
        _ => false,
    }
}

/// The `DatabaseSync` class.
fn is_database_class(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> bool {
    let ast = cx.ast();
    let id = skip_wrappers(cx, id);
    match *ast.node(id) {
        Node::Member { obj, prop, .. } => {
            ident_name(ast, prop) == Some("DatabaseSync") && is_sqlite_namespace(cx, obj, depth)
        }
        Node::Index { obj, index, .. } => {
            matches!(*ast.node(index), Node::StringLit { value } if ast.atom(value) == "DatabaseSync")
                && is_sqlite_namespace(cx, obj, depth)
        }
        _ => match binding(cx, id) {
            Some(Binding::Import(Some(name))) => name == "DatabaseSync",
            Some(Binding::Const(init)) if depth < MAX_DEPTH => {
                is_database_class(cx, init, depth + 1)
            }
            _ => false,
        },
    }
}

/// A `DatabaseSync` instance.
fn is_database(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> bool {
    let id = skip_wrappers(cx, id);
    if let Node::New { callee, .. } = *cx.node(id) {
        return is_database_class(cx, callee, 0);
    }
    match binding(cx, id) {
        Some(Binding::Const(init)) if depth < MAX_DEPTH => is_database(cx, init, depth + 1),
        _ => false,
    }
}

/// SQL built with a substitution or a tag.
fn is_unsafe_sql(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> bool {
    let ast = cx.ast();
    let id = skip_wrappers(cx, id);
    match *ast.node(id) {
        Node::TemplateLit { parts } => ast.list(parts).len() > 1,
        Node::TaggedTemplate { .. } => true,
        _ => match binding(cx, id) {
            Some(Binding::Const(init)) if depth < MAX_DEPTH => is_unsafe_sql(cx, init, depth + 1),
            _ => false,
        },
    }
}

impl Rule for NoUnsafeSqliteInterpolation {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call {
            callee,
            type_args: None,
            args,
            ..
        } = *ast.node(node)
        else {
            return;
        };
        let Some(&sql) = ast.list(args).first() else {
            return;
        };
        if matches!(ast.node(sql), Node::Spread { .. }) {
            return;
        }
        let (obj, method) = match *ast.node(skip_parens(ast, callee)) {
            Node::Member { obj, prop, .. } => (obj, ident_name(ast, prop).map(str::to_owned)),
            Node::Index { obj, index, .. } => match static_value(cx, index) {
                Some(Static::Str(s)) => (obj, Some(s)),
                _ => return,
            },
            _ => return,
        };
        if !matches!(method.as_deref(), Some("exec" | "prepare"))
            || !is_unsafe_sql(cx, sql, 0)
            || !is_database(cx, obj, 0)
        {
            return;
        }
        let span = cx.span(skip_parens(ast, sql));
        cx.report(
            span,
            "Do not interpolate values into `node:sqlite` SQL strings.",
        )
        .id("no-unsafe-sqlite-interpolation");
    }
}
