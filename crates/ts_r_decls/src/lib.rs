//! Per-file declaration index: what a file contributes to the program's API, with a stable
//! hash per entry and one for the whole file.
//!
//! For each export of a module (or each global of a script, plus ambient modules,
//! augmentations and `declare global` contributions) the index records the symbol kind, a
//! canonical BLAKE3 hash of its *signature*, whether that signature is fully explicit, and
//! the names its signature depends on. Function bodies, initializers of typed variables and
//! properties, constructor and setter bodies, decorators and private member types are not
//! part of a signature, and neither are positions, whitespace or comments. When a type has
//! to be inferred (no return type, untyped initializer), the inferred-from parts are hashed
//! too and the entry is marked not explicit, so the hash stays conservative. A function's
//! expando assignments (`f.x = v`, `f[k] = v`) add members to its type and are hashed the
//! same way.
//!
//! Non-exported local declarations that a signature references (`export function f():
//! Local`, or a value an inferred type is read from: `export const y = local`) are hashed
//! into the entry. Imported names become dependencies (`specifier`, imported name) for the
//! caller to chase across files.
//!
//! The index is plain data with a versioned binary encoding ([`DeclIndex::encode`],
//! [`DeclIndex::decode`]) and is stored in the shared ts-r cache under namespace
//! [`CACHE_NAMESPACE`] with keys from [`cache_key`].

use std::collections::{BTreeMap, BTreeSet};

use ts_r_ast::{Ast, FieldDump, MethodKind, Modifiers, Node, NodeId, ParentMap, SourceKind};
use ts_r_binder::symbols::{SymbolFacts, SymbolFlags};
use ts_r_binder::{AliasTarget, BindOptions, BoundFile, ImportName, SymbolId, TableId};
use ts_r_cache::{Cache, Key, KeyBuilder};

/// Bump whenever the encoding or the hashing rules change.
pub const FORMAT_VERSION: u32 = 5;
/// Namespace in the shared cache.
pub const CACHE_NAMESPACE: &str = "decls";
const MAGIC: &[u8; 4] = b"TSRD";
/// Bound on nested hashing (AST depth, local-reference closure).
const MAX_DEPTH: u32 = 2000;

pub type Hash = [u8; 32];

/// Where an entry comes from.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(u8)]
pub enum Section {
    /// Module exports (`export`, `export default`, `export =`, re-exports).
    Export = 0,
    /// Script-level declarations of a non-module file.
    Global = 1,
    /// `declare global { }` contributions of a module.
    GlobalAugmentation = 2,
    /// `declare module "x" { }` in a script or declaration file.
    AmbientModule = 3,
    /// `declare module "x" { }` in a module.
    ModuleAugmentation = 4,
}

impl Section {
    fn from_u8(v: u8) -> Option<Section> {
        Some(match v {
            0 => Section::Export,
            1 => Section::Global,
            2 => Section::GlobalAugmentation,
            3 => Section::AmbientModule,
            4 => Section::ModuleAugmentation,
            _ => return None,
        })
    }
}

/// A dependency of a signature.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Dep {
    /// An imported binding: module specifier and imported name (`default`, `*`, `export=`
    /// or the export name).
    Import { specifier: String, name: String },
    /// A name the file does not declare (a global).
    Global(String),
    /// Another export of the same module, by export name: a signature that names an
    /// exported symbol depends on that export's entry (it is not folded in).
    Sibling(String),
}

/// One API entry.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    pub section: Section,
    pub name: String,
    /// `SymbolFlags` bits of the symbol.
    pub flags: u32,
    /// Every part of the signature is written out (nothing inferred from bodies or values).
    pub explicit: bool,
    pub hash: Hash,
    /// Sorted, deduplicated.
    pub deps: Vec<Dep>,
}

/// A module specifier the file mentions.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Import {
    pub specifier: String,
    /// `ts_r_binder::RequestKind` as a small integer (see [`request_kind_code`]).
    pub kind: u8,
    pub type_only: bool,
}

