//! `jsdoc/prefer-import-tag`: an inline `import('m')` type should use an `@import` (or a
//! matching `@typedef`) instead.
//!
//! Every type of a tag that may have one (not a `@typedef`-like tag with `exemptTypedefs`,
//! the default) is parsed and each `import('m')` in it, with the property path after it
//! (`import('m').A.B`), is reported, in this order of preference:
//! - a `@typedef` anywhere in the file whose type is the same import followed by a
//!   suffix of the path (`@typedef {import('m').A} T` turns `import('m').A.B` into `T.B`);
//! - an `@import` of the module that brings in the path's first name, its default (for
//!   `.default`) or a namespace;
//! - otherwise a new `@import` is added at the top of the file: `* as m` (a valid
//!   identifier made of the module name) with `outputType: "namespaced-import"` (the
//!   default), the path's first name with `"named-import"`, the default for `.default`.
//!
//! The fix rewrites the tag's type (re-printed) and the whole block. The typedef search
//! keeps its state across typedefs as the plugin's does, so a typedef that fails half-way
//! still leaves its skipped path names in front of the next one's. Once a fix replaces
//! the path, imports nested in the rest of the replaced node are not visited (the
//! plugin's walk sees the rewritten node).

use super::prelude::*;
use crate::jsdoc::{Block, Doc, Finder};
use crate::jsdoc_tags::{SMode, Structure};
use crate::jsdoc_type::{self, Id, Mode, N, PathKind, Quote, TypeTree};
use crate::jsdoc_util::each_block;
use std::collections::VecDeque;
use ts_r_lexer::{is_id_part, is_id_start};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/prefer-import-tag",
        "Prefer `@import` tags to inline `import()` statements.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<PreferImportTag>,
};

#[derive(Debug)]
struct PreferImportTag {
    enable_fixer: bool,
    exempt_typedefs: bool,
    named: bool,
}

impl FromOptions for PreferImportTag {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = opts.first().and_then(Value::as_object);
        let get = |k: &str| o.and_then(|o| o.get(k));
        Ok(PreferImportTag {
            enable_fixer: get("enableFixer").and_then(Value::as_bool).unwrap_or(true),
            exempt_typedefs: get("exemptTypedefs")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            named: get("outputType").and_then(Value::as_str) == Some("named-import"),
        })
    }
}

