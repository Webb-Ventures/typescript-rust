//! Missing imports (TS2304, TS2552, TS2503): which modules export a name, the specifier to
//! write, and the edits adding the import. Rules observed from tsserver 6.0.3 on the
//! `fix-import-*` fixtures:
//!
//! - A module of the program that exports the name (or default-exports a declaration of that
//!   name) is a source. Its specifier is a bare one when some import in the program resolves
//!   to that file by a bare specifier (packages, workspace packages), a `paths` alias when
//!   that is shorter, else a relative path (no extension, no `/index`, `.js` when the file's
//!   relative imports use it or resolution is node16/nodenext). Files outside the project
//!   directory with no bare specifier are not offered.
//! - When the file already imports that module (not namespace, not side-effect), the name is
//!   added to that import ("Update import from"): in sorted position when the braces are
//!   sorted, else at the end; `type X` inline under `verbatimModuleSyntax`.
//! - Otherwise a new declaration ("Add import from") goes before the first import whose
//!   module sorts after it (when the imports are sorted), else after the last import; in a
//!   file without imports, after directives and header comments that a blank line separates
//!   from the code, followed by a blank line. Several new declarations at one place come in
//!   reverse order of creation. Quotes follow the first import, semicolons the file.

use super::Ctx;
use super::edit::Edit;
use super::organize::{Case, cmp_spec, cmp_str};
use super::style;
use std::cmp::Ordering;
use std::collections::HashMap;
use ts_r_ast::{Ast, ImportPhase, Node, NodeId};
use ts_r_binder::meaning;
use ts_r_checker::Ck;
use ts_r_config::ModuleResolution;
use ts_r_diagnostics::FileId;

/// A module that exports a missing name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub spec: String,
    /// Imported as the default export.
    pub default: bool,
    /// Only a type (imported with `type` under `verbatimModuleSyntax`).
    pub type_only: bool,
}

/// Names to import from one module.
#[derive(Clone, Debug, Default)]
pub struct Want {
    pub spec: String,
    pub default: Option<String>,
    /// Named imports: name, type-only.
    pub named: Vec<(String, bool)>,
}

/// The name a diagnostic is about and the meaning it needs (value, type, namespace).
pub fn missing_name(cx: &Ctx<'_, '_>, start: u32) -> Option<(NodeId, String, u8)> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let id = super::super::locate::ident_at(ast, start)?;
    let name = super::super::locate::name_text(ast, id)?.to_string();
    let m = f
        .bound
        .refs
        .free
        .iter()
        .find(|r| r.node == id)
        .map_or(meaning::VALUE, |r| r.meaning);
    Some((id, name, m))
}

/// Modules exporting `name` with meaning `want`, best first.
pub fn sources(cx: &Ctx<'_, '_>, ck: &Ck<'_, '_>, name: &str, want: u8) -> Vec<Source> {
    let c = ck.c;
    let here = c.file(cx.file);
    let specs = Specifiers::new(cx);
    let verbatim = cx.program.options.verbatim_module_syntax() == Some(true);
    let mut out: Vec<Source> = Vec::new();
    for (i, f) in c.files.iter().enumerate() {
        let fid = FileId(i as u32);
        if fid == cx.file || f.is_lib || !f.bound.is_module {
            continue;
        }
        let module_sym = c.sym(fid, f.bound.file_symbol);
        let mut found: Option<(bool, ts_r_types::SymId)> = None;
        for (export, s) in c.exports_of(module_sym) {
            if export == name {
                found = Some((false, s));
                break;
            }
            if export == "default" {
                let target = c.resolve_alias(s).unwrap_or(s);
                let named = c.first_decl(target).and_then(|d| {
                    let ast = c.ast(d.file);
                    let n = ts_r_binder::decl_name_node(ast, d.node)?;
                    super::super::locate::name_text(ast, n).map(str::to_string)
                });
                if named.as_deref() == Some(name) {
                    found = Some((true, s));
                }
            }
        }
        let Some((default, s)) = found else {
            continue;
        };
        let target = c.resolve_alias(s).unwrap_or(s);
        if !c.has_meaning(target, want) {
            continue;
        }
        let Some(spec) = specs.of(&here.path, &f.path) else {
            continue;
        };
        let type_only = verbatim && !c.has_meaning(target, meaning::VALUE);
        let src = Source {
            spec,
            default,
            type_only,
        };
        if !out.contains(&src) {
            out.push(src);
        }
    }
    out.sort_by(|a, b| {
        spec_rank(&a.spec)
            .cmp(&spec_rank(&b.spec))
            .then_with(|| a.spec.cmp(&b.spec))
    });
    out
}

