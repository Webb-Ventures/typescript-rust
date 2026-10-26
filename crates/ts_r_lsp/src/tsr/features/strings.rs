//! Completions inside string literals: the literal types the position expects, property
//! names (`o["|"]`, `"|" in o`, quoted object literal keys, `T["|"]`), and module
//! specifiers (`import "./|"`, `require("|")`, `/// <reference path="|"`).
//!
//! The rules were learned from tsserver's behaviour on its fourslash tests
//! (`completionForStringLiteral*`); see the crate's AGENTS.md, "String completions".

use super::Ctx;
use super::complete::ImportStyle;
use std::collections::BTreeSet;
use ts_r_ast::{Ast, BinaryOp, Node, NodeId};
use ts_r_binder::SymbolFlags;
use ts_r_checker::members::SigKind;
use ts_r_checker::{Ck, Loc};
use ts_r_diagnostics::FileId;
use ts_r_resolver::ResolverOptions;
use ts_r_types::{TypeId, TypeKind};

/// LSP CompletionItemKind values used here.
pub(super) const KIND_PROPERTY: u32 = 10;
const KIND_VALUE: u32 = 12;
const KIND_MODULE: u32 = 9;
const KIND_FILE: u32 = 17;
const KIND_FOLDER: u32 = 19;

/// Directory entries read at most per listing.
const MAX_ENTRIES: usize = 2000;
/// Ancestor directories visited at most.
const MAX_ANCESTORS: usize = 64;

/// One string completion: label and LSP kind.
pub(super) type Item = (String, u32);

/// The string literal (or no-substitution template) holding `offset`, if any: the cursor is
/// after its opening quote and before its closing one (an unterminated literal runs to its
/// end).
pub(super) fn literal_at(ck: &Ck<'_, '_>, file: FileId, offset: u32) -> Option<NodeId> {
    let c = ck.c;
    let ast = c.ast(file);
    let text = c.file(file).text.as_str();
    let mut n = super::locate::node_at(ast, offset)?;
    if matches!(ast.node(n), Node::TemplateElement { .. }) {
        n = c.parent(Loc::new(file, n))?.node;
    }
    let is_string = match ast.node(n) {
        Node::StringLit { .. } | Node::Directive { .. } => true,
        Node::TemplateLit { parts } => ast.list(*parts).len() <= 1,
        _ => false,
    };
    if !is_string {
        return None;
    }
    let sp = ast.span(n);
    let mut lit = &text[sp.start as usize..sp.end as usize];
    if matches!(ast.node(n), Node::Directive { .. }) {
        // A directive's span holds its `;`.
        lit = lit.trim_end().trim_end_matches(';').trim_end();
    }
    let end = sp.start + lit.len() as u32;
    if lit.is_empty() || offset <= sp.start || offset > end {
        return None;
    }
    let quote = lit.as_bytes()[0];
    let closed = lit.len() >= 2 && lit.as_bytes()[lit.len() - 1] == quote && !ends_escaped(lit);
    if offset == end && closed {
        return None;
    }
    Some(n)
}

/// Whether the last character of `lit` is escaped by a backslash.
fn ends_escaped(lit: &str) -> bool {
    let b = lit.as_bytes();
    let mut i = b.len().saturating_sub(1);
    let mut slashes = 0;
    while i > 1 && b[i - 1] == b'\\' {
        slashes += 1;
        i -= 1;
    }
    slashes % 2 == 1
}

/// A completion label as it would be written inside the literal: backslashes, line breaks
/// and double quotes are escaped (tsc 7 escapes `"` in single-quoted strings too), and
/// backticks inside a template.
pub(super) fn escape_label(value: &str, template: bool) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '"' if !template => out.push_str("\\\""),
            '`' if template => out.push_str("\\`"),
            c => out.push(c),
        }
    }
    out
}

/// Whether the literal `lit` is a template (backtick) literal.
pub(super) fn is_template(ast: &Ast, lit: NodeId) -> bool {
    matches!(ast.node(lit), Node::TemplateLit { .. })
}

