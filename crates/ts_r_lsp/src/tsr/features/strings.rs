//! Completions inside string literals: the literal types the position expects, property
//! names (`o["|"]`, `"|" in o`, quoted object literal keys, `T["|"]`), and module
//! specifiers (`import "./|"`, `require("|")`, `/// <reference path="|"`).
//!
//! The rules were learned from tsserver's behaviour on its fourslash tests
//! (`completionForStringLiteral*`); see the crate's AGENTS.md, "String completions".

use super::Ctx;
use super::complete::ImportStyle;
use std::cmp::Ordering;
use std::collections::BTreeSet;
use ts_r_ast::{Ast, BinaryOp, Node, NodeId};
use ts_r_binder::SymbolFlags;
use ts_r_checker::members::SigKind;
use ts_r_checker::{Ck, Loc};
use ts_r_config::jsonc::{Value, ValueKind};
use ts_r_diagnostics::FileId;
use ts_r_resolver::{ImportKind, PackageJson, Resolver, ResolverOptions};
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

/// One string completion: label and LSP kind (with [`OPTIONAL`] added for an optional
/// property).
pub(super) type Item = (String, u32);

/// Kind bit marking an optional property's name (its label gets a `?`).
pub(super) const OPTIONAL: u32 = 1 << 16;

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
        Node::ImportDecl { source, .. } if source == child => {
            module_items(cx, ck, fragment, ImportKind::Static)
        }
        Node::ExportNamed {
            source: Some(s), ..
        }
        | Node::ExportAll { source: s, .. }
            if s == child =>
        {
            module_items(cx, ck, fragment, ImportKind::Static)
        }
        Node::ExternalModuleRef { .. } => module_items(cx, ck, fragment, ImportKind::ImportEquals),
        Node::ImportCall { arg, .. } if arg == child => {
            module_items(cx, ck, fragment, ImportKind::Dynamic)
        }
        Node::ImportType { arg, .. } if arg == child => {
            module_items(cx, ck, fragment, ImportKind::Static)
        }
        Node::Call { callee, args, .. }
            if ast.list(args).first() == Some(&child)
                && matches!(ast.node(callee), Node::Ident { name } if ast.atom(*name) == "require") =>
        {
            module_items(cx, ck, fragment, ImportKind::RequireCall)
        }
        // `with { type: "|" }`: the literals of that property of the global
        // `ImportAttributes` interface.
        Node::ImportAttribute { key, value } if value == child => {
            let name = match ast.node(key) {
                Node::Ident { name } | Node::StringLit { value: name } => ast.atom(*name),
                _ => return None,
            };
            Some(import_attribute_items(ck, name))
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
        // `G<"|">`: the type parameter's constraint (`tsc: tsc/internal/checker/services.go`).
        Node::TypeArgs { .. } => {
            let k = super::type_args::argument_constraint(ck, Loc::new(parent.file, child))?;
            Some(literal_items(ck, k, &[]))
        }
        // `G<{ x: "|" }>`: what the property is expected to be.
        Node::PropertySignature { type_ann, .. } if type_ann == Some(child) => {
            let k = super::type_args::expected_type(ck, parent)?;
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
/// The string literals the global `ImportAttributes` interface allows for `key`.
pub(super) fn import_attribute_items(ck: &mut Ck<'_, '_>, key: &str) -> Vec<Item> {
    let Some(s) =
        ck.c.global_symbol("ImportAttributes", ts_r_binder::meaning::TYPE)
    else {
        return Vec::new();
    };
    let t = ck.declared_type_of_symbol(s);
    let Some(p) = ck.property_of_type(t, key) else {
        return Vec::new();
    };
    let pt = ck.type_of_symbol(p);
    literal_items(ck, pt, &[])
}

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
                let opt = if super::complete::is_optional(ck.c, s) {
                    OPTIONAL
                } else {
                    0
                };
                out.push((name, kind | opt));
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
    /// With `allowImportingTsExtensions`: whether a TypeScript file is named with its own
    /// extension. `always` for ES modules under node16/nodenext; `wildcard` for matches of a
    /// package.json `exports`/`imports` wildcard when extensionless names are preferred.
    ts_always: bool,
    ts_wildcard: bool,
    /// Extensions ambient `declare module "*.css"` patterns make importable.
    extra_exts: &'a [String],
}

const TS_EXTS: &[&str] = &[".d.ts", ".d.mts", ".d.cts", ".ts", ".tsx", ".mts", ".cts"];
const JS_EXTS: &[&str] = &[".js", ".jsx", ".mjs", ".cjs"];

/// The known extension `name` ends with (`.d.ts` before `.ts`), if any.
fn known_ext(name: &str) -> Option<&'static str> {
    TS_EXTS
        .iter()
        .chain(JS_EXTS)
        .chain(&[".json"])
        .find(|e| name.len() > e.len() && name.ends_with(*e))
        .copied()
}