const RESERVED: &[&str] = &[
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "interface",
    "let",
    "new",
    "null",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

/// A valid identifier from a module name: each character that can't be in one becomes `$`,
/// its code point less 26 in base 36, and `$` (`@` is `$12$`, `-` is `$j$`, `/` is `$l$`);
/// a reserved word gets a leading `_`.
fn to_valid_identifier(name: &str) -> String {
    if RESERVED.contains(&name) {
        return format!("_{name}");
    }
    let mut out = String::with_capacity(name.len());
    for (i, c) in name.chars().enumerate() {
        let ok = if i == 0 {
            is_id_start(c)
        } else {
            is_id_part(c)
        };
        if ok {
            out.push(c);
            continue;
        }
        let mut n = (c as u32).saturating_sub(26);
        let mut digits = Vec::new();
        loop {
            digits.push(std::char::from_digit(n % 36, 36).unwrap_or('0'));
            n /= 36;
            if n == 0 {
                break;
            }
        }
        out.push('$');
        out.extend(digits.iter().rev());
        out.push('$');
    }
    out
}

/// One `@import` statement: `import [D][, * as NS | , { a, b as c }] from 'm'`.
#[derive(Debug, Default)]
struct ImportDecl {
    module: String,
    default: Option<String>,
    namespace: Option<String>,
    /// Local names of `{ ... }` imports.
    names: Vec<String>,
    braces: bool,
}

fn word_end(s: &str) -> usize {
    s.char_indices()
        .find(|&(i, c)| {
            !(if i == 0 {
                is_id_start(c)
            } else {
                is_id_part(c)
            })
        })
        .map_or(s.len(), |(i, _)| i)
}

/// Parses the text of an `@import` tag (without the `@`); `None` if it isn't one.
fn parse_import(text: &str) -> Option<ImportDecl> {
    let mut s = text.trim_start().strip_prefix("import")?;
    let mut d = ImportDecl::default();
    let ws = |s: &str| s.trim_start().to_owned();
    let mut rest = ws(s);
    if let Some(r) = rest.strip_prefix("type")
        && r.starts_with(char::is_whitespace)
    {
        rest = ws(r);
    }
    s = &rest;
    let mut first = true;
    loop {
        if let Some(r) = s.strip_prefix('*') {
            let r = r.trim_start().strip_prefix("as")?.trim_start();
            let e = word_end(r);
            if e == 0 {
                return None;
            }
            d.namespace = Some(r[..e].to_owned());
            s = r[e..].trim_start();
        } else if let Some(r) = s.strip_prefix('{') {
            d.braces = true;
            let close = r.find('}')?;
            for spec in r[..close].split(',') {
                let mut parts = spec.split_whitespace().collect::<Vec<_>>();
                if parts.first() == Some(&"type") && parts.len() > 1 {
                    parts.remove(0);
                }
                match parts.as_slice() {
                    [] => {}
                    [n] => d.names.push((*n).to_owned()),
                    [_, "as", n] => d.names.push((*n).to_owned()),
                    _ => return None,
                }
            }
            s = r[close + 1..].trim_start();
        } else if first {
            let e = word_end(s);
            if e == 0 {
                return None;
            }
            d.default = Some(s[..e].to_owned());
            s = s[e..].trim_start();
        } else {
            return None;
        }
        first = false;
        if let Some(r) = s.strip_prefix(',') {
            s = r.trim_start();
            continue;
        }
        break;
    }
    let r = s.strip_prefix("from")?.trim_start();
    let q = r.chars().next().filter(|&c| c == '\'' || c == '"')?;
    let body = &r[1..];
    let end = body.find(q)?;
    d.module = body[..end].to_owned();
    Some(d)
}

/// The text of a tag's lines from the tag on, joined with line breaks (`estreeToString`).
fn tag_text(b: &Block, tag: usize) -> String {
    let mut lines = Vec::new();
    for &id in &b.tags[tag].source {
        let t = &b.lines[id].tokens;
        lines.push(
            [
                &t.tag,
                &t.post_tag,
                &t.typ,
                &t.post_type,
                &t.name,
                &t.post_name,
                &t.description,
            ]
            .iter()
            .map(|s| s.as_str())
            .collect::<String>(),
        );
    }
    let s = lines.join("\n");
    s.trim_start().trim_start_matches('@').to_owned()
}

fn parse_type(ty: &str, mode: &str) -> Option<TypeTree> {
    match mode {
        "permissive" => jsdoc_type::try_parse(ty, false),
        m => jsdoc_type::parse(ty, Mode::from_setting(m)?, false),
    }
}

fn subtree_string(t: &TypeTree, id: Id) -> String {
    TypeTree {
        nodes: t.nodes.clone(),
        root: id,
    }
    .stringify()
}

/// A path step's name: a property's value, or the printed index of `[...]`.
fn step_name(t: &TypeTree, right: Id) -> String {
    match &t.nodes[right] {
        N::IndexedAccessIndex(inner) => subtree_string(t, *inner),
        N::Property { value, .. } => value.clone(),
        N::Name(v) => v.clone(),
        _ => subtree_string(t, right),
    }
}

struct Typedef {
    name: String,
    tree: Option<TypeTree>,
}

/// The file's `@typedef`-like tags and `@import` statements.
struct FileInfo {
    typedefs: Vec<Typedef>,
    imports: Vec<ImportDecl>,
}

fn file_info(f: &Finder<'_>, structure: &Structure) -> FileInfo {
    let mode = match f.settings.mode.as_str() {
        "permissive" => "typescript",
        m => m,
    };
    let mut info = FileInfo {
        typedefs: Vec::new(),
        imports: Vec::new(),
    };
    for c in &f.ast.comments {
        let text = c.span.text(f.src);
        // `/^\*(?!\*)/` on the value: `/**` but not `/***`.
        if !(text.starts_with("/**") && !text.starts_with("/***") && text.len() >= 4) {
            continue;
        }
        let b = Block::parse(text);
        for (i, t) in b.tags.iter().enumerate() {
            if structure.is_name_or_namepath_defining(&t.tag) {
                info.typedefs.push(Typedef {
                    name: t.name.clone(),
                    tree: parse_type(&t.typ, mode),
                });
            }
            if t.tag == "import"
                && let Some(d) = parse_import(&tag_text(&b, i))
            {
                info.imports.push(d);
            }
        }
    }
    info
}

/// One `import(...)` found in a tag's type, with the path that follows it (outermost step
/// first, as the plugin collects it).
struct Found {
    import: Id,
    module: String,
    path: Vec<String>,
    nodes: Vec<Id>,
    quotes: VecDeque<Quote>,
    kinds: VecDeque<PathKind>,
}

impl Found {
    /// `getFixer`: replaces the outermost path node (or the import) with `name` followed
    /// by `extra` steps (outermost first). Returns the replaced node.
    fn rewrite(&mut self, t: &mut TypeTree, name: &str, extra: &[String]) -> Id {
        let x = self.nodes.first().copied().unwrap_or(self.import);
        let name = to_valid_identifier(name);
        let mut cur = x;
        for seg in extra {
            let right = t.nodes.len();
            t.nodes.push(N::Property {
                value: seg.clone(),
                quote: self.quotes.pop_front().unwrap_or(Quote::None),
            });
            let left = t.nodes.len();
            t.nodes.push(N::Name(String::new()));
            t.nodes[cur] = N::NamePath {
                left,
                right,
                kind: self.kinds.pop_front().unwrap_or(PathKind::Property),
            };
            cur = left;
        }
        t.nodes[cur] = N::Name(name);
        x
    }
}

impl PreferImportTag {
    /// `findMatchingTypedef`: the first typedef whose type is this import plus a run of
    /// the path, with the extra and unused path names (state shared across typedefs).
    fn matching_typedef<'i>(
        &self,
        found: &Found,
        typedefs: &'i [Typedef],
    ) -> Option<(&'i Typedef, Vec<String>)> {
        if !self.exempt_typedefs {
            return None;
        }
        let mut pth: VecDeque<String> = found.path.iter().cloned().collect();
        let mut extra: Vec<String> = Vec::new();
        let mut unused_set = false;
        for td in typedefs {
            let Some(tt) = &td.tree else { continue };
            let mut node = Some(tt.root);
            let mut matched: Option<bool> = None;
            let mut guard = 0;
            while let Some(n) = node
                && let N::NamePath { left, right, .. } = &tt.nodes[n]
            {
                guard += 1;
                if guard > 10_000 {
                    break;
                }
                let Some(seg) = pth.pop_front() else {
                    matched = Some(false);
                    break;
                };
                if step_name(tt, *right) != seg {
                    if matched == Some(true) {
                        break;
                    }
                    extra.push(seg);
                    matched = Some(false);
                    continue;
                }
                matched = Some(true);
                unused_set = true;
                node = Some(*left);
            }
            let is_import = node.is_some_and(|n| match &tt.nodes[n] {
                N::Import(el) => import_module(tt, *el).as_deref() == Some(found.module.as_str()),
                _ => false,
            });
            if matched == Some(true) && is_import {
                let unused: Vec<String> = if unused_set {
                    pth.iter().cloned().collect()
                } else {
                    Vec::new()
                };
                let mut steps = extra;
                if let Some(last) = unused.last() {
                    steps.push(last.clone());
                }
                steps.extend(unused.iter().take(unused.len().saturating_sub(1)).cloned());
                return Some((td, steps));
            }
        }
        None
    }
}