/// Shorter and closer specifiers first.
fn spec_rank(s: &str) -> (usize, usize, usize) {
    (s.matches("../").count(), s.matches('/').count(), s.len())
}

/// Computes module specifiers from the current file.
struct Specifiers<'a> {
    /// Resolved path → the bare specifiers that resolve to it.
    bare: HashMap<&'a str, Vec<&'a str>>,
    js_ext: bool,
    project_dir: Option<String>,
    paths: Vec<(String, String)>,
}

impl<'a> Specifiers<'a> {
    fn new(cx: &Ctx<'a, '_>) -> Specifiers<'a> {
        let p = cx.program;
        let mut bare: HashMap<&str, Vec<&str>> = HashMap::new();
        for ((_, spec), r) in &p.resolutions {
            if let Some(r) = r
                && !spec.starts_with('.')
                && !spec.starts_with('/')
            {
                bare.entry(r.resolved.as_str())
                    .or_default()
                    .push(spec.as_str());
            }
        }
        let f = cx.checker.file(cx.file);
        let node = matches!(
            p.options.effective().resolution_algorithm(),
            ModuleResolution::Node16 | ModuleResolution::NodeNext
        );
        let mut js_ext = node;
        for s in style::top_level(&f.ast) {
            if let Node::ImportDecl { source, .. } = *f.ast.node(s) {
                let raw = f.ast.span(source).text(&f.text);
                let v = raw.trim_matches(|ch| ch == '"' || ch == '\'');
                if v.starts_with('.') && (v.ends_with(".js") || v.ends_with(".jsx")) {
                    js_ext = true;
                }
            }
        }
        let project_dir = p
            .options
            .config_file_path
            .as_deref()
            .and_then(|c| c.rsplit_once('/'))
            .map(|(d, _)| format!("{d}/"));
        let mut paths = Vec::new();
        let base = p.options.paths_base_path.clone().or_else(|| {
            project_dir
                .clone()
                .map(|d| d.trim_end_matches('/').to_string())
        });
        if let (Some(map), Some(base)) = (p.options.paths(), base) {
            for (pattern, targets) in &map.0 {
                let Some(prefix) = pattern.strip_suffix('*') else {
                    continue;
                };
                for t in targets {
                    if let Some(tp) = t.strip_suffix('*') {
                        let tp = tp.trim_start_matches("./");
                        paths.push((prefix.to_string(), format!("{base}/{tp}")));
                    }
                }
            }
        }
        Specifiers {
            bare,
            js_ext,
            project_dir,
            paths,
        }
    }

    fn of(&self, from: &str, to: &str) -> Option<String> {
        if let Some(list) = self.bare.get(to) {
            return list
                .iter()
                .min_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)))
                .map(|s| s.to_string());
        }
        if let Some(i) = to.rfind("/node_modules/") {
            let rest = &to[i + "/node_modules/".len()..];
            let rest = rest.strip_prefix("@types/").unwrap_or(rest);
            let pkg = rest.strip_suffix("/index.d.ts")?;
            if pkg.contains("/node_modules/")
                || pkg.matches('/').count() > usize::from(pkg.starts_with('@'))
            {
                return None;
            }
            return Some(match pkg.split_once("__") {
                Some((scope, name)) if !pkg.starts_with('@') => format!("@{scope}/{name}"),
                _ => pkg.to_string(),
            });
        }
        let rel = relative(from, to, self.js_ext);
        let alias = self.paths.iter().find_map(|(prefix, target)| {
            let rest = to.strip_prefix(target.as_str())?;
            Some(format!("{prefix}{}", strip_ext(rest, self.js_ext)))
        });
        if let Some(a) = alias
            && a.matches('/').count() < rel.matches('/').count()
        {
            return Some(a);
        }
        if let Some(dir) = &self.project_dir
            && !to.starts_with(dir.as_str())
        {
            return None;
        }
        Some(rel)
    }
}