/// The completions inside the literal `lit`. `None` when the position offers nothing.
pub(super) fn string_items(
    cx: &Ctx<'_, '_>,
    ck: &mut Ck<'_, '_>,
    lit: NodeId,
    offset: u32,
) -> Option<Vec<Item>> {
    let c = ck.c;
    let file = cx.file;
    let ast = c.ast(file);
    let text = c.file(file).text.as_str();
    let start = ast.span(lit).start + 1;
    let fragment = text.get(start as usize..offset as usize).unwrap_or("");
    let (parent, child) = parent_skipping_parens(ck, Loc::new(file, lit))?;
    let pl = Loc::new(file, parent);
    match *ast.node(parent) {
        Node::ImportDecl { source, .. } if source == child => module_items(cx, ck, fragment),
        Node::ExportNamed {
            source: Some(s), ..
        }
        | Node::ExportAll { source: s, .. }
            if s == child =>
        {
            module_items(cx, ck, fragment)
        }
        Node::ExternalModuleRef { .. } => module_items(cx, ck, fragment),
        Node::ImportCall { arg, .. } if arg == child => module_items(cx, ck, fragment),
        Node::ImportType { arg, .. } if arg == child => module_items(cx, ck, fragment),
        Node::Call { callee, args, .. }
            if ast.list(args).first() == Some(&child)
                && matches!(ast.node(callee), Node::Ident { name } if ast.atom(*name) == "require") =>
        {
            module_items(cx, ck, fragment)
        }
        Node::Index { obj, index, .. } if index == child => {
            let t = ck.check_expr(Loc::new(file, obj));
            Some(property_items(ck, t, &[]))
        }
        Node::Binary {
            left,
            op: BinaryOp::In,
            right,
        } if left == child => {
            let t = ck.check_expr(Loc::new(file, right));
            Some(property_items(ck, t, &[]))
        }
        Node::Binary {
            left,
            op: BinaryOp::Eq | BinaryOp::NotEq | BinaryOp::StrictEq | BinaryOp::StrictNotEq,
            right,
        } => {
            let other = if left == child { right } else { left };
            let t = ck.check_expr(Loc::new(file, other));
            Some(literal_items(ck, t, &[]))
        }
        Node::Property { key, .. } if key == child => {
            let obj = c.parent(pl)?;
            let t = ck.contextual_type(obj)?;
            let present = written_keys(ast, obj.node);
            Some(property_items(ck, t, &present))
        }
        Node::SwitchCase { test: Some(t), .. } if t == child => {
            let sw = c.parent(pl)?;
            let Node::Switch { disc, cases } = *ast.node(sw.node) else {
                return None;
            };
            let used: Vec<String> = ast
                .list(cases)
                .iter()
                .filter_map(|&k| match *ast.node(k) {
                    Node::SwitchCase { test: Some(t), .. } if t != child => string_value(ast, t),
                    _ => None,
                })
                .collect();
            let t = ck.check_expr(Loc::new(file, disc));
            Some(literal_items(ck, t, &used))
        }
        Node::Call { args, .. } | Node::New { args, .. } if ast.list(args).contains(&child) => {
            let i = ast.list(args).iter().position(|&a| a == child).unwrap_or(0);
            let from_sigs = call_arg_items(ck, pl, i, ast.list(args).len());
            if from_sigs.is_empty() {
                Some(contextual_items(ck, Loc::new(file, lit)))
            } else {
                Some(from_sigs)
            }
        }
        _ if is_type_position(ast.node(parent), child) => type_position_items(ck, pl, child),
        _ => Some(contextual_items(ck, Loc::new(file, lit))),
    }
}