/// The declaration index of one file.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DeclIndex {
    pub is_module: bool,
    /// Sorted by (section, name).
    pub entries: Vec<Entry>,
    /// Sorted, deduplicated.
    pub imports: Vec<Import>,
    /// Specifiers of `export * from` (sorted, deduplicated).
    pub export_stars: Vec<String>,
    /// `export as namespace X`.
    pub umd_name: Option<String>,
    /// Hash over everything above: equal hashes mean an identical API.
    pub api_hash: Hash,
}

impl DeclIndex {
    /// True when every entry is explicit.
    pub fn is_explicit(&self) -> bool {
        self.entries.iter().all(|e| e.explicit)
    }

    pub fn entry(&self, section: Section, name: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|e| e.section == section && e.name == name)
    }
}

pub fn request_kind_code(k: ts_r_binder::RequestKind) -> u8 {
    use ts_r_binder::RequestKind as R;
    match k {
        R::Import => 0,
        R::ExportFrom => 1,
        R::ImportEquals => 2,
        R::DynamicImport => 3,
        R::ImportType => 4,
        R::Augmentation => 5,
    }
}

// -------------------------------------------------------------------------------------------
// Building
// -------------------------------------------------------------------------------------------

/// Builds the index of a bound file.
pub fn index(ast: &Ast, bound: &BoundFile) -> DeclIndex {
    let parents = ParentMap::build(ast);
    let mut free: Vec<NodeId> = bound.refs.free.iter().map(|r| r.node).collect();
    free.sort_unstable();
    let mut entries = Vec::new();
    let b = bound;
    // Export names per exported symbol (modules only).
    let mut exported: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    if b.is_module {
        for &(name, sym) in b.exports() {
            exported
                .entry(sym.0)
                .or_default()
                .push(b.name_text(ast, name).to_owned());
        }
    }
    let mut add_table = |section: Section, table: &[(ts_r_binder::Name, SymbolId)]| {
        for &(name, sym) in table {
            let mut h = SigHasher::new(ast, b, &parents, &free, &exported);
            h.symbol(sym, 0);
            h.close_locals();
            entries.push(h.finish(
                section,
                b.name_text(ast, name).to_owned(),
                b.symbol(sym).flags.0,
            ));
        }
    };
    if b.is_module {
        add_table(Section::Export, b.exports());
    } else {
        add_table(Section::Global, b.script_globals());
    }
    add_table(Section::GlobalAugmentation, b.table(b.global_augmentations));
    add_table(Section::AmbientModule, b.table(b.ambient_modules));
    add_table(Section::ModuleAugmentation, b.table(b.module_augmentations));
    entries.sort_by(|a, b| (a.section, &a.name).cmp(&(b.section, &b.name)));

    let mut imports: Vec<Import> = b
        .module_requests
        .iter()
        .map(|r| Import {
            specifier: ast.atom(r.specifier).to_owned(),
            kind: request_kind_code(r.kind),
            type_only: r.type_only,
        })
        .collect();
    imports.sort();
    imports.dedup();
    let mut export_stars: Vec<String> = b
        .export_stars
        .iter()
        .filter(|s| s.container == b.file_symbol)
        .filter_map(|s| b.module_requests.get(s.request as usize))
        .map(|r| ast.atom(r.specifier).to_owned())
        .collect();
    export_stars.sort();
    export_stars.dedup();
    let umd_name = b.umd_name.map(|(n, _)| b.name_text(ast, n).to_owned());
    let mut idx = DeclIndex {
        is_module: b.is_module,
        entries,
        imports,
        export_stars,
        umd_name,
        api_hash: [0; 32],
    };
    let mut body = Vec::new();
    idx.encode_body(&mut body);
    idx.api_hash = *blake3::hash(&body).as_bytes();
    idx
}