fn import_module(t: &TypeTree, el: Id) -> Option<String> {
    match &t.nodes[el] {
        N::Str { value, .. } => Some(value.clone()),
        _ => None,
    }
}

/// Where the plugin (ESLint 10) puts a new `@import` line: after the first line break and
/// its indent when the file has code after such a start, else at the very start (taking
/// the place of a leading line break).
fn import_insertion(src: &str, has_code: bool, text: &str) -> (Span, String) {
    if has_code && src.starts_with('\n') {
        let lead = 1 + src[1..].len() - src[1..].trim_start_matches([' ', '\t']).len();
        let at = lead as u32;
        return (Span::new(at, at), format!("{text}\n{}", &src[1..lead]));
    }
    if src.starts_with('\n') {
        (Span::new(0, 1), format!("{text}\n"))
    } else {
        (Span::new(0, 0), format!("{text}\n"))
    }
}

impl Rule for PreferImportTag {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let settings_value = cx
            .env()
            .settings
            .get("jsdoc")
            .cloned()
            .unwrap_or(Value::Null);
        let has_code = match cx.file.ast.node(cx.file.ast.root) {
            Node::Program { stmts } => !cx.file.ast.list(*stmts).is_empty(),
            _ => false,
        };
        let src = cx.file.src;
        let mut info: Option<FileInfo> = None;
        each_block(cx, |cx, f, doc| {
            let mode = f.settings.mode.clone();
            let structure = Structure::new(SMode::from_setting(&mode), &settings_value);
            let info = info.get_or_insert_with(|| file_info(f, &structure));
            for i in 0..doc.block.tags.len() {
                let t = &doc.block.tags[i];
                if !structure.might_type(&t.tag) || t.typ.is_empty() {
                    continue;
                }
                if self.exempt_typedefs && structure.is_name_or_namepath_defining(&t.tag) {
                    continue;
                }
                self.check_tag(cx, f, doc, i, info, &mode, src, has_code);
            }
        });
    }
}