/// The parent of `l` above any parentheses, and the child of it on the way up.
fn parent_skipping_parens(ck: &Ck<'_, '_>, l: Loc) -> Option<(NodeId, NodeId)> {
    let c = ck.c;
    let mut child = l.node;
    let mut p = c.parent(l)?;
    for _ in 0..1000 {
        if !matches!(
            c.ast(l.file).node(p.node),
            Node::Paren { .. } | Node::ParenType { .. }
        ) {
            return Some((p.node, child));
        }
        child = p.node;
        p = c.parent(p)?;
    }
    None
}

fn string_value(ast: &Ast, n: NodeId) -> Option<String> {
    match ast.node(n) {
        Node::StringLit { value } => Some(ast.atom(*value).to_string()),
        _ => None,
    }
}

/// Whether `child` of `parent` is a type node (a literal type `"x"`).
fn is_type_position(parent: &Node, child: NodeId) -> bool {
    match *parent {
        Node::UnionType { .. }
        | Node::TypeArgs { .. }
        | Node::IndexedAccessType { .. }
        | Node::IntersectionType { .. }
        | Node::ArrayType { .. }
        | Node::TupleType { .. } => true,
        Node::VarDeclarator { type_ann, .. }
        | Node::PropertySignature { type_ann, .. }
        | Node::PropertyDef { type_ann, .. }
        | Node::Param { type_ann, .. } => type_ann == Some(child),
        Node::TypeAlias { ty, .. } => ty == child,
        _ => false,
    }
}

/// A literal type `"|"`: keys in `T["|"]`, the type parameter's constraint in `G<"|">`,
/// other members' types in a union.
fn type_position_items(ck: &mut Ck<'_, '_>, parent: Loc, child: NodeId) -> Option<Vec<Item>> {
    let c = ck.c;
    let ast = c.ast(parent.file);
    match *ast.node(parent.node) {
        Node::IndexedAccessType { obj, index } if index == child => {
            let t = ck.type_from_node(Loc::new(parent.file, obj));
            Some(property_items(ck, t, &[]))
        }
        Node::UnionType { types } => {
            let used: Vec<String> = ast
                .list(types)
                .iter()
                .filter(|&&t| t != child)
                .filter_map(|&t| match ast.node(t) {
                    Node::StringLit { value } => Some(ast.atom(*value).to_string()),
                    _ => None,
                })
                .collect();
            let (gp, me) = parent_skipping_parens(ck, parent)?;
            let mut items = type_position_items(ck, Loc::new(parent.file, gp), me)?;
            items.retain(|(n, _)| !used.contains(n));
            Some(items)
        }
        Node::TypeArgs { args } => {
            let i = ast.list(args).iter().position(|&a| a == child)?;
            let owner = c.parent(parent)?;
            let params = match *ast.node(owner.node) {
                Node::TypeRef { name, .. } => {
                    let s = super::locate::entity_symbol(ck, Loc::new(parent.file, name))?;
                    let s = c.resolve_alias(s).unwrap_or(s);
                    ck.local_type_params(s)
                }
                Node::ExprWithTypeArgs { expr, .. } => {
                    let s = super::locate::entity_symbol(ck, Loc::new(parent.file, expr))?;
                    let s = c.resolve_alias(s).unwrap_or(s);
                    ck.local_type_params(s)
                }
                Node::Call { callee, .. } | Node::New { callee, .. } => {
                    let kind = if matches!(ast.node(owner.node), Node::New { .. }) {
                        SigKind::Construct
                    } else {
                        SigKind::Call
                    };
                    let t = ck.check_expr(Loc::new(parent.file, callee));
                    let sigs = ck.signatures_of(t, kind);
                    let mut out = Vec::new();
                    for s in sigs {
                        let tps = ck
                            .store()
                            .list_items(ck.store().sig(s).type_params)
                            .to_vec();
                        if let Some(&tp) = tps.get(i) {
                            out.push(tp);
                        }
                    }
                    let mut items = Vec::new();
                    for tp in out {
                        if let Some(k) = ck.type_param_constraint(tp) {
                            for it in literal_items(ck, k, &[]) {
                                if !items.contains(&it) {
                                    items.push(it);
                                }
                            }
                        }
                    }
                    return Some(items);
                }
                _ => return None,
            };
            let tp = *params.get(i)?;
            let k = ck.type_param_constraint(tp)?;
            Some(literal_items(ck, k, &[]))
        }
        _ => None,
    }
}