struct SigHasher<'a> {
    ast: &'a Ast,
    b: &'a BoundFile,
    parents: &'a ParentMap,
    /// Identifiers referencing names the file does not declare (sorted).
    free: &'a [NodeId],
    /// Export names of the module's exported symbols.
    exported: &'a BTreeMap<u32, Vec<String>>,
    h: blake3::Hasher,
    explicit: bool,
    deps: BTreeSet<Dep>,
    /// Local symbols referenced by the signature, hashed after the entry's own declarations.
    pending: Vec<SymbolId>,
    seen: BTreeSet<u32>,
}

impl<'a> SigHasher<'a> {
    fn new(
        ast: &'a Ast,
        b: &'a BoundFile,
        parents: &'a ParentMap,
        free: &'a [NodeId],
        exported: &'a BTreeMap<u32, Vec<String>>,
    ) -> Self {
        SigHasher {
            ast,
            b,
            parents,
            free,
            exported,
            h: blake3::Hasher::new(),
            explicit: true,
            deps: BTreeSet::new(),
            pending: Vec::new(),
            seen: BTreeSet::new(),
        }
    }

    fn tag(&mut self, s: &str) {
        self.h.update(&(s.len() as u32).to_le_bytes());
        self.h.update(s.as_bytes());
    }

    fn parent(&self, n: NodeId) -> Option<NodeId> {
        self.parents.parent(n)
    }

    /// Hashes a symbol: its flags and every declaration (aliases: their target).
    fn symbol(&mut self, sym: SymbolId, depth: u32) {
        if !self.seen.insert(sym.0) {
            return;
        }
        let s = *self.b.symbol(sym);
        self.tag("sym");
        self.h.update(&s.flags.0.to_le_bytes());
        if s.flags.intersects(SymbolFlags::ALIAS) {
            for a in self.b.aliases_of(sym) {
                // `export type { x }` / `import type`: the name is not a value.
                if self.type_only(a.decl) {
                    self.tag("type-only");
                }
                match a.target {
                    AliasTarget::Module { request, import } => {
                        let spec = self
                            .b
                            .module_requests
                            .get(request as usize)
                            .map_or("", |r| self.ast.atom(r.specifier))
                            .to_owned();
                        let name = match import {
                            ImportName::Default => "default".to_owned(),
                            ImportName::Named(n) => self.b.name_text(self.ast, n).to_owned(),
                            ImportName::Namespace => "*".to_owned(),
                            ImportName::ExportEquals => "export=".to_owned(),
                        };
                        self.tag("import");
                        self.tag(&spec);
                        self.tag(&name);
                        self.deps.insert(Dep::Import {
                            specifier: spec,
                            name,
                        });
                    }
                    AliasTarget::Entity(node) => {
                        self.tag("entity");
                        self.node(node, depth + 1);
                    }
                }
            }
        }
        for &d in self.b.decls(sym) {
            let d = self.decl_root(d);
            if let Node::VarDeclarator { .. } = self.ast.node(d) {
                // `let`/`const`/`declare`/`export` from the statement, then this declarator
                // only (siblings in `const a = 1, b = f()` are separate entries).
                if let Some(p) = self.parent(d)
                    && let Node::VarDecl {
                        modifiers, kind, ..
                    } = *self.ast.node(p)
                {
                    self.tag("VarDecl");
                    self.h.update(&modifiers.0.to_le_bytes());
                    self.tag(&format!("{kind:?}"));
                }
            }
            self.node(d, depth + 1);
        }
        // Expando properties (`f.x = v`, `f[k] = v`) are members of the function's type:
        // their assignments (key and inferred value) are part of the signature.
        let mut lefts: Vec<NodeId> = Vec::new();
        for &(_, x) in self.b.table(s.exports) {
            if self.b.symbol(x).facts.intersects(SymbolFacts::EXPANDO) {
                lefts.extend_from_slice(self.b.decls(x));
            }
        }
        let late = &self.b.late_expandos;
        let lo = late.partition_point(|&(t, _)| t < sym);
        lefts.extend(
            late[lo..]
                .iter()
                .take_while(|&&(t, _)| t == sym)
                .map(|&(_, l)| l),
        );
        for left in lefts {
            self.tag("expando");
            self.explicit = false;
            let assign = self.parent(left).unwrap_or(left);
            self.node(assign, depth + 1);
        }
    }

