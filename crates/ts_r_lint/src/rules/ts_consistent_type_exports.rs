//! `@typescript-eslint/consistent-type-exports`: exports of type-only names use `export type`
//! (and `export *` of a module with no values uses `export type *`).

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/consistent-type-exports",
        "Enforce consistent usage of type exports",
        NO_PRESET,
        Category::Style,
    )
    .typed()
    .fixable(),
    create: create::<ConsistentTypeExports>,
};

#[derive(Debug)]
struct ConsistentTypeExports {
    inline: bool,
}

impl FromOptions for ConsistentTypeExports {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(ConsistentTypeExports {
            inline: bool_field(
                obj_opt(opts, 0),
                "fixMixedExportsWithInlineTypeSpecifier",
                false,
            ),
        })
    }
}

/// A namespace with only types in it (TypeScript's "non-instantiated" module).
fn non_instantiated(ast: &ts_r_ast::Ast, module: NodeId, depth: u32) -> bool {
    let Node::Module { body, .. } = *ast.node(module) else {
        return false;
    };
    let Some(body) = body else {
        return true;
    };
    let stmts = match *ast.node(body) {
        Node::ModuleBlock { stmts } => ast.list(stmts),
        // `namespace A.B {}`: the body is the inner module.
        Node::Module { .. } => return depth < 32 && non_instantiated(ast, body, depth + 1),
        _ => return false,
    };
    stmts.iter().all(|&s| match ast.node(s) {
        Node::Interface { .. } | Node::TypeAlias { .. } => true,
        Node::Module { .. } => depth < 32 && non_instantiated(ast, s, depth + 1),
        Node::ImportDecl { type_only, .. } | Node::ExportNamed { type_only, .. } => *type_only,
        _ => false,
    })
}

/// A declaration made with `import type` / `export type` (or a `type` specifier).
fn type_only_decl(cx: &Ctx<'_, '_>, d: &crate::types::Decl) -> bool {
    let Some((ast, _)) = cx.types().decl_file(d) else {
        return false;
    };
    let own = match *ast.node(d.node) {
        Node::ImportSpecifier { type_only, .. }
        | Node::ExportSpecifier { type_only, .. }
        | Node::ImportEquals { type_only, .. }
        | Node::ImportDecl { type_only, .. }
        | Node::ExportNamed { type_only, .. }
        | Node::ExportAll { type_only, .. } => type_only,
        _ => false,
    };
    // The declaration's parent, known for the linted file.
    own || (d.in_this_file
        && cx.file.parents.parent(d.node).is_some_and(|p| {
            matches!(
                cx.ast().node(p),
                Node::ImportDecl {
                    type_only: true,
                    ..
                } | Node::ExportNamed {
                    type_only: true,
                    ..
                }
            )
        }))
}

/// Whether the exported symbol is only a type (`None` when unknown).
fn type_based(cx: &Ctx<'_, '_>, sym: Option<Sym>, depth: u32) -> Option<bool> {
    let ti = cx.types();
    let sym = sym?;
    if depth > 16 {
        return None;
    }
    let decls = ti.symbol_decls(sym);
    if decls.iter().any(|d| type_only_decl(cx, d)) {
        return Some(true);
    }
    let f = ti.symbol_facts(sym);
    let module_value = f.module
        && decls.iter().any(|d| {
            ti.decl_file(d).is_some_and(|(ast, _)| {
                matches!(ast.node(d.node), Node::Program { .. })
                    || (matches!(ast.node(d.node), Node::Module { .. })
                        && !non_instantiated(ast, d.node, 0))
            })
        });
    if f.variable
        || f.function
        || f.class
        || f.enum_
        || f.method
        || f.property
        || f.accessor
        || f.enum_member
        || module_value
    {
        return Some(false);
    }
    if f.alias {
        // An import merged with a value of the same name in its own module (our checker
        // keeps the two apart).
        if decls.iter().any(|d| merged_with_value(cx, d)) {
            return Some(false);
        }
        let next = immediate_alias(cx, sym).or_else(|| ti.resolve_alias(sym));
        return type_based(cx, next.filter(|&a| a != sym), depth + 1);
    }
    Some(true)
}