/// `a.d.css.ts` declares `a.css`, so `a.css` is what an import names.
fn declared_file(name: &str) -> Option<String> {
    let no_ts = name.strip_suffix(".ts")?;
    if name.ends_with(".d.ts") {
        return None;
    }
    let d = no_ts.find(".d.")?;
    let ext = &no_ts[no_ts.rfind('.')?..];
    Some(format!("{}{ext}", &no_ts[..d]))
}

impl Listing<'_> {
    /// Whether the file `name` can be imported: a TypeScript file, a JavaScript file with
    /// `allowJs`, JSON with `resolveJsonModule`, or an extension an ambient pattern declares.
    /// Hidden files (`.x`) are never listed.
    fn importable(&self, name: &str) -> bool {
        if name.starts_with('.') {
            return false;
        }
        match known_ext(name) {
            Some(e) if JS_EXTS.contains(&e) => self.allow_js,
            Some(".json") => self.json || self.extra_exts.iter().any(|x| x == ".json"),
            Some(_) => true,
            None => self.extra_exts.iter().any(|e| name.ends_with(e.as_str())),
        }
    }

    /// The name an import writes for the file `name`. A `wildcard` match of package.json
    /// `exports`/`imports` (`"./*": "./src/*"`) must name the file, so its extension is
    /// never dropped.
    fn module_name(&self, name: &str, wildcard: bool) -> String {
        if let Some(real) = declared_file(name) {
            return real;
        }
        if self.ending == Ending::Filename {
            return name.to_string();
        }
        let Some(ext) = known_ext(name) else {
            return name.to_string();
        };
        let stem = &name[..name.len() - ext.len()];
        if (self.ts_always || (wildcard && self.ts_wildcard))
            && matches!(ext, ".ts" | ".tsx" | ".mts" | ".cts")
        {
            return name.to_string();
        }
        if self.ending == Ending::Minimal
            && !wildcard
            && matches!(ext, ".d.ts" | ".ts" | ".tsx" | ".js" | ".jsx")
        {
            return stem.to_string();
        }
        match ext {
            ".d.ts" | ".ts" => format!("{stem}.js"),
            ".tsx" if self.jsx_preserve => format!("{stem}.jsx"),
            ".tsx" => format!("{stem}.js"),
            ".d.mts" | ".mts" => format!("{stem}.mjs"),
            ".d.cts" | ".cts" => format!("{stem}.cjs"),
            _ => name.to_string(),
        }
    }

    /// The completion name of the file `name`, or `None` when it cannot be imported.
    fn file_name(&self, name: &str) -> Option<String> {
        self.importable(name).then(|| self.module_name(name, false))
    }
}

/// Specifiers starting `./` or `../` are relative to the importing file.
fn is_relative(spec: &str) -> bool {
    spec.starts_with("./") || spec.starts_with("../")
}

/// The directory part of `p`, keeping its root (`/`, `c:/`, `//server/`). A trailing `/` is
/// a separator of its own: `dir_of("sub/")` is `sub`.
fn dir_of(p: &str) -> &str {
    let root = ts_r_config::path::root_length(p);
    match p.rfind('/') {
        Some(i) if i >= root => &p[..i],
        Some(_) => &p[..root],
        None => "",
    }
}