/// `rest` without its TypeScript extension (`.js` instead with `js_ext`) and `/index`.
fn strip_ext(rest: &str, js_ext: bool) -> String {
    let stem = [
        "d.ts", "d.mts", "d.cts", "tsx", "ts", "mts", "cts", "jsx", "js", "mjs", "cjs",
    ]
    .iter()
    .find_map(|e| rest.strip_suffix(&format!(".{e}")).map(|x| (x, *e)));
    match stem {
        Some((base, ext)) if js_ext => {
            let js = match ext {
                "mts" | "d.mts" | "mjs" => "mjs",
                "cts" | "d.cts" | "cjs" => "cjs",
                _ => "js",
            };
            format!("{base}.{js}")
        }
        Some((base, _)) => base.strip_suffix("/index").unwrap_or(base).to_string(),
        None => rest.to_string(),
    }
}

/// Relative specifier from file `from` to file `to`.
pub fn relative(from: &str, to: &str, js_ext: bool) -> String {
    let from_dir: Vec<&str> = from
        .rsplit_once('/')
        .map_or("", |x| x.0)
        .split('/')
        .collect();
    let to_parts: Vec<&str> = to.split('/').collect();
    let mut common = 0;
    while common < from_dir.len()
        && common + 1 < to_parts.len()
        && from_dir[common] == to_parts[common]
    {
        common += 1;
    }
    let mut s = String::new();
    if common == from_dir.len() {
        s.push_str("./");
    } else {
        for _ in common..from_dir.len() {
            s.push_str("../");
        }
    }
    s.push_str(&to_parts[common..].join("/"));
    let out = strip_ext(&s, js_ext);
    if out == "." || out.is_empty() {
        "./".into()
    } else if out == ".." {
        "../".into()
    } else {
        out
    }
}

/// An existing import declaration of a module.
struct Existing {
    node: NodeId,
    type_only: bool,
    default: Option<NodeId>,
    namespace: bool,
    /// Named specifiers (empty braces count: `Some(vec![])`).
    named: Option<Vec<NodeId>>,
}

fn existing(ast: &Ast, text: &str, spec: &str) -> Vec<Existing> {
    let mut out = Vec::new();
    for s in style::top_level(ast) {
        let Node::ImportDecl {
            type_only,
            phase,
            has_clause,
            specifiers,
            source,
            ..
        } = *ast.node(s)
        else {
            continue;
        };
        if phase != ImportPhase::Evaluation || !has_clause {
            continue;
        }
        let Node::StringLit { value } = *ast.node(source) else {
            continue;
        };
        if ast.atom(value) != spec {
            continue;
        }
        let mut e = Existing {
            node: s,
            type_only,
            default: None,
            namespace: false,
            named: None,
        };
        for &sp in ast.list(specifiers) {
            match *ast.node(sp) {
                Node::ImportDefaultSpecifier { local } => e.default = Some(local),
                Node::ImportNamespaceSpecifier { .. } => e.namespace = true,
                Node::ImportSpecifier { .. } => e.named.get_or_insert_with(Vec::new).push(sp),
                _ => {}
            }
        }
        let clause = &text[ast.span(s).start as usize..ast.span(source).start as usize];
        if e.named.is_none() && clause.contains('{') {
            e.named = Some(Vec::new());
        }
        out.push(e);
    }
    out
}