/// Keys written in the object literal `obj`.
fn written_keys(ast: &Ast, obj: NodeId) -> Vec<String> {
    let Node::ObjectLit { props } = *ast.node(obj) else {
        return Vec::new();
    };
    ast.list(props)
        .iter()
        .filter_map(|&p| {
            let key = match *ast.node(p) {
                Node::Property { key, .. } => key,
                Node::ShorthandProp { name, .. } => name,
                Node::MethodDef { key, .. } => key,
                _ => return None,
            };
            key_text(ast, key)
        })
        .collect()
}

/// Text of a property key: a name, or a string or number literal.
fn key_text(ast: &Ast, key: NodeId) -> Option<String> {
    match ast.node(key) {
        Node::Ident { name } | Node::PrivateName { name } => Some(ast.atom(*name).to_string()),
        Node::StringLit { value } => Some(ast.atom(*value).to_string()),
        _ => None,
    }
}

/// String literal types the literal's contextual type allows.
fn contextual_items(ck: &mut Ck<'_, '_>, lit: Loc) -> Vec<Item> {
    match ck.contextual_type(lit) {
        Some(t) => literal_items(ck, t, &[]),
        None => Vec::new(),
    }
}

/// String literal types of the parameter at `index` of every signature the call could use.
fn call_arg_items(ck: &mut Ck<'_, '_>, call: Loc, index: usize, argc: usize) -> Vec<Item> {
    let c = ck.c;
    let ast = c.ast(call.file);
    let (callee, kind) = match *ast.node(call.node) {
        Node::Call { callee, .. } => (callee, SigKind::Call),
        Node::New { callee, .. } => (callee, SigKind::Construct),
        _ => return Vec::new(),
    };
    let t = ck.check_expr(Loc::new(call.file, callee));
    let a = ck.apparent_type(t);
    let mut sigs = ck.signatures_of(a, kind);
    if let Some(r) = ck.resolved_signature(call) {
        sigs.insert(0, r);
    }
    let mut out: Vec<Item> = Vec::new();
    for s in sigs {
        let d = ck.store().sig(s);
        let rest = d.has_rest();
        let params: Vec<TypeId> = d.params.iter().map(|p| p.ty).collect();
        if !rest && argc > params.len() {
            continue;
        }
        let ty = if index < params.len() && !(rest && index + 1 >= params.len()) {
            params[index]
        } else if rest && !params.is_empty() {
            let arr = params[params.len() - 1];
            match ck.element_type_of_array_like(arr) {
                Some(e) => e,
                None => continue,
            }
        } else {
            continue;
        };
        for it in literal_items(ck, ty, &[]) {
            if !out.contains(&it) {
                out.push(it);
            }
        }
    }
    out
}

/// The string literal members of `t` (through type parameter constraints), minus `skip`.
fn literal_items(ck: &mut Ck<'_, '_>, t: TypeId, skip: &[String]) -> Vec<Item> {
    let mut names = Vec::new();
    collect_literals(ck, t, &mut names, 0);
    let mut out: Vec<Item> = Vec::new();
    for n in names {
        if !skip.contains(&n) && !out.iter().any(|(m, _)| *m == n) {
            out.push((n, KIND_VALUE));
        }
    }
    out
}

fn collect_literals(ck: &mut Ck<'_, '_>, t: TypeId, out: &mut Vec<String>, depth: u32) {
    if depth > 8 {
        return;
    }
    match ck.type_kind(t) {
        TypeKind::StringLiteral(s) => out.push(ck.store().str(s).to_string()),
        TypeKind::Union { .. } => {
            for m in ck.union_members(t) {
                collect_literals(ck, m, out, depth + 1);
            }
        }
        TypeKind::TypeParameter(_)
        | TypeKind::Index(_)
        | TypeKind::IndexedAccess { .. }
        | TypeKind::Conditional(_)
        | TypeKind::Substitution { .. } => {
            let b = ck.base_constraint(t);
            if b != t {
                collect_literals(ck, b, out, depth + 1);
            }
        }
        _ => {}
    }
}