/// Joins `dir` and a relative `rel` (which may hold `.` and `..` segments), keeping the
/// root of either (a POSIX, drive or UNC root).
fn join(dir: &str, rel: &str) -> String {
    ts_r_config::path::resolve(dir, rel)
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

/// Adds `name` once: a module name wins over a file of that name, a file over a folder.
fn push(out: &mut Vec<Item>, name: String, kind: u32) {
    let rank = |k: u32| match k {
        KIND_FOLDER => 0,
        KIND_FILE => 1,
        _ => 2,
    };
    match out.iter_mut().find(|(n, _)| *n == name) {
        Some(e) if rank(e.1) < rank(kind) => e.1 = kind,
        Some(_) => {}
        None => out.push((name, kind)),
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
    let allow_ts = e.allow_importing_ts_extensions();
    let (mut ts_always, mut ts_wildcard) = (false, false);
    if ending == Ending::Minimal {
        let style = ImportStyle::of(ck.c, cx.file);
        let esm_node = super::complete::needs_js_extension(o, &ck.c.file(cx.file).path);
        ts_always = allow_ts && esm_node;
        ts_wildcard = allow_ts && !style.js_ext;
        if style.js_ext || esm_node {
            ending = Ending::Js;
        }
    }
    Listing {
        allow_js: e.allow_js(),
        json: e.resolve_json_module(),
        jsx_preserve: o.jsx() == Some(ts_r_config::JsxEmit::Preserve),
        ending,
        ts_always,
        ts_wildcard,
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

/// Which kind of pattern map a key comes from: compiler `paths` and package.json
/// `typesVersions`, or package.json `exports` / `imports`.
#[derive(Clone, Copy, PartialEq)]
enum Table {
    Paths,
    Exports,
    Imports,
}

/// A map key or target: exact, or one `*` between a prefix and a suffix.
#[derive(Clone, Copy)]
enum Pattern<'a> {
    Exact,
    Star(&'a str, &'a str),
}

/// `None` for a pattern with more than one `*` (such a key is ignored).
fn pattern(s: &str) -> Option<Pattern<'_>> {
    match s.find('*') {
        None => Some(Pattern::Exact),
        Some(i) if !s[i + 1..].contains('*') => Some(Pattern::Star(&s[..i], &s[i + 1..])),
        Some(_) => None,
    }
}

/// `Less` when key `a` wins over `b`. `paths`: the longer prefix (a key without `*` counts
/// whole). `exports`/`imports`: Node's order: the longer text up to and including the `*`,
/// then a key with a `*` before one without, then the longer key.
fn key_order(table: Table, a: &str, b: &str) -> Ordering {
    if table == Table::Paths {
        let len = |k: &str| match pattern(k) {
            Some(Pattern::Star(p, _)) => p.len(),
            _ => k.len(),
        };
        return len(b).cmp(&len(a));
    }
    let (sa, sb) = (a.find('*'), b.find('*'));
    let base = |k: &str, s: Option<usize>| s.map_or(k.len(), |i| i + 1);
    base(b, sb)
        .cmp(&base(a, sa))
        .then_with(|| match (sa.is_some(), sb.is_some()) {
            (false, _) => Ordering::Greater,
            (true, false) => Ordering::Less,
            _ => b.len().cmp(&a.len()),
        })
}

/// The suffixes a target's suffix stands for on disk: `*.js` is written by `*.d.ts`, `*.ts`,
/// `*.tsx`, ... files (`.mjs` by `.d.mts`, `.mts`; `.cjs` by `.d.cts`, `.cts`).
fn matching_suffixes(suffix: &str) -> Vec<String> {
    const REMOVABLE: &[&str] = &[
        ".d.ts", ".d.mts", ".d.cts", ".mjs", ".mts", ".cjs", ".cts", ".ts", ".js", ".tsx", ".jsx",
        ".json",
    ];
    if suffix.is_empty() {
        return vec![String::new()];
    }
    let ends = |es: &[&str]| es.iter().any(|e| suffix.ends_with(e));
    let decl = if ends(&[".mjs", ".mts"]) {
        ".d.mts"
    } else if ends(&[".cjs", ".cts"]) {
        ".d.cts"
    } else if ends(&[".json"]) {
        ".d.json.ts"
    } else {
        ".d.ts"
    };
    let inputs: &[&str] = if ends(&[".d.mts", ".mjs", ".mts"]) {
        &[".mts", ".mjs"]
    } else if ends(&[".d.cts", ".cjs", ".cts"]) {
        &[".cts", ".cjs"]
    } else if ends(&[".d.json.ts"]) {
        &[".json"]
    } else {
        &[".tsx", ".ts", ".jsx", ".js"]
    };
    let change = |ext: &str| match REMOVABLE.iter().find(|e| suffix.ends_with(*e)) {
        Some(e) => format!("{}{ext}", &suffix[..suffix.len() - e.len()]),
        None => suffix.to_string(),
    };
    let mut out = vec![change(decl)];
    out.extend(inputs.iter().map(|e| change(e)));
    out.push(suffix.to_string());
    out
}

/// Directories [`files_below`] lists at most in one search.
const MAX_DIRS: u32 = 4096;

/// Files under `dir` (depth-first, files of a directory before its subdirectories), leaving
/// out `node_modules` and hidden entries. At most `MAX_ENTRIES` files, `MAX_DEPTH` levels and
/// `MAX_DIRS` directory listings; a directory reached again through a link (same real path)
/// is not listed twice.
fn files_below(dir: &str, out: &mut Vec<String>) {
    let mut walk = Walk {
        seen: BTreeSet::new(),
        dirs: 0,
    };
    walk_files(dir, 0, &mut walk, out);
}

/// The state of one [`files_below`] search.
struct Walk {
    /// Real paths of the directories listed.
    seen: BTreeSet<std::path::PathBuf>,
    /// Directories listed so far.
    dirs: u32,
}

fn walk_files(dir: &str, depth: u32, walk: &mut Walk, out: &mut Vec<String>) {
    const MAX_DEPTH: u32 = 16;
    if depth > MAX_DEPTH || out.len() >= MAX_ENTRIES || walk.dirs >= MAX_DIRS {
        return;
    }
    walk.dirs += 1;
    let Ok(real) = std::fs::canonicalize(dir) else {
        return;
    };
    if !walk.seen.insert(real) {
        return;
    }
    let entries = read_dir(dir);
    let dir = dir.trim_end_matches('/');
    for (name, is_dir) in &entries {
        if !is_dir && !name.starts_with('.') && out.len() < MAX_ENTRIES {
            out.push(format!("{dir}/{name}"));
        }
    }
    for (name, is_dir) in &entries {
        if *is_dir
            && !name.starts_with('.')
            && !matches!(
                name.as_str(),
                "node_modules" | "bower_components" | "jspm_packages"
            )
        {
            walk_files(&format!("{dir}/{name}"), depth + 1, walk, out);
        }
    }
}

/// The pattern of an `exports`/`imports` target: a string, or the first key of a conditions
/// object that is `default`, one of `conditions` or `types@<range>` matching our version.
fn first_condition(v: &Value, conditions: &[String], depth: u32) -> Option<String> {
    match &v.kind {
        ValueKind::String(s) => Some(s.clone()),
        ValueKind::Object(members) if depth < 16 => {
            let m = members.iter().find(|m| {
                m.key == "default"
                    || conditions.contains(&m.key)
                    || m.key.strip_prefix("types@").is_some_and(|r| {
                        conditions.iter().any(|c| c == "types")
                            && ts_r_resolver::semver::range_matches(r, ts_r_resolver::TS_VERSION)
                    })
            })?;
            first_condition(&m.value, conditions, depth + 1)
        }
        _ => None,
    }
}

/// `paths`-like entries of a `typesVersions` path map.
fn path_entries(v: &Value) -> Vec<(String, Vec<String>)> {
    let Some(members) = v.as_object() else {
        return Vec::new();
    };
    members
        .iter()
        .filter_map(|m| {
            let targets = m
                .value
                .as_array()?
                .iter()
                .filter_map(|t| t.as_str().map(str::to_owned))
                .collect();
            Some((m.key.clone(), targets))
        })
        .collect()
}

/// Lists module specifiers: directory listings, and the `paths` / `typesVersions` /
/// `exports` / `imports` pattern maps.
struct Modules<'a> {
    listing: Listing<'a>,
    /// For `imports` targets: output directories (`outDir`, `declarationDir`) and the source
    /// directory they mirror.
    out_dirs: Vec<(String, String)>,
}

impl Modules<'_> {
    /// A non-relative listing of `fragment`'s directory under `base` (a `node_modules`
    /// folder): when a package.json above that directory has `typesVersions` paths that
    /// match, only they are listed.
    fn package_directory(&self, base: &str, fragment: &str, out: &mut Vec<Item>) {
        let dir_part = match fragment.rfind('/') {
            Some(i) => &fragment[..=i],
            None => "",
        };
        let abs = join(base, dir_part);
        for d in ancestors(&abs) {
            let pj = format!("{}/package.json", d.trim_end_matches('/'));
            let Ok(text) = std::fs::read_to_string(&pj) else {
                continue;
            };
            let paths = PackageJson::parse(&d, &pj, &text).and_then(|p| {
                p.types_versions_paths(ts_r_resolver::TS_VERSION)
                    .map(path_entries)
            });
            if let Some(paths) = paths {
                let inside = abs
                    .strip_prefix(d.trim_end_matches('/'))
                    .unwrap_or("")
                    .trim_start_matches('/');
                let inside = if inside.is_empty() {
                    String::new()
                } else {
                    format!("{inside}/")
                };
                if self.add_from_patterns(out, Table::Paths, &inside, &d, &paths) {
                    return;
                }
            }
            break;
        }
        directory_items(base, fragment, &self.listing, None, out);
    }

    /// Adds what the pattern map `entries` (key, targets) offers for `fragment`, targets
    /// relative to `base`. When a key with a `*` matches, the best matching key's results
    /// replace those of earlier matches, and keys that rank below it add nothing; exact keys
    /// are always offered. Returns whether a key matched.
    fn add_from_patterns(
        &self,
        out: &mut Vec<Item>,
        table: Table,
        fragment: &str,
        base: &str,
        entries: &[(String, Vec<String>)],
    ) -> bool {
        let mut groups: Vec<(bool, Vec<Item>)> = Vec::new();
        let mut best: Option<String> = None;
        for (key, targets) in entries {
            if key == "." {
                continue;
            }
            let mut k = key.strip_prefix("./").unwrap_or(key).to_string();
            if table != Table::Paths && key.ends_with('/') {
                k.push('*');
            }
            let Some(p) = pattern(&k) else {
                continue;
            };
            let is_match = matches!(p, Pattern::Star(pre, suf)
                if fragment.len() >= pre.len() + suf.len()
                    && fragment.starts_with(pre)
                    && fragment.ends_with(suf));
            if is_match
                && best
                    .as_deref()
                    .is_none_or(|b| key_order(table, &k, b) == Ordering::Less)
            {
                best = Some(k.clone());
                groups.retain(|g| !g.0);
            }
            let keep = matches!(p, Pattern::Exact)
                || best
                    .as_deref()
                    .is_none_or(|b| key_order(table, &k, b) != Ordering::Greater);
            if keep {
                let items = self.path_mapping(&k, p, targets, fragment, base, table);
                groups.push((is_match, items));
            }
        }
        for (_, items) in groups {
            for (name, kind) in items {
                push(out, name, kind);
            }
        }
        best.is_some()
    }

    /// What one key offers: an exact key itself; a `*` key the fragment has not reached yet
    /// its directory (`foo/*` as `foo`) or its targets' files wrapped in the key; a `*` key the
    /// fragment is inside the targets' files for the rest of the fragment.
    fn path_mapping(
        &self,
        key: &str,
        p: Pattern<'_>,
        targets: &[String],
        fragment: &str,
        base: &str,
        table: Table,
    ) -> Vec<Item> {
        // Names are written relative to the fragment's directory (`foo/` of `foo/b`).
        let frag_dir = match fragment.rfind('/') {
            Some(i) => &fragment[..=i],
            None => "",
        };
        let named = |name: &str, kind: u32| -> Vec<Item> {
            if !name.starts_with(fragment) {
                return Vec::new();
            }
            let n = if name.len() > 1 {
                name.strip_suffix('/').unwrap_or(name)
            } else {
                name
            };
            let n = n.strip_prefix(frag_dir).unwrap_or(n);
            vec![(n.to_string(), kind)]
        };
        // A file match is wrapped in the key's text after the `*`; a folder is not.
        let wrap = |lead: &str, items: Vec<Item>, suf: &str| -> Vec<Item> {
            items
                .into_iter()
                .map(|(n, k)| {
                    let tail = if k == KIND_FILE { suf } else { "" };
                    (format!("{lead}{n}{tail}"), k)
                })
                .collect()
        };
        match p {
            Pattern::Exact => named(key, KIND_FILE),
            Pattern::Star(pre, suf) => match fragment.strip_prefix(pre) {
                None if !pre.starts_with(fragment) => Vec::new(),
                None if key.ends_with("/*") => named(pre, KIND_FOLDER),
                None => {
                    let lead = pre.get(frag_dir.len()..).unwrap_or("");
                    let items = targets
                        .iter()
                        .flat_map(|t| self.pattern_modules("", base, t, table))
                        .collect();
                    wrap(lead, items, suf)
                }
                Some(rest) => {
                    let lead = if frag_dir.starts_with(pre) {
                        ""
                    } else {
                        pre.get(frag_dir.len()..).unwrap_or("")
                    };
                    let items = targets
                        .iter()
                        .flat_map(|t| self.pattern_modules(rest, base, t, table))
                        .collect();
                    wrap(lead, items, suf)
                }
            },
        }
    }

    /// Files and folders the target pattern `target` (under `base`) offers for `fragment`,
    /// the part of the specifier the key's `*` stands for.
    fn pattern_modules(&self, fragment: &str, base: &str, target: &str, table: Table) -> Vec<Item> {
        let Some(Pattern::Star(pre, suf)) = pattern(target) else {
            return Vec::new();
        };
        let (pre_dir, pre_base) = if pre.is_empty() || pre.ends_with('/') {
            (pre, "")
        } else {
            match pre.rfind('/') {
                Some(i) => (&pre[..=i], &pre[i + 1..]),
                None => ("", pre),
            }
        };
        let frag_dir = fragment.contains('/').then(|| {
            if fragment.ends_with('/') {
                fragment
            } else {
                dir_of(fragment)
            }
        });
        let expanded = match frag_dir {
            Some(d) => format!("{pre_dir}{pre_base}{d}"),
            None => pre_dir.to_string(),
        };
        let mut dirs = vec![join(base, &expanded)];
        if table == Table::Imports {
            for (out_dir, src) in &self.out_dirs {
                let rest = dirs[0]
                    .strip_prefix(out_dir.as_str())
                    .filter(|r| r.is_empty() || r.starts_with('/'));
                if let Some(rest) = rest {
                    dirs.push(format!("{src}{rest}"));
                }
            }
        }
        let suffixes = matching_suffixes(suf);
        let wildcard = table != Table::Paths && target.ends_with("/*");
        let mut out = Vec::new();
        for dir in &dirs {
            let prefix = match frag_dir {
                Some(_) => dir.clone(),
                None => format!("{}/{pre_base}", dir.trim_end_matches('/')),
            };
            let mut files = Vec::new();
            if suf.is_empty() {
                let d = dir.trim_end_matches('/');
                files.extend(
                    read_dir(dir)
                        .into_iter()
                        .filter(|(_, is_dir)| !is_dir)
                        .map(|(n, _)| format!("{d}/{n}")),
                );
            } else {
                files_below(dir, &mut files);
            }
            for f in files {
                let name = f.rsplit('/').next().unwrap_or(&f);
                if !self.listing.importable(name) {
                    continue;
                }
                let inner = suffixes.iter().find_map(|s| {
                    let i = f.strip_prefix(prefix.as_str())?.strip_suffix(s.as_str())?;
                    Some(i.strip_prefix('/').unwrap_or(i))
                });
                let Some(inner) = inner.filter(|i| !i.is_empty()) else {
                    continue;
                };
                match inner.split_once('/') {
                    Some((first, _)) => push(&mut out, first.to_string(), KIND_FOLDER),
                    None => push(
                        &mut out,
                        self.listing.module_name(inner, wildcard),
                        KIND_FILE,
                    ),
                }
            }
        }
        if suf.is_empty() {
            for dir in &dirs {
                for (name, is_dir) in read_dir(dir) {
                    if is_dir && name != "node_modules" {
                        push(&mut out, name, KIND_FOLDER);
                    }
                }
            }
        }
        out
    }

    /// `exports` or `imports` of a package.json (in `dir`) for `fragment`. Returns whether
    /// the search for the package stops here: a map was read, or `exports` is a single
    /// entry point (or `null`).
    fn lookup(
        &self,
        out: &mut Vec<Item>,
        table_value: Option<&Value>,
        table: Table,
        fragment: &str,
        dir: &str,
        conditions: &[String],
    ) -> bool {
        let Some(v) = table_value else {
            return false;
        };
        let Some(members) = v.as_object() else {
            return true;
        };
        let entries: Vec<(String, Vec<String>)> = members
            .iter()
            .filter_map(|m| {
                let mut p = first_condition(&m.value, conditions, 0)?;
                if m.key.ends_with('/') && p.ends_with('/') {
                    p.push('*');
                }
                Some((m.key.clone(), vec![p]))
            })
            .collect();
        self.add_from_patterns(out, table, fragment, dir, &entries);
        true
    }
}