/// The edits importing every want (and for each, whether an existing import was updated).
pub fn plan(cx: &Ctx<'_, '_>, wants: &[Want]) -> (Vec<Edit>, Vec<bool>) {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let q = style::quote(ast, text);
    let semi = style::semi(ast, text);
    let nl = style::newline(text);
    let mut edits: Vec<Edit> = Vec::new();
    let mut updated = Vec::with_capacity(wants.len());
    // New declarations by insertion offset, in creation order.
    let mut new_decls: Vec<(u32, String, bool)> = Vec::new();
    for w in wants {
        let all_types = w.default.is_none() && w.named.iter().all(|x| x.1);
        let ex = existing(ast, text, &w.spec);
        let target = ex.iter().find(|e| {
            !e.namespace
                && (!e.type_only || all_types && w.default.is_none())
                && (w.default.is_none() || e.default.is_none() && !e.type_only)
                && (w.named.is_empty() || e.named.is_some() || !e.type_only)
        });
        if let Some(e) = target {
            update(ast, text, e, w, &mut edits);
            updated.push(true);
            continue;
        }
        updated.push(false);
        let mut s = String::from("import ");
        if all_types && !w.named.is_empty() {
            s.push_str("type ");
        }
        if let Some(d) = &w.default {
            s.push_str(d);
            if !w.named.is_empty() {
                s.push_str(", ");
            }
        }
        if !w.named.is_empty() {
            let names: Vec<&str> = w.named.iter().map(|x| x.0.as_str()).collect();
            s.push_str(&format!("{{ {} }}", names.join(", ")));
        }
        s.push_str(&format!(" from {q}{}{q}{semi}", w.spec));
        let (at, alone) = new_import_position(ast, text, &w.spec);
        new_decls.push((at, s, alone));
    }
    // Several declarations at one place: the last created comes first.
    let mut positions: Vec<u32> = new_decls.iter().map(|x| x.0).collect();
    positions.sort_unstable();
    positions.dedup();
    for at in positions {
        let here: Vec<&(u32, String, bool)> =
            new_decls.iter().filter(|x| x.0 == at).rev().collect();
        let alone = here[0].2;
        let mut t = String::new();
        if at as usize == text.len() && !text.is_empty() && !text.ends_with('\n') {
            t.push_str(nl);
        }
        for (_, s, _) in &here {
            t.push_str(s);
            t.push_str(nl);
        }
        if alone {
            t.push_str(nl);
        }
        edits.push(Edit::insert(at, t));
    }
    (edits, updated)
}

/// Adds `w`'s names to the existing import `e`.
fn update(ast: &Ast, text: &str, e: &Existing, w: &Want, edits: &mut Vec<Edit>) {
    let decl_start = ast.span(e.node).start;
    if let Some(d) = &w.default {
        // `import X, { a } from` / `import X, * as ns from`.
        let after_kw = text[decl_start as usize..]
            .find("import")
            .map_or(decl_start, |i| {
                decl_start + i as u32 + "import".len() as u32
            });
        let first = match &e.named {
            Some(_) => text[after_kw as usize..]
                .find('{')
                .map(|i| after_kw + i as u32),
            None => None,
        };
        if let Some(at) = first {
            edits.push(Edit::insert(at, format!("{d}, ")));
        }
    }
    if w.named.is_empty() {
        return;
    }
    let print = |(n, t): &(String, bool)| {
        if *t && !e.type_only {
            format!("type {n}")
        } else {
            n.clone()
        }
    };
    match &e.named {
        None => {
            // Default only: `import X, { a } from`.
            if let Some(def) = e.default {
                let names: Vec<String> = w.named.iter().map(print).collect();
                edits.push(Edit::insert(
                    ast.span(def).end,
                    format!(", {{ {} }}", names.join(", ")),
                ));
            }
        }
        Some(items) if items.is_empty() => {
            let open = text[decl_start as usize..]
                .find('{')
                .map(|i| decl_start + i as u32 + 1);
            if let Some(at) = open {
                let names: Vec<String> = w.named.iter().map(print).collect();
                let close = text[at as usize..].find('}').map_or(at, |i| at + i as u32);
                edits.push(Edit::new(at, close, format!(" {} ", names.join(", "))));
            }
        }
        Some(items) => {
            let name_of = |sp: NodeId| -> String {
                match *ast.node(sp) {
                    Node::ImportSpecifier {
                        imported, local, ..
                    } => ast.span(imported.unwrap_or(local)).text(text).to_string(),
                    _ => String::new(),
                }
            };
            let names: Vec<String> = items.iter().map(|&i| name_of(i)).collect();
            let sorted = names
                .windows(2)
                .all(|p| cmp_str(&p[0], &p[1], Case::Insensitive) != Ordering::Greater);
            let first = ast.span(items[0]).start;
            let open = text[decl_start as usize..first as usize]
                .rfind('{')
                .map_or(first, |i| decl_start + i as u32);
            let multiline = text[open as usize..first as usize].contains('\n');
            let sep = if multiline {
                format!(
                    ",{}{}",
                    style::newline(text),
                    style::line_indent(text, first)
                )
            } else {
                ", ".to_string()
            };
            let mut new: Vec<&(String, bool)> = w.named.iter().collect();
            if sorted {
                new.sort_by(|a, b| cmp_str(&a.0, &b.0, Case::Insensitive));
            }
            // Index of the existing item each new name goes before.
            let mut groups: Vec<(usize, Vec<String>)> = Vec::new();
            for n in new {
                let idx = if sorted {
                    names
                        .iter()
                        .position(|x| cmp_str(&n.0, x, Case::Insensitive) == Ordering::Less)
                        .unwrap_or(names.len())
                } else {
                    names.len()
                };
                match groups.iter_mut().find(|g| g.0 == idx) {
                    Some(g) => g.1.push(print(n)),
                    None => groups.push((idx, vec![print(n)])),
                }
            }
            for (idx, ns) in groups {
                if idx < items.len() {
                    let at = ast.span(items[idx]).start;
                    edits.push(Edit::insert(at, format!("{}{sep}", ns.join(&sep))));
                } else {
                    let at = ast.span(items[items.len() - 1]).end;
                    edits.push(Edit::insert(at, format!("{sep}{}", ns.join(&sep))));
                }
            }
        }
    }
}