/// Property names of `t` (every member's, for a union), minus `skip` and `#private` names.
fn property_items(ck: &mut Ck<'_, '_>, t: TypeId, skip: &[String]) -> Vec<Item> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let members = if matches!(ck.type_kind(t), TypeKind::Union { .. }) {
        ck.union_members(t)
    } else {
        vec![t]
    };
    for m in members {
        let a = ck.apparent_type(m);
        for (n, s) in ck.properties_of(a) {
            let name = ck.store().str(n).to_string();
            if name.starts_with("__@") || name.starts_with('#') || skip.contains(&name) {
                continue;
            }
            if seen.insert(name.clone()) {
                let kind = if ck.c.sym_flags(s).intersects(SymbolFlags::METHOD) {
                    2
                } else {
                    KIND_PROPERTY
                };
                out.push((name, kind));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Module specifiers

/// How names of files are written in the completed specifier.
#[derive(Clone, Copy, PartialEq)]
enum Ending {
    /// `./a` for `a.ts` (bundler and classic Node resolution).
    Minimal,
    /// `./a.js` for `a.ts` (ES modules under node16/nodenext, or a file that already
    /// writes extensions).
    Js,
    /// Names as they are on disk (`/// <reference path>`).
    Filename,
}

struct Listing<'a> {
    allow_js: bool,
    json: bool,
    jsx_preserve: bool,
    ending: Ending,
    /// Extensions ambient `declare module "*.css"` patterns make importable.
    extra_exts: &'a [String],
}

impl Listing<'_> {
    /// The completion name of the file `name`, or `None` when it cannot be imported.
    fn file_name(&self, name: &str) -> Option<String> {
        const TS: &[&str] = &[".d.ts", ".d.mts", ".d.cts", ".ts", ".tsx", ".mts", ".cts"];
        const JS: &[&str] = &[".js", ".jsx", ".mjs", ".cjs"];
        let ext = TS
            .iter()
            .chain(JS.iter())
            .find(|e| name.len() > e.len() && name.ends_with(*e))
            .copied();
        let ext = match ext {
            Some(e) if JS.contains(&e) && !self.allow_js => return None,
            Some(e) => e,
            None if self.json && name.ends_with(".json") => return Some(name.to_string()),
            None if self.extra_exts.iter().any(|e| name.ends_with(e.as_str())) => {
                return Some(name.to_string());
            }
            None => return None,
        };
        if self.ending == Ending::Filename {
            return Some(name.to_string());
        }
        let stem = &name[..name.len() - ext.len()];
        let out = match (self.ending, ext) {
            (Ending::Minimal, ".d.ts" | ".ts" | ".tsx" | ".js" | ".jsx") => stem.to_string(),
            (_, ".d.ts" | ".ts") => format!("{stem}.js"),
            (_, ".tsx") if self.jsx_preserve => format!("{stem}.jsx"),
            (_, ".tsx") => format!("{stem}.js"),
            (_, ".d.mts" | ".mts") => format!("{stem}.mjs"),
            (_, ".d.cts" | ".cts") => format!("{stem}.cjs"),
            _ => name.to_string(),
        };
        Some(out)
    }
}

/// Specifiers starting `./` or `../` are relative to the importing file.
fn is_relative(spec: &str) -> bool {
    spec.starts_with("./") || spec.starts_with("../")
}

fn dir_of(p: &str) -> &str {
    match p.rfind('/') {
        Some(0) => "/",
        Some(i) => &p[..i],
        None => "",
    }
}

/// Joins `dir` and a relative `rel` (which may hold `.` and `..` segments).
fn join(dir: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = if rel.starts_with('/') {
        Vec::new()
    } else {
        dir.split('/').filter(|s| !s.is_empty()).collect()
    };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    format!("/{}", parts.join("/"))
}

/// Entries of the directory `dir`: `(name, is_dir)`, sorted.
fn read_dir(dir: &str) -> Vec<(String, bool)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, bool)> = rd
        .take(MAX_ENTRIES)
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let is_dir = std::fs::metadata(e.path()).ok()?.is_dir();
            Some((name, is_dir))
        })
        .collect();
    out.sort();
    out
}

