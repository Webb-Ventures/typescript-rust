//! `--inject:file` (esbuild's inject, from its docs and observed output).
//!
//! Every name an injected file exports (`export let x`, `export { y as "a.b" }`, re-exports
//! included) replaces the same free name in every other module: a global identifier `x`, or
//! a member chain `a.b` on a global `a` (also `import.meta`), the longest key first. The
//! module gets an import of that export from the injected file, which runs before the entry
//! points' code: each entry point first imports every injected file for its side effects, in
//! the order given. `define` runs first, so a define that names an injected export forwards
//! to it and a define with a value wins. Writing to a replaced name is an error. Injected
//! files are not rewritten themselves.
//!
//! The imports this adds do not make a file an ES module (`Prepared::injected`): a CommonJS
//! file that refers to an injected name stays CommonJS.

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_ast::{
    Ast, ImportPhase, MetaPropKind, Modifiers, Node, NodeId, NodeList, SourceKind, UnaryOp,
};
use ts_r_config::FileSystem;
use ts_r_minify::{Scopes, build, names};

use crate::options::{BuildOptions, Loader};

/// The injected files of a build and the names they export.
#[derive(Debug, Default)]
pub struct Inject {
    /// Injected files (absolute, real paths, without duplicates, in the order given).
    pub files: Vec<String>,
    /// `(key path, file index, export name)`: the key is the export name split at `.`.
    names: Vec<(Vec<String>, usize, String)>,
    /// First segments of every key (fast rejection).
    roots: FxHashSet<String>,
    /// Entry points (absolute), which import every injected file first.
    pub entries: FxHashSet<String>,
    /// Everything above, for preparation cache keys.
    pub key: String,
}

impl Inject {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn is_injected(&self, path: &str) -> bool {
        self.files.iter().any(|f| f == path)
    }

    fn lookup(&self, path: &[&str]) -> Option<(usize, &str)> {
        self.names
            .iter()
            .find(|(k, _, _)| k.len() == path.len() && k.iter().zip(path).all(|(a, b)| a == b))
            .map(|(_, f, e)| (*f, e.as_str()))
    }

    /// Reads the injected files' exports. `files` are resolved by the caller.
    pub fn new(
        fs: &dyn FileSystem,
        opts: &BuildOptions,
        files: Vec<String>,
        entries: &[String],
    ) -> Inject {
        let mut out = Inject {
            entries: entries.iter().cloned().collect(),
            ..Inject::default()
        };
        for f in files {
            if out.files.contains(&f) {
                continue;
            }
            let i = out.files.len();
            out.files.push(f.clone());
            let loader = opts.loader_for(&f);
            if !loader.is_js() {
                continue;
            }
            let Some(src) = fs.read_file(&f) else {
                continue;
            };
            let kind = match loader {
                Loader::Ts => SourceKind::Ts,
                Loader::Tsx => SourceKind::Tsx,
                Loader::Jsx => SourceKind::Jsx,
                _ => SourceKind::Js,
            };
            let ast = ts_r_parser::parse(&src, kind).ast;
            for name in export_names(&ast) {
                let key: Vec<String> = name.split('.').map(str::to_owned).collect();
                let dotted_ok = key.iter().all(|p| names::is_identifier(p))
                    || (key.len() >= 2 && key[0] == "import" && key[1] == "meta");
                if !dotted_ok || name == "default" {
                    continue;
                }
                // The first file that exports a name wins.
                if out.names.iter().any(|(k, _, _)| *k == key) {
                    continue;
                }
                out.roots.insert(key[0].clone());
                out.names.push((key, i, name));
            }
        }
        let mut key = String::new();
        for (i, f) in out.files.iter().enumerate() {
            key.push_str(&format!("{i}:{f}\n"));
        }
        for (k, f, e) in &out.names {
            key.push_str(&format!("{}={f}:{e}\n", k.join(".")));
        }
        out.key = key;
        out
    }
}