    /// Whether an import or export declaration node (or the statement around it) is
    /// type-only.
    fn type_only(&self, decl: NodeId) -> bool {
        let mut cur = Some(decl);
        for _ in 0..4 {
            let Some(n) = cur else { break };
            match *self.ast.node(n) {
                Node::ExportSpecifier { type_only, .. }
                | Node::ImportSpecifier { type_only, .. }
                | Node::ExportAll { type_only, .. }
                | Node::ImportEquals { type_only, .. }
                    if type_only =>
                {
                    return true;
                }
                Node::ExportNamed { type_only, .. } | Node::ImportDecl { type_only, .. } => {
                    return type_only;
                }
                _ => {}
            }
            cur = self.parent(n);
        }
        false
    }

    /// The node to hash for a declaration: binding names hash their whole declarator.
    fn decl_root(&self, d: NodeId) -> NodeId {
        let mut cur = d;
        // Climb out of binding patterns to the declarator or parameter (bounded by depth).
        for _ in 0..MAX_DEPTH {
            match self.ast.node(cur) {
                Node::Ident { .. }
                | Node::ObjectPattern { .. }
                | Node::ArrayPattern { .. }
                | Node::BindingProperty { .. }
                | Node::AssignPattern { .. }
                | Node::Rest { .. } => match self.parent(cur) {
                    Some(p)
                        if matches!(
                            self.ast.node(p),
                            Node::ObjectPattern { .. }
                                | Node::ArrayPattern { .. }
                                | Node::BindingProperty { .. }
                                | Node::AssignPattern { .. }
                                | Node::Rest { .. }
                                | Node::VarDeclarator { .. }
                        ) =>
                    {
                        cur = p
                    }
                    _ => return cur,
                },
                _ => return cur,
            }
        }
        cur
    }

    fn close_locals(&mut self) {
        // Each symbol is hashed at most once (`seen`), so this ends.
        while let Some(s) = self.pending.pop() {
            self.tag("local");
            self.symbol(s, 0);
        }
    }

    /// A reference in a signature: hash a local target too, record an import or global.
    fn reference(&mut self, ident: NodeId) {
        let Node::Ident { name } = *self.ast.node(ident) else {
            return;
        };
        match self.b.resolved(ident) {
            Some(sym) => {
                let s = *self.b.symbol(sym);
                if s.flags.intersects(SymbolFlags::ALIAS) {
                    // Imports: recorded as deps when the alias is hashed.
                    self.pending.push(sym);
                } else if !self.b.is_module && self.is_file_level(sym) {
                    // Another script global: its own entry.
                    self.deps
                        .insert(Dep::Global(self.ast.atom(name).to_owned()));
                } else if self.seen.contains(&sym.0) {
                    // Hashed into this entry already (itself, or a folded local).
                } else if !s.facts.intersects(SymbolFacts::EXPORTED) {
                    // A non-exported local: part of this signature.
                    self.pending.push(sym);
                } else if let Some(names) = self.exported.get(&sym.0) {
                    // Another export: its own entry, a dependency.
                    for n in names {
                        self.deps.insert(Dep::Sibling(n.clone()));
                    }
                } else {
                    // Exported, but not under its own symbol (`export { x as y }` aliases):
                    // folded in.
                    self.pending.push(sym);
                }
            }
            None => {
                self.deps
                    .insert(Dep::Global(self.ast.atom(name).to_owned()));
            }
        }
    }

    fn is_file_level(&self, sym: SymbolId) -> bool {
        let Some(file_scope) = self.b.scopes.first() else {
            return false;
        };
        self.b.lookup(file_scope.locals, self.b.symbol(sym).name) == Some(sym)
    }