/// Files and folders in the directory part of `fragment` under `base` (the last segment is
/// left for the client to filter). `exclude` is the importing file.
fn directory_items(
    base: &str,
    fragment: &str,
    listing: &Listing<'_>,
    exclude: Option<&str>,
    out: &mut Vec<Item>,
) {
    let dir_part = if fragment.ends_with('/') {
        fragment
    } else {
        match fragment.rfind('/') {
            Some(i) => &fragment[..=i],
            None => "",
        }
    };
    let dir = join(base, dir_part);
    for (name, is_dir) in read_dir(&dir) {
        if is_dir {
            if name != "@types" {
                push(out, name, KIND_FOLDER);
            }
            continue;
        }
        let full = format!("{}/{}", dir.trim_end_matches('/'), name);
        if exclude == Some(full.as_str()) {
            continue;
        }
        if let Some(n) = listing.file_name(&name) {
            push(out, n, KIND_FILE);
        }
    }
}

fn push(out: &mut Vec<Item>, name: String, kind: u32) {
    if !out.iter().any(|(n, _)| *n == name) {
        out.push((name, kind));
    }
}

/// Ancestor directories of `dir`, `dir` first.
fn ancestors(dir: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut d = dir.to_string();
    for _ in 0..MAX_ANCESTORS {
        out.push(d.clone());
        let p = dir_of(&d).to_string();
        if p == d || p.is_empty() {
            break;
        }
        d = p;
    }
    out
}

fn listing_for<'a>(
    cx: &Ctx<'_, '_>,
    ck: &Ck<'_, '_>,
    ending: Ending,
    extra: &'a [String],
) -> Listing<'a> {
    let o = &cx.program.options;
    let e = o.effective();
    let mut ending = ending;
    if ending == Ending::Minimal {
        let mut style = ImportStyle::of(ck.c, cx.file);
        if super::complete::needs_js_extension(o, &ck.c.file(cx.file).path) {
            style.js_ext = true;
        }
        if style.js_ext {
            ending = Ending::Js;
        }
    }
    Listing {
        allow_js: e.allow_js(),
        json: e.resolve_json_module(),
        jsx_preserve: o.jsx() == Some(ts_r_config::JsxEmit::Preserve),
        ending,
        extra_exts: extra,
    }
}

/// Extensions of ambient `declare module "*.ext"` patterns.
fn ambient_exts(ck: &Ck<'_, '_>) -> Vec<String> {
    ck.c.link
        .ambient_modules
        .iter()
        .filter_map(|(n, _)| {
            let n: &str = n;
            let e = n.strip_prefix('*')?;
            (e.starts_with('.') && !e.contains('/')).then(|| e.to_string())
        })
        .collect()
}

