//! Definition, type definition, references, document highlights and rename.

use super::keywords::{self, Doc, Special};
use super::locate::{
    Meaning, ident_at, literal_accesses_in, meaning_at, name_text, names_in, node_at, property_of,
};
use super::{Ctx, LinesCache, LspLines, decl_span};
use crate::jsonrpc::codes;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use ts_r_ast::{MethodKind, Node};
use ts_r_binder::SymbolFlags;
use ts_r_checker::{Ck, Loc};
use ts_r_diagnostics::FileId;
use ts_r_types::{ObjectKind, SymId, TypeId, TypeKind};

/// Go to definition: the declarations of the symbol under the cursor, through imports. On a
/// module specifier, the resolved file.
pub fn definition(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, offset: u32) -> Value {
    let c = cx.checker;
    let ast = c.ast(cx.file);
    if let Some(n) = node_at(ast, offset)
        && let Node::StringLit { value } = *ast.node(n)
        && let Some(target) = module_specifier_target(cx, n, ast.atom(value))
    {
        let mut lc = LinesCache::default();
        return json!([lc.location(c, target, 0, 0)]);
    }
    // `break l` / `continue l` go to the label.
    let d = Doc::new(c, cx.file);
    match keywords::special_at(&d, offset) {
        Some(Special::Label(Some(l))) => {
            let mut lc = LinesCache::default();
            return match keywords::label_refs(&d, l).first() {
                // On the label itself there is nothing to go to.
                Some(&(_, s, e)) if (s..=e).contains(&offset) => Value::Null,
                Some(&(f, s, e)) => json!([lc.location(c, f, s, e)]),
                None => Value::Null,
            };
        }
        Some(Special::Label(None)) => return Value::Null,
        _ => {}
    }
    if let Some(t) = keywords::keyword_definition(&d, offset) {
        return keyword_target(cx, ck, &d, t);
    }
    let Some(n) = ident_at(ast, offset) else {
        return Value::Null;
    };
    let m = meaning_at(ck, Loc::new(cx.file, n));
    let mut decls = Vec::new();
    for s in m.iter() {
        let s = c.resolve_alias(s).unwrap_or(s);
        decls.extend(c.decls(s));
    }
    // A call goes to the overload it resolves to.
    if decls.len() > 1
        && let Some(d) = called_overload(ck, Loc::new(cx.file, n))
        && decls.contains(&d)
    {
        decls = vec![d];
    }
    decl_locations(cx, decls)
}

/// Where go to definition on a keyword leads (see [`keywords::keyword_definition`]).
fn keyword_target(
    cx: &Ctx<'_, '_>,
    ck: &mut Ck<'_, '_>,
    d: &Doc<'_>,
    t: keywords::KeywordTarget,
) -> Value {
    use keywords::KeywordTarget as T;
    let c = cx.checker;
    match t {
        T::Spans(v) if v.is_empty() => Value::Null,
        T::Spans(v) => locations(cx, v.into_iter().map(|(s, e)| (cx.file, s, e))),
        T::Name(n) => definition(cx, ck, c.ast(cx.file).span(n).start),
        T::Override { member, name } => {
            let own = || {
                let s = d.ast.span(name);
                locations(cx, std::iter::once((cx.file, s.start, s.end)))
            };
            let Some(key) = name_text(d.ast, name) else {
                return own();
            };
            let Some(class) =
                keywords::member_class(d, member).and_then(|k| c.sym_of_decl(Loc::new(cx.file, k)))
            else {
                return own();
            };
            let bases: Vec<TypeId> = if keywords::member_is_static(d, member) {
                ck.base_constructor_type(class).into_iter().collect()
            } else {
                ck.base_types(class).to_vec()
            };
            let mut decls = Vec::new();
            for b in bases {
                if let Some(p) = ck.property_of_type(b, key) {
                    decls.extend(c.decls(p));
                }
            }
            if decls.is_empty() {
                own()
            } else {
                decl_locations(cx, decls)
            }
        }
        T::Super(n) => {
            let Some((member, _)) = keywords::super_member(d, n) else {
                return Value::Null;
            };
            let Some(class) =
                keywords::member_class(d, member).and_then(|k| c.sym_of_decl(Loc::new(cx.file, k)))
            else {
                return Value::Null;
            };
            let mut decls = Vec::new();
            for b in ck.base_types(class).to_vec() {
                type_decls(ck, b, &mut decls, 0);
            }
            let mut out = Vec::new();
            for l in decls {
                let (s, e) = decl_span(c, l);
                out.push((l.file, s, e));
            }
            // `super(...)` also goes to the constructor it calls, shown whole.
            if let Some(ctor) = called_overload(ck, Loc::new(cx.file, n)) {
                let s = c.ast(ctor.file).span(ctor.node);
                out.push((ctor.file, s.start, s.end));
            }
            out.dedup();
            locations(cx, out.into_iter())
        }
    }
}