    fn leftmost_ident(&self, mut n: NodeId) -> Option<NodeId> {
        for _ in 0..MAX_DEPTH {
            match *self.ast.node(n) {
                Node::Ident { .. } => return Some(n),
                Node::QualifiedName { left, .. } => n = left,
                Node::Member { obj, .. } => n = obj,
                _ => return None,
            }
        }
        None
    }

    fn is_private(&self, modifiers: Modifiers, key: NodeId) -> bool {
        modifiers.contains(Modifiers::PRIVATE)
            || matches!(self.ast.node(key), Node::PrivateName { .. })
    }

    /// Canonical hash of a subtree, skipping non-signature parts.
    fn node(&mut self, id: NodeId, depth: u32) {
        if depth > MAX_DEPTH {
            self.tag("deep");
            self.explicit = false;
            return;
        }
        let ast = self.ast;
        let node = *ast.node(id);
        // Detached JSDoc annotations are part of the signature too.
        for t in ast.jsdoc_tags_of(id) {
            if t.kind == ts_r_ast::JsDocTagKind::Type
                && let Some(ty) = t.ty
            {
                let mut child = false;
                ast.for_each_child(id, |c| child |= c == ty);
                if !child {
                    self.tag("jsdoc-type");
                    self.node(ty, depth + 1);
                }
            }
        }
        // References that a signature depends on.
        match node {
            Node::TypeRef { name, .. }
            | Node::TypeQuery {
                expr_name: name, ..
            } => {
                if let Some(i) = self.leftmost_ident(name) {
                    self.reference(i);
                }
            }
            Node::ExprWithTypeArgs { expr, .. } => {
                if let Some(i) = self.leftmost_ident(expr) {
                    self.reference(i);
                }
            }
            // Any other reference in a hashed part (initializers and bodies a type is
            // inferred from, entity names): it can shape the type too.
            Node::Ident { .. }
                if self.b.resolved(id).is_some() || self.free.binary_search(&id).is_ok() =>
            {
                self.reference(id);
            }
            _ => {}
        }
        // Fields to skip, and inference markers.
        let mut skip: [&str; 3] = ["decorators", "", ""];
        match node {
            Node::FunctionDecl { return_type, .. }
            | Node::FunctionExpr { return_type, .. }
            | Node::Arrow { return_type, .. } => {
                if return_type.is_some() {
                    skip[1] = "body";
                } else {
                    self.explicit = false;
                }
            }
            Node::VarDeclarator { type_ann, init, .. } | Node::Param { type_ann, init, .. } => {
                if type_ann.is_some() {
                    skip[1] = "init";
                    self.tag(if init.is_some() {
                        "has-init"
                    } else {
                        "no-init"
                    });
                } else if init.is_some() {
                    self.explicit = false;
                }
            }
            Node::PropertyDef {
                modifiers,
                key,
                type_ann,
                value,
                ..
            } => {
                if self.is_private(modifiers, key) {
                    skip[1] = "type_ann";
                    skip[2] = "value";
                } else if type_ann.is_some() {
                    skip[1] = "value";
                } else if value.is_some() {
                    self.explicit = false;
                }
            }
            Node::MethodDef {
                modifiers,
                kind,
                key,
                value,
                ..
            } => {
                if self.is_private(modifiers, key) {
                    skip[1] = "value";
                } else if matches!(kind, MethodKind::Constructor | MethodKind::Set) {
                    // Only the parameters matter.
                    if let Node::FunctionExpr { params, .. } = *ast.node(value) {
                        self.tag(ast.kind(id).name());
                        self.h.update(&modifiers.0.to_le_bytes());
                        self.tag(&format!("{kind:?}"));
                        self.node(key, depth + 1);
                        for &p in ast.list(params) {
                            self.node(p, depth + 1);
                        }
                        return;
                    }
                }
            }
            Node::ModuleBlock { stmts } | Node::Program { stmts } => {
                // Only declarations shape a namespace's API.
                self.tag(ast.kind(id).name());
                for &s in ast.list(stmts) {
                    if is_declaration(ast.node(s)) {
                        self.node(s, depth + 1);
                    }
                }
                return;
            }
            Node::ExportDefaultExpr { .. } | Node::ExportAssignment { .. } => {
                // A non-entity expression: its type is inferred.
                self.explicit = false;
            }
            _ => {}
        }
        self.tag(ast.kind(id).name());
        for (field, dump) in node.dump_fields(ast) {
            if skip.contains(&field) {
                continue;
            }
            match dump {
                FieldDump::Skip => {}
                FieldDump::Scalar(s) => {
                    self.tag(field);
                    self.tag(&s);
                }
                FieldDump::Child(c) => {
                    self.tag(field);
                    self.node(c, depth + 1);
                }
                FieldDump::List(l) => {
                    self.tag(field);
                    let items = ast.list(l);
                    self.h.update(&(items.len() as u32).to_le_bytes());
                    for &c in items {
                        self.node(c, depth + 1);
                    }
                }
            }
        }
        // Literal payloads that `dump_fields` shows as scalars are already hashed above;
        // number literals print their value through `F64Bits`'s dump.
    }