/// Completions for a module specifier being typed (`fragment`).
fn module_items(cx: &Ctx<'_, '_>, ck: &Ck<'_, '_>, fragment: &str) -> Option<Vec<Item>> {
    let c = ck.c;
    let path = c.file(cx.file).path.clone();
    let script_dir = dir_of(&path).to_string();
    let extra = ambient_exts(ck);
    let listing = listing_for(cx, ck, Ending::Minimal, &extra);
    let mut out = Vec::new();
    let opts = &cx.program.options;
    if is_relative(fragment) || (opts.paths().is_none() && fragment.starts_with('/')) {
        directory_items(&script_dir, fragment, &listing, Some(&path), &mut out);
        return Some(out);
    }
    let frag_dir = fragment.contains('/').then(|| match fragment.rfind('/') {
        Some(i) => &fragment[..i],
        None => fragment,
    });
    // `paths` keys without wildcards.
    if let Some(paths) = opts.paths() {
        for (key, _) in &paths.0 {
            if !key.contains('*') && key.starts_with(fragment) {
                push(&mut out, key.clone(), KIND_MODULE);
            }
        }
    }
    // Ambient modules (`declare module "x"`).
    for (name, _) in &c.link.ambient_modules {
        let name: &str = name;
        if name.contains('*') || !name.starts_with(fragment) {
            continue;
        }
        let shown = match frag_dir {
            Some(d) => name.strip_prefix(&format!("{d}/")).unwrap_or(name),
            None => name,
        };
        push(&mut out, shown.to_string(), KIND_MODULE);
    }
    typings_items(cx, &script_dir, frag_dir, &listing, &mut out);
    // Packages: the dependencies package.json files name, or else what node_modules holds.
    let mut found = false;
    if frag_dir.is_none() {
        for name in dependencies(&script_dir) {
            if !out.iter().any(|(n, _)| *n == name) {
                found = true;
                out.push((name, KIND_MODULE));
            }
        }
    }
    if !found {
        for d in ancestors(&script_dir) {
            let nm = format!("{}/node_modules", d.trim_end_matches('/'));
            if std::path::Path::new(&nm).is_dir() {
                directory_items(&nm, fragment, &listing, None, &mut out);
            }
        }
    }
    Some(out)
}

/// Package names under the type roots (`@types/x` as `x`), or, with a directory in the
/// fragment, the files of the package it names.
fn typings_items(
    cx: &Ctx<'_, '_>,
    script_dir: &str,
    frag_dir: Option<&str>,
    listing: &Listing<'_>,
    out: &mut Vec<Item>,
) {
    let o = &cx.program.options;
    let ro = ResolverOptions::from_compiler_options(o);
    let mut roots: Vec<String> = match &ro.type_roots {
        Some(r) => r.clone(),
        None => {
            let start = ro
                .config_dir
                .clone()
                .unwrap_or_else(|| script_dir.to_string());
            ancestors(&start)
                .into_iter()
                .map(|d| format!("{}/node_modules/@types", d.trim_end_matches('/')))
                .collect()
        }
    };
    for pj in package_jsons(script_dir) {
        roots.push(format!("{}/node_modules/@types", dir_of(&pj)));
    }
    let types = o.types();
    let mut seen = BTreeSet::new();
    for root in roots {
        if !seen.insert(root.clone()) {
            continue;
        }
        for (name, is_dir) in read_dir(&root) {
            if !is_dir {
                continue;
            }
            let pkg = match name.split_once("__") {
                Some((scope, rest)) => format!("@{scope}/{rest}"),
                None => name.clone(),
            };
            if types.is_some_and(|t| !t.contains(&pkg)) {
                continue;
            }
            match frag_dir {
                None => push(out, pkg, KIND_MODULE),
                Some(d) => {
                    let rest = if d == pkg {
                        Some("")
                    } else {
                        d.strip_prefix(&format!("{pkg}/"))
                    };
                    if let Some(rest) = rest {
                        let base = format!("{root}/{name}");
                        let frag = if rest.is_empty() {
                            String::new()
                        } else {
                            format!("{rest}/")
                        };
                        directory_items(&base, &frag, listing, None, out);
                    }
                }
            }
        }
    }
}

/// `package.json` files in `dir` and its ancestors.
fn package_jsons(dir: &str) -> Vec<String> {
    ancestors(dir)
        .into_iter()
        .map(|d| format!("{}/package.json", d.trim_end_matches('/')))
        .filter(|p| std::path::Path::new(p).is_file())
        .collect()
}