/// The declaration of the signature a call resolves to, when `name` is the callee (`f(1)`,
/// `o.m(1)`, `new C()`, ``f`x` ``).
fn called_overload(ck: &mut Ck<'_, '_>, name: Loc) -> Option<Loc> {
    let c = ck.c;
    let mut callee = name.node;
    let mut p = c.parent(name)?;
    if let Node::Member { prop, .. } = *c.node(p)
        && prop == name.node
    {
        callee = p.node;
        p = c.parent(p)?;
    }
    let is_callee = match *c.node(p) {
        Node::Call { callee: x, .. } | Node::New { callee: x, .. } => x == callee,
        Node::TaggedTemplate { tag, .. } => tag == callee,
        _ => false,
    };
    if !is_callee {
        return None;
    }
    let sig = ck.resolved_signature(p)?;
    let store = ck.store();
    let target = store.sig(sig).target.map(|t| t.0).unwrap_or(sig);
    let (f, n) = store.sig(target).decl?;
    if f as usize >= c.files.len() {
        return None;
    }
    Some(Loc::new(FileId(f), ck.node_id(n)?))
}

/// The file a module specifier string resolves to.
fn module_specifier_target(cx: &Ctx<'_, '_>, lit: ts_r_ast::NodeId, spec: &str) -> Option<FileId> {
    let c = cx.checker;
    let parent = c.file(cx.file).parents().parent(lit)?;
    let is_spec = match *c.ast(cx.file).node(parent) {
        Node::ImportDecl { source, .. } | Node::ExportAll { source, .. } => source == lit,
        Node::ExportNamed { source, .. } => source == Some(lit),
        Node::ExternalModuleRef { expr } => expr == lit,
        Node::ImportCall { arg, .. } => arg == lit,
        Node::ImportType { .. } => true,
        _ => false,
    };
    if !is_spec {
        return None;
    }
    let from = &c.file(cx.file).path;
    let r = cx
        .program
        .resolutions
        .get(&(from.clone(), spec.to_string()))?
        .as_ref()?;
    let target = r.resolved.as_str();
    c.files
        .iter()
        .position(|f| f.path == target)
        .map(|i| FileId(i as u32))
}

/// Go to type definition: the declarations of the type of the symbol under the cursor
/// (tsc: `tsc/internal/ls/definition.go`, ProvideTypeDefinition). A keyword before a
/// declaration's name stands for the name; a function's own type stands for its return
/// type; `Array<T>`, `Promise<T>` and the other well-known generics also lead to `T`; a
/// type-only symbol whose type has no declaration goes to itself.
pub fn type_definition(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, offset: u32) -> Value {
    let c = cx.checker;
    let ast = c.ast(cx.file);
    let d = Doc::new(c, cx.file);
    let keyword_name = match keywords::keyword_definition(&d, offset) {
        Some(
            keywords::KeywordTarget::Name(n) | keywords::KeywordTarget::Override { name: n, .. },
        ) => Some(n),
        _ => None,
    };
    let Some(n) = keyword_name
        .or_else(|| ident_at(ast, offset))
        .or_else(|| node_at(ast, offset))
    else {
        return Value::Null;
    };
    let l = Loc::new(cx.file, n);
    let (t, sym) = match ast.node(n) {
        Node::This {} => (ck.check_expr(l), None),
        _ => {
            let Some(s) = meaning_at(ck, l).primary else {
                return Value::Null;
            };
            let s = c.resolve_alias(s).unwrap_or(s);
            let t = if super::locate::is_expression_name(ck, l) {
                ck.check_expr(l)
            } else if let Some(p) = c.parent(l)
                && let Node::Member { prop, .. } = *c.node(p)
                && prop == n
            {
                ck.check_expr(p)
            } else {
                ck.type_of_symbol(s)
            };
            (t, Some(s))
        }
    };
    let t = match sym {
        Some(s) if is_own_function_type(ck, s, t) => {
            let sigs = ck.signatures_of(t, ts_r_checker::members::SigKind::Call);
            match sigs[..] {
                [sig] => ck.return_type_of(sig),
                _ => t,
            }
        }
        _ => t,
    };
    let mut decls = Vec::new();
    if let Some(arg) = known_type_argument(ck, t) {
        type_decls(ck, arg, &mut decls, 0);
    }
    type_decls(ck, t, &mut decls, 0);
    if decls.is_empty()
        && let Some(s) = sym
    {
        let f = c.sym_flags(s);
        if !f.intersects(SymbolFlags::VALUE) && f.intersects(SymbolFlags::TYPE) {
            decls.extend(c.decls(s));
        }
    }
    let mut seen = HashSet::new();
    decls.retain(|l| seen.insert(*l));
    decl_locations(cx, decls)
}