/// Completions for a module specifier being typed (`fragment`), written as `kind`.
/// Module specifier completions for `fragment` as a static import names it (a JSDoc
/// `@import ... from "|"`).
pub(super) fn static_module_items(
    cx: &Ctx<'_, '_>,
    ck: &Ck<'_, '_>,
    fragment: &str,
) -> Option<Vec<Item>> {
    module_items(cx, ck, fragment, ImportKind::Static)
}

fn module_items(
    cx: &Ctx<'_, '_>,
    ck: &Ck<'_, '_>,
    fragment: &str,
    kind: ImportKind,
) -> Option<Vec<Item>> {
    let c = ck.c;
    let fragment = fragment.replace('\\', "/");
    let fragment = fragment.as_str();
    let path = c.file(cx.file).path.clone();
    let script_dir = dir_of(&path).to_string();
    let extra = ambient_exts(ck);
    let opts = &cx.program.options;
    let ro = ResolverOptions::from_compiler_options(opts);
    let mut out_dirs = Vec::new();
    if let Some(src) = ro.root_dir.clone().or_else(|| ro.config_dir.clone()) {
        for d in [&ro.out_dir, &ro.declaration_dir].into_iter().flatten() {
            out_dirs.push((
                d.trim_end_matches('/').to_string(),
                src.trim_end_matches('/').to_string(),
            ));
        }
    }
    let m = Modules {
        listing: listing_for(cx, ck, Ending::Minimal, &extra),
        out_dirs,
    };
    let mut out = Vec::new();
    if is_relative(fragment) || (opts.paths().is_none() && fragment.starts_with('/')) {
        directory_items(&script_dir, fragment, &m.listing, Some(&path), &mut out);
        return Some(out);
    }
    let frag_dir = fragment.contains('/').then(|| {
        if fragment.ends_with('/') {
            fragment.trim_end_matches('/')
        } else {
            dir_of(fragment)
        }
    });
    if let Some(paths) = opts.paths() {
        let base = ro
            .paths_base
            .clone()
            .or_else(|| ro.config_dir.clone())
            .unwrap_or_else(|| script_dir.clone());
        m.add_from_patterns(&mut out, Table::Paths, fragment, &base, &paths.0);
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
    typings_items(cx, &script_dir, frag_dir, &m.listing, &mut out);
    if ro.algorithm == ts_r_config::ModuleResolution::Classic {
        return Some(out);
    }
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
    if found {
        return Some(out);
    }
    let fs = ts_r_config::RealFs::new();
    let exports_on = ro.resolve_package_json_exports;
    let imports_on = ro.resolve_package_json_imports;
    let resolver = Resolver::new(&fs, ro);
    let mut conditions = resolver.conditions(resolver.mode_for(&path, kind)).to_vec();
    conditions.push("default".to_string());
    let mut seen_scope = false;
    let imports = |d: &str, out: &mut Vec<Item>, seen: &mut bool| {
        if !imports_on || *seen {
            return;
        }
        let pj = format!("{}/package.json", d.trim_end_matches('/'));
        let Ok(text) = std::fs::read_to_string(&pj) else {
            return;
        };
        *seen = true;
        if let Some(p) = PackageJson::parse(d, &pj, &text) {
            m.lookup(
                out,
                p.imports.as_ref(),
                Table::Imports,
                fragment,
                d,
                &conditions,
            );
        }
    };
    let comps: Vec<&str> = fragment.split('/').filter(|s| !s.is_empty()).collect();
    for d in ancestors(&script_dir) {
        let root = d.trim_end_matches('/');
        if frag_dir.is_some() && exports_on && !comps.is_empty() {
            let (pkg, rest) = if comps[0].starts_with('@') && comps.len() > 1 {
                (format!("{}/{}", comps[0], comps[1]), &comps[2..])
            } else {
                (comps[0].to_string(), &comps[1..])
            };
            let scoped_alone = comps[0].starts_with('@') && comps.len() == 1;
            if !scoped_alone {
                if imports_on && pkg.starts_with('#') {
                    imports(&d, &mut out, &mut seen_scope);
                    continue;
                }
                let pkg_dir = format!("{root}/node_modules/{pkg}");
                let pj = format!("{pkg_dir}/package.json");
                if let Ok(text) = std::fs::read_to_string(&pj)
                    && let Some(p) = PackageJson::parse(&pkg_dir, &pj, &text)
                {
                    let mut sub = rest.join("/");
                    if !rest.is_empty() && fragment.ends_with('/') {
                        sub.push('/');
                    }
                    if m.lookup(
                        &mut out,
                        p.exports.as_ref(),
                        Table::Exports,
                        &sub,
                        &pkg_dir,
                        &conditions,
                    ) {
                        continue;
                    }
                }
            }
        }
        let nm = format!("{root}/node_modules");
        if std::path::Path::new(&nm).is_dir() {
            m.package_directory(&nm, fragment, &mut out);
        }
        imports(&d, &mut out, &mut seen_scope);
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
    let frag = frag.replace('\\', "/");
    let frag = frag.as_str();
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
        assert_eq!(dir_of("sub/"), "sub");
    }

    /// Drive and UNC roots survive a join (`import "./|"` in `c:/project/src/main.ts`).
    #[test]
    fn joins_keep_windows_roots() {
        assert_eq!(join("C:/project/src", "./"), "C:/project/src");
        assert_eq!(join("C:/project/src", "../lib/"), "C:/project/lib");
        assert_eq!(join("C:/", "../"), "C:/");
        assert_eq!(join("//server/share/src", "./a/"), "//server/share/src/a");
        assert_eq!(dir_of("C:/project/main.ts"), "C:/project");
        assert_eq!(dir_of("C:/main.ts"), "C:/");
        assert_eq!(ancestors("C:/project"), vec!["C:/project", "C:/"]);
        assert_eq!(
            ancestors("//server/share/x"),
            vec!["//server/share/x", "//server/share", "//server/"]
        );
    }

    /// A directory linked back to itself is listed once, and links that never reach a file
    /// stop at the directory budget instead of listing every path through them.
    #[cfg(unix)]
    #[test]
    fn files_below_lists_each_real_directory_once() {
        let dir = std::env::temp_dir().join(format!("tsr-files-below-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let src = dir.join("src");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("x.ts"), "").unwrap();
        std::fs::write(src.join("sub/y.ts"), "").unwrap();
        std::os::unix::fs::symlink(".", src.join("loop")).unwrap();
        let empty = dir.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        for l in ["a", "b", "c"] {
            std::os::unix::fs::symlink(".", empty.join(l)).unwrap();
        }
        let root = src.to_str().unwrap().to_string();
        let mut files = Vec::new();
        files_below(&root, &mut files);
        assert_eq!(
            files,
            vec![format!("{root}/x.ts"), format!("{root}/sub/y.ts")]
        );
        let t = std::time::Instant::now();
        let mut none = Vec::new();
        files_below(empty.to_str().unwrap(), &mut none);
        assert!(none.is_empty());
        assert!(t.elapsed() < std::time::Duration::from_secs(5));
        let _ = std::fs::remove_dir_all(&dir);
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
            ts_always: false,
            ts_wildcard: false,
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
        // `allowImportingTsExtensions`: a wildcard export match keeps `.ts`, a plain one doesn't.
        let ts = Listing {
            ts_wildcard: true,
            ..l
        };
        assert_eq!(ts.module_name("a.tsx", true), "a.tsx");
        assert_eq!(ts.module_name("a.d.ts", true), "a.js");
        assert_eq!(ts.module_name("a.ts", false), "a");
        assert!(is_relative("./x") && is_relative("../") && !is_relative(".") && !is_relative("x"));
    }
}