/// Where a new import of `spec` goes, and whether it is the file's first import (then a
/// blank line follows it).
fn new_import_position(ast: &Ast, text: &str, spec: &str) -> (u32, bool) {
    let stmts = style::top_level(ast);
    let imports: Vec<(NodeId, Option<String>)> = stmts
        .iter()
        .filter_map(|&s| match *ast.node(s) {
            Node::ImportDecl { source, .. } => Some((
                s,
                match *ast.node(source) {
                    Node::StringLit { value } => Some(ast.atom(value).to_string()),
                    _ => None,
                },
            )),
            Node::ImportEquals { .. } => Some((s, None)),
            _ => None,
        })
        .collect();
    if let Some(&(last, _)) = imports.last() {
        let specs: Vec<Option<&str>> = imports.iter().map(|x| x.1.as_deref()).collect();
        let sorted = specs.iter().all(Option::is_some)
            && specs
                .windows(2)
                .all(|p| cmp_spec(p[0], p[1], Case::Insensitive) != Ordering::Greater);
        if sorted
            && let Some(&(before, _)) = imports
                .iter()
                .find(|x| cmp_spec(Some(spec), x.1.as_deref(), Case::Insensitive) == Ordering::Less)
        {
            return (style::line_start(text, ast.span(before).start), false);
        }
        return (style::next_line(text, ast.span(last).end), false);
    }
    // After directives, and after comments a blank line separates from the first statement.
    let mut from = 0u32;
    let mut first = None;
    for &s in &stmts {
        if matches!(ast.node(s), Node::Directive { .. }) {
            from = ast.span(s).end;
        } else {
            first = Some(ast.span(s).start);
            break;
        }
    }
    let Some(first) = first else {
        return (style::next_line(text, from).min(text.len() as u32), true);
    };
    let start = if from == 0 {
        0
    } else {
        style::next_line(text, from)
    };
    if start > first {
        return (first, true);
    }
    // After the last blank line before the first statement (comments touching the statement
    // belong to it).
    let mut at = start;
    let mut line_begin = start;
    for line in text[start as usize..first as usize].split_inclusive('\n') {
        line_begin += line.len() as u32;
        if line.ends_with('\n') && line.trim().is_empty() {
            at = line_begin;
        }
    }
    (at, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_specifiers() {
        assert_eq!(relative("/p/src/a.ts", "/p/src/b.ts", false), "./b");
        assert_eq!(relative("/p/src/x/a.ts", "/p/src/b.tsx", false), "../b");
        assert_eq!(
            relative("/p/src/a.ts", "/p/src/lib/index.ts", false),
            "./lib"
        );
        assert_eq!(
            relative("/p/src/a.ts", "/p/src/lib/x.ts", true),
            "./lib/x.js"
        );
        assert_eq!(relative("/p/a/b/c.ts", "/p/d/e.mts", true), "../../d/e.mjs");
    }
}