/// The symbol a type comes from (tsc's `type.symbol`): the class or interface, the
/// function, class or namespace of a `typeof`, the type literal, the enum member.
fn type_symbol(ck: &Ck<'_, '_>, t: TypeId) -> Option<SymId> {
    let store = ck.store();
    match store.kind(t) {
        TypeKind::Object(_) => match store.object_of(t)?.kind {
            ObjectKind::Declared { sym, .. } | ObjectKind::Literal { sym } => Some(sym),
            ObjectKind::Reference { target, .. } => match store.object_of(target)?.kind {
                ObjectKind::Declared { sym, .. } => Some(sym),
                _ => None,
            },
            ObjectKind::Anonymous {
                decl: (u32::MAX, s),
                ..
            } => Some(SymId(s)),
            ObjectKind::Anonymous { decl: (f, n), .. } => {
                let node = ck.node_id(n)?;
                ck.c.sym_of_decl(Loc::new(FileId(f), node))
            }
            _ => None,
        },
        TypeKind::Enum(s) => Some(s),
        TypeKind::EnumLiteral { member, .. } => Some(member),
        _ => None,
    }
}

/// Whether `t` is just the inferred type of the function `s` (or of the function that
/// initializes the variable `s`): go to type definition shows its return type instead.
fn is_own_function_type(ck: &Ck<'_, '_>, s: SymId, t: TypeId) -> bool {
    let c = ck.c;
    let store = ck.store();
    let fn_decl = match store.object_of(t).map(|o| o.kind) {
        Some(ObjectKind::Anonymous {
            decl: (u32::MAX, f),
            ..
        }) => c.first_decl(SymId(f)),
        Some(ObjectKind::Literal { sym }) => c.first_decl(sym),
        Some(ObjectKind::Anonymous { decl: (f, n), .. }) if (f as usize) < c.files.len() => {
            ck.node_id(n).map(|n| Loc::new(FileId(f), n))
        }
        _ => None,
    };
    if let Some(fd) = fn_decl {
        // `const f = () => {}`: the arrow is the variable's initializer.
        let is_init = |d: Loc| {
            d.file == fd.file
                && matches!(*c.node(d), Node::VarDeclarator { init: Some(i), .. } if i == fd.node)
        };
        if c.decls(s)
            .into_iter()
            .any(|d| is_init(d) || c.parent(d).is_some_and(is_init))
        {
            return true;
        }
    }
    type_symbol(ck, t) == Some(s)
}

/// The first type argument of `Array<T>`, `Promise<T>`, `Pick<T, K>` and the other
/// well-known global generics (tsc: `GetFirstTypeArgumentFromKnownType`).
fn known_type_argument(ck: &Ck<'_, '_>, t: TypeId) -> Option<TypeId> {
    const KNOWN: &[&str] = &[
        "Array",
        "ArrayLike",
        "ReadonlyArray",
        "Promise",
        "PromiseLike",
        "Iterable",
        "IterableIterator",
        "AsyncIterable",
        "Set",
        "WeakSet",
        "ReadonlySet",
        "Map",
        "WeakMap",
        "ReadonlyMap",
        "Partial",
        "Required",
        "Readonly",
        "Pick",
        "Omit",
        "NonNullable",
    ];
    let c = ck.c;
    let store = ck.store();
    let is_known_global = |s: SymId| {
        KNOWN.contains(&c.sym_name(s)) && c.decls(s).iter().all(|d| c.file(d.file).is_lib)
    };
    if let Some(ObjectKind::Reference { target, args }) = store.object_of(t).map(|o| o.kind)
        && let Some(ObjectKind::Declared { sym, .. }) = store.object_of(target).map(|o| o.kind)
        && is_known_global(sym)
    {
        return store.list_items(args).first().copied();
    }
    let a = store.alias_of(t)?;
    if is_known_global(a.sym) {
        return store.list_items(a.args).first().copied();
    }
    None
}