/// Dependency names (not `@types/`) of every package.json above `dir`.
fn dependencies(dir: &str) -> Vec<String> {
    let mut out = Vec::new();
    for pj in package_jsons(dir) {
        let Ok(text) = std::fs::read_to_string(&pj) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        for key in [
            "dependencies",
            "devDependencies",
            "peerDependencies",
            "optionalDependencies",
        ] {
            if let Some(m) = v[key].as_object() {
                for name in m.keys() {
                    if !name.starts_with("@types/") && !out.contains(name) {
                        out.push(name.clone());
                    }
                }
            }
        }
    }
    out
}

/// `/// <reference path="|` and `/// <reference types="|` in the comment holding `offset`.
pub(super) fn reference_items(cx: &Ctx<'_, '_>, ck: &Ck<'_, '_>, offset: u32) -> Option<Vec<Item>> {
    let c = ck.c;
    let text = c.file(cx.file).text.as_str();
    let line_start = text[..offset as usize].rfind('\n').map_or(0, |i| i + 1);
    let line = &text[line_start..offset as usize];
    let rest = line.trim_start().strip_prefix("///")?.trim_start();
    let rest = rest.strip_prefix("<reference")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let rest = rest.trim_start();
    let (kind, rest) = if let Some(r) = rest.strip_prefix("path") {
        ("path", r)
    } else if let Some(r) = rest.strip_prefix("types") {
        ("types", r)
    } else {
        return None;
    };
    let rest = rest.trim_start().strip_prefix('=')?.trim_start();
    let frag = rest.strip_prefix('"').or_else(|| rest.strip_prefix('\''))?;
    if frag.contains('"') || frag.contains('\'') {
        return None;
    }
    let path = c.file(cx.file).path.clone();
    let script_dir = dir_of(&path).to_string();
    let mut out = Vec::new();
    if kind == "path" {
        let listing = listing_for(cx, ck, Ending::Filename, &[]);
        directory_items(&script_dir, frag, &listing, Some(&path), &mut out);
    } else {
        let listing = listing_for(cx, ck, Ending::Minimal, &[]);
        let frag_dir = frag
            .contains('/')
            .then(|| frag.rfind('/').map_or(frag, |i| &frag[..i]));
        typings_items(cx, &script_dir, frag_dir, &listing, &mut out);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_relative_paths() {
        assert_eq!(join("/p/src", "./a/"), "/p/src/a");
        assert_eq!(join("/p/src", "../"), "/p");
        assert_eq!(join("/p/src", "/x/"), "/x");
        assert_eq!(dir_of("/p/a.ts"), "/p");
        assert_eq!(dir_of("/a.ts"), "/");
    }

    #[test]
    fn labels_are_escaped_for_the_literal() {
        assert_eq!(escape_label("a\"b\\c\n", false), "a\\\"b\\\\c\\n");
        assert_eq!(escape_label("a\"`b", true), "a\"\\`b");
        assert!(ends_escaped("\"ab\\\""));
        assert!(!ends_escaped("\"ab\\\\\""));
    }

    #[test]
    fn file_names_follow_the_ending() {
        let l = Listing {
            allow_js: false,
            json: true,
            jsx_preserve: false,
            ending: Ending::Minimal,
            extra_exts: &[],
        };
        assert_eq!(l.file_name("a.ts").as_deref(), Some("a"));
        assert_eq!(l.file_name("a.d.ts").as_deref(), Some("a"));
        assert_eq!(l.file_name("a.mts").as_deref(), Some("a.mjs"));
        assert_eq!(l.file_name("a.js"), None);
        assert_eq!(
            l.file_name("tsconfig.json").as_deref(),
            Some("tsconfig.json")
        );
        let js = Listing {
            ending: Ending::Js,
            allow_js: true,
            ..l
        };
        assert_eq!(js.file_name("a.tsx").as_deref(), Some("a.js"));
        assert_eq!(js.file_name("a.js").as_deref(), Some("a.js"));
        assert!(is_relative("./x") && is_relative("../") && !is_relative(".") && !is_relative("x"));
    }
}