/// Export names of a module, from its syntax.
fn export_names(ast: &Ast) -> Vec<String> {
    let mut out = Vec::new();
    let name_of = |id: NodeId| match *ast.node(id) {
        Node::Ident { name } => Some(ast.atom(name).to_owned()),
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        _ => None,
    };
    for s in build::program_stmts(ast) {
        let n = *ast.node(s);
        match n {
            Node::ExportNamed {
                type_only: false,
                specifiers,
                ..
            } => {
                for &sp in ast.list(specifiers) {
                    if let Node::ExportSpecifier {
                        type_only: false,
                        local,
                        exported,
                    } = *ast.node(sp)
                        && let Some(name) = name_of(exported.unwrap_or(local))
                    {
                        out.push(name);
                    }
                }
            }
            Node::ExportAll {
                type_only: false,
                exported: Some(e),
                ..
            } => out.extend(name_of(e)),
            _ if n.modifiers().contains(Modifiers::EXPORT)
                && !n.modifiers().contains(Modifiers::DEFAULT)
                && !n.modifiers().contains(Modifiers::DECLARE) =>
            {
                match n {
                    Node::VarDecl { decls, .. } => {
                        for &d in ast.list(decls) {
                            if let Node::VarDeclarator { name, .. } = *ast.node(d) {
                                let mut ids = Vec::new();
                                ts_r_minify::fold::binding_idents(ast, name, &mut ids);
                                out.extend(ids.into_iter().filter_map(name_of));
                            }
                        }
                    }
                    Node::FunctionDecl { name: Some(id), .. }
                    | Node::ClassDecl { name: Some(id), .. }
                    | Node::Enum { name: id, .. }
                    | Node::Module { name: id, .. } => out.extend(name_of(id)),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    out
}

/// The key path of a read of a global (`a`, `a.b`, `a["b"]`, `import.meta.x`), outermost
/// segment last.
fn key_path<'a>(ast: &'a Ast, id: NodeId, globals: &FxHashSet<u32>) -> Option<Vec<&'a str>> {
    let mut path: Vec<&str> = Vec::new();
    let mut cur = id;
    loop {
        match *ast.node(cur) {
            Node::Member { obj, prop, .. } => match *ast.node(prop) {
                Node::Ident { name } => {
                    path.push(ast.atom(name));
                    cur = obj;
                }
                _ => return None,
            },
            Node::Index { obj, index, .. } => match *ast.node(index) {
                Node::StringLit { value } => {
                    path.push(ast.atom(value));
                    cur = obj;
                }
                _ => return None,
            },
            Node::Ident { name } => {
                path.push(ast.atom(name));
                return globals.contains(&cur.raw()).then_some(path);
            }
            Node::MetaProperty {
                kind: MetaPropKind::ImportMeta,
            } => {
                path.push("meta");
                path.push("import");
                return Some(path);
            }
            _ => return None,
        }
    }
}

/// Rewrites a module's references to injected names and adds its imports (and, for an
/// entry point, the side-effect imports of every injected file). Returns the statements
/// added; `error(at, text)` reports a write to an injected name.
pub fn apply(
    ast: &mut Ast,
    scopes: &Scopes,
    inject: &Inject,
    is_entry: bool,
    error: &mut dyn FnMut(u32, String),
) -> Vec<NodeId> {
    let globals: FxHashSet<u32> = scopes
        .refs
        .iter()
        .filter(|r| r.symbol.is_none())
        .map(|r| r.node.raw())
        .collect();
    let mut writes: FxHashSet<u32> = FxHashSet::default();
    for n in &ast.nodes {
        match *n {
            Node::Assign { left, .. } => {
                writes.insert(left.raw());
            }
            Node::Update { arg, .. }
            | Node::Unary {
                op: UnaryOp::Delete,
                arg,
            } => {
                writes.insert(arg.raw());
            }
            Node::ForIn { left, .. } | Node::ForOf { left, .. } => {
                writes.insert(left.raw());
            }
            _ => {}
        }
    }
    // Names the file uses: a new local must be none of them.
    let mut taken: FxHashSet<String> = ast
        .nodes
        .iter()
        .filter_map(|n| match *n {
            Node::Ident { name } => Some(ast.atom(name).to_owned()),
            _ => None,
        })
        .collect();
    // (file, export) -> local, in first-use order.
    let mut locals: Vec<(usize, String, String)> = Vec::new();
    let mut index: FxHashMap<(usize, String), usize> = FxHashMap::default();
    let may_match = !globals.is_empty() || inject.roots.contains("import");
    if may_match {
        // Parents before children; a replaced node's children are not visited.
        let mut stack = vec![ast.root];
        let mut children = Vec::new();
        let mut steps = 0usize;
        let limit = ast.nodes.len().saturating_mul(8).max(1 << 16);
        while let Some(id) = stack.pop() {
            steps += 1;
            if steps > limit {
                break;
            }
            let found = key_path(ast, id, &globals).and_then(|mut p| {
                if !inject.roots.contains(*p.last()?) {
                    return None;
                }
                p.reverse();
                let (f, e) = inject.lookup(&p)?;
                Some((f, e.to_owned(), p.join(".")))
            });
            if let Some((f, export, dotted)) = found {
                let at = ast.span(id).start;
                if writes.contains(&id.raw()) {
                    error(
                        at,
                        format!(
                            "Cannot assign to {dotted:?} because it's an import from an injected file"
                        ),
                    );
                    continue;
                }
                let k = (f, export.clone());
                let local = match index.get(&k) {
                    Some(&i) => locals[i].2.clone(),
                    None => {
                        let base = names::sanitize(&dotted.replace('.', "_"));
                        let mut local = base.clone();
                        let mut n = 2;
                        while taken.contains(&local) {
                            local = format!("{base}{n}");
                            n += 1;
                        }
                        taken.insert(local.clone());
                        index.insert(k, locals.len());
                        locals.push((f, export, local.clone()));
                        local
                    }
                };
                let new = build::ident(ast, &local, at);
                build::replace(ast, id, new);
                continue;
            }
            children.clear();
            ast.for_each_child(id, |c| children.push(c));
            stack.extend(children.iter().rev());
        }
    }
    if locals.is_empty() && !is_entry {
        return Vec::new();
    }
    let mut added = Vec::new();
    if is_entry {
        for f in &inject.files {
            let source = build::string(ast, f, 0);
            added.push(ast.add(
                Node::ImportDecl {
                    type_only: false,
                    phase: ImportPhase::Evaluation,
                    has_clause: false,
                    specifiers: NodeList::EMPTY,
                    source,
                    attributes: None,
                },
                ts_r_ast::Span::new(0, 0),
            ));
        }
    }
    for (f, export, local) in &locals {
        let imported = if names::is_identifier(export) {
            build::ident(ast, export, 0)
        } else {
            build::string(ast, export, 0)
        };
        let local = build::ident(ast, local, 0);
        let spec = ast.add(
            Node::ImportSpecifier {
                type_only: false,
                imported: Some(imported),
                local,
            },
            ts_r_ast::Span::new(0, 0),
        );
        let specifiers = ast.add_list(&[spec]);
        let source = build::string(ast, &inject.files[*f], 0);
        added.push(ast.add(
            Node::ImportDecl {
                type_only: false,
                phase: ImportPhase::Evaluation,
                has_clause: true,
                specifiers,
                source,
                attributes: None,
            },
            ts_r_ast::Span::new(0, 0),
        ));
    }
    // After the directive prologue.
    let stmts = build::program_stmts(ast);
    let directives = stmts
        .iter()
        .take_while(|&&s| matches!(ast.node(s), Node::Directive { .. }))
        .count();
    let mut all = stmts[..directives].to_vec();
    all.extend(&added);
    all.extend(&stmts[directives..]);
    build::set_program(ast, &all);
    added
}