/// Declarations of the types in `t` (tsc's `getDeclarationsFromType`): the declarations of
/// the symbol of each union member.
fn type_decls(ck: &mut Ck<'_, '_>, t: TypeId, out: &mut Vec<Loc>, depth: u32) {
    if depth > 8 {
        return;
    }
    let c = ck.c;
    let store = ck.store();
    match store.kind(t) {
        TypeKind::Union { list, .. } => {
            let items = store.list_items(list).to_vec();
            for m in items {
                type_decls(ck, m, out, depth + 1);
            }
        }
        TypeKind::Object(_) => {
            let Some(o) = store.object_of(t) else { return };
            match o.kind {
                ObjectKind::Anonymous { decl: (f, n), .. }
                | ObjectKind::Mapped { decl: (f, n), .. }
                    if f != u32::MAX =>
                {
                    if let Some(node) = ck.node_id(n)
                        && (f as usize) < c.files.len()
                    {
                        out.push(Loc::new(FileId(f), node));
                    }
                }
                _ => {
                    if let Some(s) = type_symbol(ck, t) {
                        out.extend(c.decls(s));
                    }
                }
            }
        }
        TypeKind::Enum(_) | TypeKind::EnumLiteral { .. } => {
            if let Some(s) = type_symbol(ck, t) {
                out.extend(c.decls(s));
            }
        }
        TypeKind::TypeParameter(_) => {
            if let Some(d) = store.type_param_data(t) {
                out.extend(c.decls(d.sym));
            }
        }
        _ => {}
    }
}

fn decl_locations(cx: &Ctx<'_, '_>, decls: Vec<Loc>) -> Value {
    let c = cx.checker;
    let mut lc = LinesCache::default();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for d in decls {
        let (s, e) = decl_span(c, d);
        if seen.insert((d.file, s, e)) {
            out.push(lc.location(c, d.file, s, e));
        }
    }
    Value::Array(out)
}

/// Where to look for references.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    File,
    Project,
}

/// One reference to the symbol.
#[derive(Clone, Copy, Debug)]
pub struct Ref {
    pub file: FileId,
    pub start: u32,
    pub end: u32,
    pub is_decl: bool,
    pub is_write: bool,
    /// Which meaning of a shorthand matched (`None` when not a shorthand).
    pub shorthand: Option<ShorthandSide>,
}

/// Which side of a shorthand (`{ x }`) a reference is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShorthandSide {
    /// The property (`x:` of `{ x: x }`).
    Property,
    /// The variable (`: x` of `{ x: x }`).
    Variable,
}

impl Ref {
    pub fn loc_span(&self) -> (FileId, u32, u32) {
        (self.file, self.start, self.end)
    }
}

/// The symbol under the cursor, resolved through aliases, and the declarations that
/// identify it.
struct Target {
    name: String,
    syms: Vec<SymId>,
    decls: HashSet<Loc>,
}

impl Target {
    fn at(ck: &mut Ck<'_, '_>, l: Loc) -> Option<Target> {
        let c = ck.c;
        let name = name_text(c.ast(l.file), l.node)?.to_string();
        let m = meaning_at(ck, l);
        // On a shorthand, references are those of the variable (what tsserver does).
        let s = match (m.primary, m.shorthand) {
            (Some(p), Some(v)) if matches!(c.node(c.parent(l)?), Node::ShorthandProp { .. }) => {
                let _ = p;
                v
            }
            (Some(p), _) => p,
            (None, Some(v)) => v,
            (None, None) => return None,
        };
        let s = c.resolve_alias(s).unwrap_or(s);
        let decls: HashSet<Loc> = c.decls(s).into_iter().collect();
        Some(Target {
            name,
            syms: vec![s],
            decls,
        })
    }