    fn finish(self, section: Section, name: String, flags: u32) -> Entry {
        Entry {
            section,
            name,
            flags,
            explicit: self.explicit,
            hash: *self.h.finalize().as_bytes(),
            deps: self.deps.into_iter().collect(),
        }
    }
}

fn is_declaration(n: &Node) -> bool {
    matches!(
        n,
        Node::VarDecl { .. }
            | Node::FunctionDecl { .. }
            | Node::ClassDecl { .. }
            | Node::Interface { .. }
            | Node::TypeAlias { .. }
            | Node::Enum { .. }
            | Node::Module { .. }
            | Node::ImportEquals { .. }
            | Node::ImportDecl { .. }
            | Node::ExportNamed { .. }
            | Node::ExportAll { .. }
            | Node::ExportDefaultExpr { .. }
            | Node::ExportAssignment { .. }
            | Node::NamespaceExport { .. }
    )
}

// -------------------------------------------------------------------------------------------
// Encoding
// -------------------------------------------------------------------------------------------

struct Writer<'a>(&'a mut Vec<u8>);

impl Writer<'_> {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, b: &[u8]) {
        self.u32(b.len() as u32);
        self.0.extend_from_slice(b);
    }
    fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn hash(&mut self) -> Option<Hash> {
        self.take(32)?.try_into().ok()
    }
    fn str(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
    /// A length prefix that cannot exceed the remaining bytes (each item takes ≥ 1 byte).
    fn len(&mut self) -> Option<usize> {
        let n = self.u32()? as usize;
        (n <= self.data.len() - self.pos).then_some(n)
    }
}

impl DeclIndex {
    fn encode_body(&self, out: &mut Vec<u8>) {
        let mut w = Writer(out);
        w.u8(self.is_module as u8);
        w.u32(self.entries.len() as u32);
        for e in &self.entries {
            w.u8(e.section as u8);
            w.str(&e.name);
            w.u32(e.flags);
            w.u8(e.explicit as u8);
            w.0.extend_from_slice(&e.hash);
            w.u32(e.deps.len() as u32);
            for d in &e.deps {
                match d {
                    Dep::Import { specifier, name } => {
                        w.u8(0);
                        w.str(specifier);
                        w.str(name);
                    }
                    Dep::Global(n) => {
                        w.u8(1);
                        w.str(n);
                    }
                    Dep::Sibling(n) => {
                        w.u8(2);
                        w.str(n);
                    }
                }
            }
        }
        w.u32(self.imports.len() as u32);
        for i in &self.imports {
            w.str(&i.specifier);
            w.u8(i.kind);
            w.u8(i.type_only as u8);
        }
        w.u32(self.export_stars.len() as u32);
        for s in &self.export_stars {
            w.str(s);
        }
        match &self.umd_name {
            Some(n) => {
                w.u8(1);
                w.str(n);
            }
            None => w.u8(0),
        }
    }