impl PreferImportTag {
    #[allow(clippy::too_many_arguments)]
    fn check_tag(
        &self,
        cx: &mut Ctx<'_, '_>,
        f: &Finder<'_>,
        doc: &mut Doc,
        tag: usize,
        info: &FileInfo,
        mode: &str,
        src: &str,
        has_code: bool,
    ) {
        let Some(mut tree) = parse_type(&doc.block.tags[tag].typ, mode) else {
            return;
        };
        let parents: Vec<Option<Id>> = {
            let mut p = vec![None; tree.nodes.len()];
            for (id, parent, _) in tree.walk() {
                p[id] = parent;
            }
            p
        };
        let mut stack: Vec<(Id, Option<Id>)> = vec![(tree.root, None)];
        let mut steps = 0;
        while let Some((id, _)) = stack.pop() {
            steps += 1;
            if steps > 100_000 {
                break;
            }
            let children = tree.children(id);
            for (c, _) in children.into_iter().rev() {
                stack.push((c, Some(id)));
            }
            let N::Import(el) = &tree.nodes[id] else {
                continue;
            };
            let Some(module) = import_module(&tree, *el) else {
                continue;
            };
            let mut found = Found {
                import: id,
                module,
                path: Vec::new(),
                nodes: Vec::new(),
                quotes: VecDeque::new(),
                kinds: VecDeque::new(),
            };
            let mut cur = id;
            while let Some(p) = parents.get(cur).copied().flatten() {
                let N::NamePath { right, kind, .. } = &tree.nodes[p] else {
                    break;
                };
                found.path.push(step_name(&tree, *right));
                found.nodes.push(p);
                found.kinds.push_back(*kind);
                found.quotes.push_back(match &tree.nodes[*right] {
                    N::Property { quote, .. } => *quote,
                    _ => Quote::None,
                });
                cur = p;
            }
            // Collected innermost first; the plugin's lists are outermost first.
            found.path.reverse();
            found.nodes.reverse();
            found.kinds.make_contiguous().reverse();
            found.quotes.make_contiguous().reverse();
            if let Some(replaced) =
                self.report_import(cx, f, doc, tag, &mut tree, found, info, src, has_code)
            {
                stack.retain(|&(_, parent)| parent != Some(replaced));
            }
        }
    }