    fn matches(&self, ck: &Ck<'_, '_>, s: SymId) -> bool {
        let c = ck.c;
        let s = c.resolve_alias(s).unwrap_or(s);
        if self.syms.contains(&s) {
            return true;
        }
        !self.decls.is_empty() && c.decls(s).iter().any(|d| self.decls.contains(d))
    }
}

/// References to the symbol at `offset`, in `scope`, in file order then source order.
pub fn references(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, offset: u32, scope: Scope) -> Vec<Ref> {
    let c = cx.checker;
    if let Some(refs) = special_references(cx, offset, scope) {
        return refs;
    }
    let Some(n) = ident_at(c.ast(cx.file), offset) else {
        return Vec::new();
    };
    let Some(target) = Target::at(ck, Loc::new(cx.file, n)) else {
        return Vec::new();
    };
    let files: Vec<FileId> = match scope {
        Scope::File => vec![cx.file],
        Scope::Project => (0..c.files.len())
            .map(|i| FileId(i as u32))
            .filter(|&f| {
                let sf = c.file(f);
                !sf.is_lib && !sf.path.contains("/node_modules/") && sf.text.contains(&target.name)
            })
            .collect(),
    };
    let mut out = Vec::new();
    for f in files {
        let ast = c.ast(f);
        let first = out.len();
        for id in names_in(ast) {
            if name_text(ast, id) != Some(target.name.as_str()) {
                continue;
            }
            let l = Loc::new(f, id);
            let m = meaning_at(ck, l);
            let Meaning {
                primary,
                shorthand,
                is_decl,
            } = m;
            let p = primary.is_some_and(|s| target.matches(ck, s));
            let v = shorthand.is_some_and(|s| target.matches(ck, s));
            if !p && !v {
                continue;
            }
            let side = match (shorthand.is_some(), p, v) {
                (false, ..) => None,
                (true, true, true) => None,
                (true, true, false) => Some(primary_side(ck, l)),
                (true, false, _) => Some(other(primary_side(ck, l))),
            };
            let span = ast.span(id);
            out.push(Ref {
                file: f,
                start: span.start,
                end: span.end,
                is_decl: is_decl && p,
                is_write: if is_decl && p {
                    decl_is_write(ck, l)
                } else {
                    is_write(ck, l)
                },
                shorthand: side,
            });
        }
        // `o["name"]` reads the same property as `o.name`.
        let text = c.file(f).text.as_str();
        for (access, lit) in literal_accesses_in(ast) {
            let (Node::Index { obj, .. }, Node::StringLit { value }) =
                (*ast.node(access), *ast.node(lit))
            else {
                continue;
            };
            if ast.atom(value) != target.name {
                continue;
            }
            // The name between the quotes, spelled without escapes, is what gets edited.
            let span = ast.span(lit);
            let (start, end) = (span.start + 1, span.end.saturating_sub(1));
            if text.get(start as usize..end as usize) != Some(target.name.as_str()) {
                continue;
            }
            let t = ck.check_expr(Loc::new(f, obj));
            if !property_of(ck, t, &target.name).is_some_and(|s| target.matches(ck, s)) {
                continue;
            }
            out.push(Ref {
                file: f,
                start,
                end,
                is_decl: false,
                is_write: is_write(ck, Loc::new(f, access)),
                shorthand: None,
            });
        }
        out[first..].sort_by_key(|r| r.start);
    }
    // On a class's name, `this` in its static methods and accessors is the class too.
    let on_class_name = c.parent(Loc::new(cx.file, n)).is_some_and(|p| {
        matches!(c.node(p), Node::ClassDecl { name: Some(x), .. } | Node::ClassExpr { name: Some(x), .. } if *x == n)
    });
    if on_class_name {
        let mut classes: Vec<Loc> = target
            .decls
            .iter()
            .copied()
            .filter(|d| matches!(c.node(*d), Node::ClassDecl { .. } | Node::ClassExpr { .. }))
            .filter(|d| scope == Scope::Project || d.file == cx.file)
            .collect();
        classes.sort_by_key(|d| (d.file.0, d.node.index()));
        for d in classes {
            let doc = Doc::new(c, d.file);
            out.extend(keywords::static_this_refs(&doc, d.node).into_iter().map(
                |(file, start, end)| Ref {
                    file,
                    start,
                    end,
                    is_decl: false,
                    is_write: false,
                    shorthand: None,
                },
            ));
        }
        out.sort_by_key(|r| (r.file.0, r.start));
        out.dedup_by_key(|r| (r.file.0, r.start));
    }
    out
}