/// An import or `export { x }` in another file whose name that file also declares
/// as a top-level value.
fn merged_with_value(cx: &Ctx<'_, '_>, d: &crate::types::Decl) -> bool {
    if d.in_this_file {
        return false;
    }
    let Some((ast, _)) = cx.types().decl_file(d) else {
        return false;
    };
    let local = match *ast.node(d.node) {
        Node::ImportSpecifier { local, .. }
        | Node::ImportDefaultSpecifier { local }
        | Node::ImportNamespaceSpecifier { local }
        | Node::ExportSpecifier { local, .. } => local,
        _ => return false,
    };
    let Some(name) = ident_name(ast, local) else {
        return false;
    };
    let Node::Program { stmts } = *ast.node(ast.root) else {
        return false;
    };
    let named = |n: Option<NodeId>| n.and_then(|n| ident_name(ast, n)) == Some(name);
    ast.list(stmts).iter().any(|&s| match *ast.node(s) {
        Node::VarDecl { decls, .. } => ast.list(decls).iter().any(|&v| {
            let Node::VarDeclarator { name: pat, .. } = *ast.node(v) else {
                return false;
            };
            let mut ids = Vec::new();
            binding_idents(ast, pat, &mut ids);
            ids.iter().any(|&i| named(Some(i)))
        }),
        Node::FunctionDecl { name: n, .. } | Node::ClassDecl { name: n, .. } => named(n),
        Node::Enum { name: n, .. } => named(Some(n)),
        Node::Module { name: n, .. } => named(Some(n)) && !non_instantiated(ast, s, 0),
        _ => false,
    })
}

/// The symbol an import or re-export in the linted file names in its source module (one
/// hop, where `resolve_alias` follows the whole chain).
fn immediate_alias(cx: &Ctx<'_, '_>, sym: Sym) -> Option<Sym> {
    let ti = cx.types();
    let ast = cx.ast();
    for d in ti.symbol_decls(sym) {
        if !d.in_this_file {
            continue;
        }
        let parent = cx.file.parents.parent(d.node);
        let source = parent.and_then(|p| match *ast.node(p) {
            Node::ImportDecl { source, .. } => Some(source),
            Node::ExportNamed { source, .. } => source,
            _ => None,
        });
        let Some(module) = source.and_then(|s| ti.symbol_at(s)) else {
            continue;
        };
        let name = match *ast.node(d.node) {
            Node::ImportSpecifier {
                imported, local, ..
            } => export_name(ast, imported.unwrap_or(local)),
            Node::ExportSpecifier { local, .. } => export_name(ast, local),
            Node::ImportDefaultSpecifier { .. } => Some("default"),
            Node::ImportNamespaceSpecifier { .. } => return Some(module),
            _ => None,
        };
        if let Some(n) = name {
            return ti.export_of(module, n);
        }
    }
    None
}

fn export_name(ast: &ts_r_ast::Ast, id: NodeId) -> Option<&str> {
    match *ast.node(id) {
        Node::StringLit { value } => Some(ast.atom(value)),
        _ => ident_name(ast, id),
    }
}

fn name_text<'a>(cx: &Ctx<'_, 'a>, id: NodeId) -> &'a str {
    let src: &'a str = cx.src();
    cx.span(id).text(src)
}

/// `local` or `local as exported`, as written.
fn specifier_text(cx: &Ctx<'_, '_>, spec: NodeId) -> String {
    let Node::ExportSpecifier {
        local, exported, ..
    } = *cx.node(spec)
    else {
        return String::new();
    };
    let l = name_text(cx, local);
    match exported.map(|e| name_text(cx, e)) {
        Some(e) if e != l => format!("{l} as {e}"),
        _ => l.to_owned(),
    }
}