    /// Reports one import; with the fixer, returns the node its fix replaced.
    #[allow(clippy::too_many_arguments)]
    fn report_import(
        &self,
        cx: &mut Ctx<'_, '_>,
        f: &Finder<'_>,
        doc: &mut Doc,
        tag: usize,
        tree: &mut TypeTree,
        mut found: Found,
        info: &FileInfo,
        src: &str,
        has_code: bool,
    ) -> Option<Id> {
        const PREFER: &str = "Inline `import()` found; prefer `@import`";
        let path = found.path.clone();
        let last = path.last().cloned();
        let module = found.module.clone();
        // (name, extra steps, message, new @import text)
        let plan: (String, Vec<String>, &str, Option<String>) =
            if let Some((td, steps)) = self.matching_typedef(&found, &info.typedefs) {
                (
                    td.name.clone(),
                    steps,
                    "Inline `import()` found; using `@typedef`",
                    None,
                )
            } else if let Some(p) =
                Self::matching_import(&info.imports, &module, last.as_deref(), &path)
            {
                (p.0, p.1, PREFER, None)
            } else if path.is_empty() {
                let text = format!(
                    "/** @import * as {} from '{module}'; */",
                    to_valid_identifier(&module)
                );
                (module.clone(), Vec::new(), PREFER, Some(text))
            } else if last.as_deref() == Some("default") {
                let text = format!("/** @import {module} from '{module}'; */");
                (module.clone(), Vec::new(), PREFER, Some(text))
            } else if self.named {
                let l = last.clone().unwrap_or_default();
                let text = format!(
                    "/** @import {{ {} }} from '{module}'; */",
                    to_valid_identifier(&l)
                );
                (l, path[..path.len() - 1].to_vec(), PREFER, Some(text))
            } else {
                let text = format!(
                    "/** @import * as {} from '{module}'; */",
                    to_valid_identifier(&module)
                );
                (module.clone(), path.clone(), PREFER, Some(text))
            };
        let (name, extra, msg, insert) = plan;
        let rel = doc.block.tag_line(tag);
        let (span, loc) = doc.report_at(f, rel, None);
        if !self.enable_fixer {
            cx.report(span, msg).loc(loc);
            return None;
        }
        let replaced = found.rewrite(tree, &name, &extra);
        let printed = format!("{{{}}}", tree.stringify());
        let src_lines = doc.block.tags[tag].source.clone();
        if let Some(&line) = src_lines
            .iter()
            .find(|&&l| !doc.block.lines[l].tokens.typ.is_empty())
        {
            doc.block.lines[line].tokens.typ = printed;
        }
        let mut fix = doc.fix();
        if let Some(text) = insert {
            let (at, t) = import_insertion(src, has_code, &text);
            fix = fix.and(at, t);
        }
        cx.report(span, msg).loc(loc).fix(fix);
        Some(replaced)
    }

    /// `findMatchingImport` and what to do with it: the name to use and the extra steps.
    fn matching_import(
        imports: &[ImportDecl],
        module: &str,
        last: Option<&str>,
        path: &[String],
    ) -> Option<(String, Vec<String>)> {
        for d in imports {
            let namespace_kind = d.namespace.is_some();
            let ns_default = if namespace_kind {
                d.default.as_deref()
            } else {
                None
            };
            let named_module = (!namespace_kind).then_some(d.module.as_str());
            let names: &[String] = if namespace_kind { &[] } else { &d.names };
            let hit = (ns_default.is_some() && d.module == module)
                || (named_module == Some(module)
                    && (last.is_some_and(|l| names.iter().any(|n| n == l))
                        || last == Some("default")))
                || (namespace_kind && d.module == module);
            if !hit {
                continue;
            }
            if names.is_empty()
                && named_module.is_some()
                && let Some(def) = d.default.as_deref()
            {
                return Some((def.to_owned(), Vec::new()));
            }
            if let Some(l) = last
                && names.iter().any(|n| n == l)
            {
                return Some((l.to_owned(), path[..path.len() - 1].to_vec()));
            }
            if let Some(ns) = &d.namespace {
                return Some((ns.clone(), path.to_vec()));
            }
            // A match the plugin then does nothing with: fall through to a new import.
            return None;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_identifiers() {
        assert_eq!(to_valid_identifier("eslint"), "eslint");
        assert_eq!(to_valid_identifier("node:zlib"), "node$w$zlib");
        assert_eq!(
            to_valid_identifier("@typescript-eslint/utils"),
            "$12$typescript$j$eslint$l$utils"
        );
        assert_eq!(to_valid_identifier("default"), "_default");
    }

    #[test]
    fn import_statements() {
        let d = parse_import("import LinterDef2,\n* as LinterDef3 from \"eslint\"").unwrap();
        assert_eq!(d.default.as_deref(), Some("LinterDef2"));
        assert_eq!(d.namespace.as_deref(), Some("LinterDef3"));
        assert_eq!(d.module, "eslint");
        let d = parse_import("import {\nESLint, a as b\n} from 'eslint'").unwrap();
        assert_eq!(d.names, ["ESLint", "b"]);
        assert!(parse_import("import from").is_none());
        assert!(parse_import("import {").is_none());
    }
}