/// The files a project-wide search looks at.
fn project_files(cx: &Ctx<'_, '_>) -> impl Iterator<Item = FileId> {
    let c = cx.checker;
    (0..c.files.len())
        .map(|i| FileId(i as u32))
        .filter(move |&f| {
            let sf = c.file(f);
            !sf.is_lib && !sf.path.contains("/node_modules/")
        })
}

/// References that do not come from a symbol: `this`, `super`, labels and primitive type
/// keywords (`None` when the cursor is on none of them).
fn special_references(cx: &Ctx<'_, '_>, offset: u32, scope: Scope) -> Option<Vec<Ref>> {
    let c = cx.checker;
    let d = Doc::new(c, cx.file);
    let mut decl = None;
    let occ = match keywords::special_at(&d, offset)? {
        Special::This(n) => {
            let others: Vec<Doc<'_>> = match scope {
                Scope::File => Vec::new(),
                Scope::Project => project_files(cx)
                    .filter(|&f| f != cx.file)
                    .map(|f| Doc::new(c, f))
                    .collect(),
            };
            keywords::this_refs(&d, n, &others)
        }
        Special::Super(n) => keywords::super_refs(&d, n),
        Special::Label(Some(l)) => {
            let v = keywords::label_refs(&d, l);
            decl = v.first().copied();
            v
        }
        Special::Label(None) => Vec::new(),
        Special::Primitive(kind) => match scope {
            Scope::File => keywords::primitive_refs(&d, kind),
            Scope::Project => project_files(cx)
                .flat_map(|f| keywords::primitive_refs(&Doc::new(c, f), kind))
                .collect(),
        },
    };
    let mut out: Vec<Ref> = occ
        .into_iter()
        .map(|o| Ref {
            file: o.0,
            start: o.1,
            end: o.2,
            is_decl: Some(o) == decl,
            is_write: false,
            shorthand: None,
        })
        .collect();
    out.sort_by_key(|r| (r.file.0, r.start));
    out.dedup_by_key(|r| (r.file.0, r.start));
    Some(out)
}

/// What `primary` is for a shorthand name: the property for `{ x }`, the variable for
/// `const { x } = o`.
fn primary_side(ck: &Ck<'_, '_>, l: Loc) -> ShorthandSide {
    let c = ck.c;
    match c.parent(l).map(|p| c.node(p)) {
        Some(Node::ShorthandProp { .. }) => ShorthandSide::Property,
        _ => ShorthandSide::Variable,
    }
}

fn other(s: ShorthandSide) -> ShorthandSide {
    match s {
        ShorthandSide::Property => ShorthandSide::Variable,
        ShorthandSide::Variable => ShorthandSide::Property,
    }
}

/// Whether the name is assigned (`x = 1`, `x++`, `this.x += 1`).
fn is_write(ck: &Ck<'_, '_>, l: Loc) -> bool {
    let c = ck.c;
    let Some(mut p) = c.parent(l) else {
        return false;
    };
    let mut at = l.node;
    if let Node::Member { prop, .. } = *c.node(p)
        && prop == l.node
    {
        at = p.node;
        let Some(pp) = c.parent(p) else { return false };
        p = pp;
    }
    match *c.node(p) {
        Node::Assign { left, .. } => left == at,
        Node::Update { .. } => true,
        _ => false,
    }
}

/// Whether a declaration counts as a write (tsc 7's highlight kind): one that gives a value.
/// Signatures, constructors and fields or variables without an initializer are reads.
fn decl_is_write(ck: &Ck<'_, '_>, l: Loc) -> bool {
    let c = ck.c;
    let Some(p) = c.parent(l) else { return true };
    match *c.node(p) {
        Node::PropertySignature { .. } | Node::MethodSignature { .. } => false,
        Node::PropertyDef { value, .. } => value.is_some(),
        Node::MethodDef { kind, .. } => kind != MethodKind::Constructor,
        // `for (const x of xs)` assigns each element.
        Node::VarDeclarator { init: None, .. } => c
            .parent(p)
            .and_then(|d| c.parent(d))
            .is_some_and(|n| matches!(c.node(n), Node::ForOf { .. } | Node::ForIn { .. })),
        _ => true,
    }
}