    /// Versioned binary form: magic, [`FORMAT_VERSION`], body, API hash.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + self.entries.len() * 64);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        self.encode_body(&mut out);
        out.extend_from_slice(&self.api_hash);
        out
    }

    /// Decodes [`DeclIndex::encode`] output. `None` for other versions or damaged data.
    pub fn decode(data: &[u8]) -> Option<DeclIndex> {
        let mut r = Reader { data, pos: 0 };
        if r.take(4)? != MAGIC || r.u32()? != FORMAT_VERSION {
            return None;
        }
        let is_module = r.u8()? != 0;
        let n = r.len()?;
        let mut entries = Vec::with_capacity(n);
        for _ in 0..n {
            let section = Section::from_u8(r.u8()?)?;
            let name = r.str()?;
            let flags = r.u32()?;
            let explicit = r.u8()? != 0;
            let hash = r.hash()?;
            let nd = r.len()?;
            let mut deps = Vec::with_capacity(nd);
            for _ in 0..nd {
                deps.push(match r.u8()? {
                    0 => Dep::Import {
                        specifier: r.str()?,
                        name: r.str()?,
                    },
                    1 => Dep::Global(r.str()?),
                    2 => Dep::Sibling(r.str()?),
                    _ => return None,
                });
            }
            entries.push(Entry {
                section,
                name,
                flags,
                explicit,
                hash,
                deps,
            });
        }
        let n = r.len()?;
        let mut imports = Vec::with_capacity(n);
        for _ in 0..n {
            imports.push(Import {
                specifier: r.str()?,
                kind: r.u8()?,
                type_only: r.u8()? != 0,
            });
        }
        let n = r.len()?;
        let mut export_stars = Vec::with_capacity(n);
        for _ in 0..n {
            export_stars.push(r.str()?);
        }
        let umd_name = match r.u8()? {
            0 => None,
            1 => Some(r.str()?),
            _ => return None,
        };
        let api_hash = r.hash()?;
        if r.pos != data.len() {
            return None;
        }
        Some(DeclIndex {
            is_module,
            entries,
            imports,
            export_stars,
            umd_name,
            api_hash,
        })
    }
}

// -------------------------------------------------------------------------------------------
// Cache
// -------------------------------------------------------------------------------------------

/// Cache key of a file's index: tool version, format version, source kind, the bind options
/// that change exports, and the file contents.
pub fn cache_key(src: &str, kind: SourceKind, opts: &BindOptions) -> Key {
    KeyBuilder::new("ts-r/decls")
        .str(env!("CARGO_PKG_VERSION"))
        .u64(u64::from(FORMAT_VERSION))
        .str(&format!("{kind:?}"))
        .u64(u64::from(opts.force_module))
        .str(src)
        .key()
}

/// Opens the shared cache namespace for declaration indexes.
pub fn open_cache() -> std::io::Result<Cache> {
    Cache::open(CACHE_NAMESPACE, FORMAT_VERSION)
}

/// Returns the cached index for `key`, or computes, stores and returns it. Damaged or
/// mismatched entries are recomputed; a failed write is not an error (the index is still
/// returned).
pub fn get_or_compute(cache: &Cache, key: &Key, compute: impl FnOnce() -> DeclIndex) -> DeclIndex {
    if let Some(blob) = cache.get(key)
        && let Some(idx) = DeclIndex::decode(&blob)
    {
        return idx;
    }
    let idx = compute();
    let _ = cache.put(key, &idx.encode());
    idx
}

/// Convenience: the member table ids of a class or interface symbol (re-exported for
/// callers that walk entries back to the binder).
pub fn member_tables(b: &BoundFile, sym: SymbolId) -> Vec<TableId> {
    b.member_tables(sym)
}