impl ConsistentTypeExports {
    fn check_named(&self, cx: &mut Ctx<'_, '_>, node: NodeId) {
        let ast = cx.ast();
        let src = cx.src();
        let Node::ExportNamed {
            type_only: false,
            specifiers,
            source,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        let (mut inline, mut types, mut values) = (Vec::new(), Vec::new(), Vec::new());
        for &s in ast.list(specifiers) {
            let Node::ExportSpecifier {
                type_only,
                local,
                exported,
            } = *ast.node(s)
            else {
                continue;
            };
            if type_only {
                inline.push(s);
                continue;
            }
            let ti = cx.types();
            let sym = match source.and_then(|src| ti.symbol_at(src)) {
                Some(module) => export_name(ast, local).and_then(|n| ti.export_of(module, n)),
                None => ti.symbol_at(exported.unwrap_or(local)),
            };
            match type_based(cx, sym, 0) {
                Some(true) => types.push(s),
                Some(false) => values.push(s),
                None => {}
            }
        }
        if types.is_empty() {
            return;
        }
        let span = cx.span(node);
        if values.is_empty() {
            // `export { ... }` -> `export type { ... }`, dropping inline `type`s.
            let after_export = span.start + "export".len() as u32;
            let mut fix = Fix::insert(after_export, " type");
            for &s in &inline {
                let start = cx.span(s).start;
                let next = skip_ws(src, start as usize + "type".len());
                fix = fix.and(Span::new(start, next as u32), "");
            }
            cx.report_node(
                node,
                "All exports in the declaration are only used as types. Use `export type`.",
            )
            .id("typeOverValue")
            .fix(fix);
            return;
        }
        let names: Vec<&str> = types
            .iter()
            .map(|&s| match *ast.node(s) {
                Node::ExportSpecifier { local, .. } => match *ast.node(local) {
                    Node::StringLit { value } => ast.atom(value),
                    _ => ident_name(ast, local).unwrap_or_default(),
                },
                _ => "",
            })
            .collect();
        let (msg, id) = if let [one] = names[..] {
            (
                format!(
                    "Type export {one} is not a value and should be exported using `export type`."
                ),
                "singleExportIsType",
            )
        } else {
            let list = format!(
                "{} and {}",
                names[..names.len() - 1].join(", "),
                names[names.len() - 1]
            );
            (
                format!(
                    "Type exports {list} are not values and should be exported using `export type`."
                ),
                "multipleExportsAreTypes",
            )
        };
        let fix = if self.inline {
            let mut fix: Option<Fix> = None;
            for &s in &types {
                let at = cx.span(s).start;
                fix = Some(match fix {
                    None => Fix::insert(at, "type "),
                    Some(f) => f.and(Span::new(at, at), "type "),
                });
            }
            fix
        } else {
            let all: Vec<String> = types
                .iter()
                .chain(&inline)
                .map(|&s| specifier_text(cx, s))
                .collect();
            let kept: Vec<String> = values.iter().map(|&s| specifier_text(cx, s)).collect();
            let from = source
                .and_then(|s| match *ast.node(s) {
                    Node::StringLit { value } => Some(format!(" from '{}'", ast.atom(value))),
                    _ => None,
                })
                .unwrap_or_default();
            let open = find_char(src, span.start, b'{');
            let close = src[..span.end as usize].rfind('}').map(|p| p as u32);
            match (open, close) {
                (Some(o), Some(c)) if o < c => Some(
                    Fix::replace(Span::new(o + 1, c), format!(" {} ", kept.join(", "))).and(
                        Span::new(span.start, span.start),
                        format!("export type {{ {} }}{from};\n", all.join(", ")),
                    ),
                ),
                _ => None,
            }
        };
        cx.report_node(node, msg).id(id).fix_opt(fix);
    }

    fn check_all(cx: &mut Ctx<'_, '_>, node: NodeId) {
        let Node::ExportAll {
            type_only: false,
            source,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        let ti = cx.types();
        let Some(module) = ti.symbol_at(source) else {
            return;
        };
        let t = ti.type_of_symbol(module);
        if t == Ty::UNRESOLVED || !ti.properties_of(t).is_empty() {
            return;
        }
        let Some(star) = find_char(cx.src(), cx.span(node).start, b'*') else {
            return;
        };
        cx.report_node(
            node,
            "All exports in the declaration are only used as types. Use `export type`.",
        )
        .id("typeOverValue")
        .fix(Fix::insert(star, "type "));
    }
}

/// The next position after whitespace.
fn skip_ws(src: &str, mut pos: usize) -> usize {
    let b = src.as_bytes();
    while pos < b.len() && b[pos].is_ascii_whitespace() {
        pos += 1;
    }
    pos
}

impl Rule for ConsistentTypeExports {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ExportNamed, NodeKind::ExportAll]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match cx.node(node) {
            Node::ExportNamed { .. } => self.check_named(cx, node),
            _ => Self::check_all(cx, node),
        }
    }
}