/// LSP locations of byte ranges.
pub fn locations(cx: &Ctx<'_, '_>, spans: impl Iterator<Item = (FileId, u32, u32)>) -> Value {
    let mut lc = LinesCache::default();
    Value::Array(
        spans
            .map(|(f, s, e)| lc.location(cx.checker, f, s, e))
            .collect(),
    )
}

/// Why a symbol cannot be renamed (tsserver's messages).
fn rename_blocker(ck: &Ck<'_, '_>, s: SymId) -> Option<&'static str> {
    let c = ck.c;
    let s = c.resolve_alias(s).unwrap_or(s);
    for d in c.decls(s) {
        let f = c.file(d.file);
        if f.is_lib {
            return Some(
                "You cannot rename elements that are defined in the standard TypeScript library.",
            );
        }
        if f.path.contains("/node_modules/") {
            return Some("You cannot rename elements that are defined in a 'node_modules' folder.");
        }
    }
    None
}

/// Whether the cursor is on a label (of a labelled statement, `break` or `continue`).
fn is_label(cx: &Ctx<'_, '_>, offset: u32) -> bool {
    let d = Doc::new(cx.checker, cx.file);
    matches!(
        keywords::special_at(&d, offset),
        Some(Special::Label(Some(_)))
    )
}

pub fn prepare_rename(
    cx: &Ctx<'_, '_>,
    ck: &mut Ck<'_, '_>,
    lines: &LspLines,
    offset: u32,
) -> Result<Value, (i64, String)> {
    let c = cx.checker;
    let ast = c.ast(cx.file);
    let text = c.file(cx.file).text.as_str();
    let Some(n) = ident_at(ast, offset) else {
        return Ok(Value::Null);
    };
    if !is_label(cx, offset) {
        let m = meaning_at(ck, Loc::new(cx.file, n));
        let Some(s) = m.primary.or(m.shorthand) else {
            return Ok(Value::Null);
        };
        if let Some(why) = rename_blocker(ck, s) {
            return Err((codes::REQUEST_FAILED, why.to_string()));
        }
    }
    let span = ast.span(n);
    Ok(json!({
        "range": lines.range(text, span.start, span.end),
        "placeholder": span.text(text),
    }))
}

pub fn rename(
    cx: &Ctx<'_, '_>,
    ck: &mut Ck<'_, '_>,
    offset: u32,
    new_name: &str,
) -> Result<Value, (i64, String)> {
    let c = cx.checker;
    let ast = c.ast(cx.file);
    let Some(n) = ident_at(ast, offset) else {
        return Ok(Value::Null);
    };
    if !is_label(cx, offset) {
        let m = meaning_at(ck, Loc::new(cx.file, n));
        let Some(s) = m.primary.or(m.shorthand) else {
            return Ok(Value::Null);
        };
        if let Some(why) = rename_blocker(ck, s) {
            return Err((codes::REQUEST_FAILED, why.to_string()));
        }
    }
    let on_this = ast.span(n).text(c.file(cx.file).text.as_str()) == "this";
    let refs = references(cx, ck, offset, Scope::Project);
    let mut lc = LinesCache::default();
    let mut changes: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for r in refs {
        let f = c.file(r.file);
        let old = &f.text[r.start as usize..r.end as usize];
        // `this` in a static method refers to the class but is not renamed with it.
        if old == "this" && !on_this {
            continue;
        }
        let new_text = match r.shorthand {
            // `{ x }` -> `{ x: y }` when the variable is renamed, `{ y: x }` for the property.
            Some(ShorthandSide::Variable) => format!("{old}: {new_name}"),
            Some(ShorthandSide::Property) => format!("{new_name}: {old}"),
            None => new_name.to_string(),
        };
        changes
            .entry(crate::uri::path_to_uri(&f.path))
            .or_default()
            .push(json!({"range": lc.range(c, r.file, r.start, r.end), "newText": new_text}));
    }
    Ok(json!({"changes": changes}))
}
