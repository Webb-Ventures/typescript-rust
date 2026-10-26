//! What the linker needs to know about a module, without its tree.
//!
//! [`ModuleMeta`] is produced by [`crate::scan`] from a prepared module and is plain data:
//! symbols are the `u32` ids of `ts_r_minify::scope` (stable because preparation is
//! deterministic), names are strings. It is cached in memory between rebuilds and on disk
//! (see [`ModuleMeta::encode`]) so a warm build links without parsing anything.

/// A module-scope symbol id (`ts_r_minify::SymbolId::0`).
pub type Sym = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RecordKind {
    /// `import ... from "x"` / `import "x"`.
    Import,
    /// `export ... from "x"` / `export * from "x"`.
    ReExport,
    /// `require("x")`.
    Require,
    /// `import("x")`.
    Dynamic,
    /// A stylesheet's `@import` (the record's `node` is its index in the [`crate::css::Sheet`]).
    CssImport,
    /// A stylesheet's `url()`.
    CssUrl,
    /// A stylesheet's `composes: x from "file"`.
    CssComposes,
}

impl RecordKind {
    pub fn is_css(self) -> bool {
        matches!(
            self,
            RecordKind::CssImport | RecordKind::CssUrl | RecordKind::CssComposes
        )
    }
}

/// One import/require of another module.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Record {
    pub kind: RecordKind,
    pub spec: String,
    /// Byte range of the specifier string (for messages).
    pub start: u32,
    pub end: u32,
    /// The statement (`Import`/`ReExport`) or call node (`Require`/`Dynamic`).
    pub node: u32,
    /// Inside a `try` block: a missing module is a warning, not an error.
    pub in_try: bool,
    /// Import phase: [`PHASE_EVALUATION`], [`PHASE_DEFER`] (`import defer`,
    /// `import.defer()`) or [`PHASE_SOURCE`] (`import source`, `import.source()`).
    pub phase: u8,
    /// Import attributes of a static `import`/`export ... from` as printed after `with`
    /// (`{ type: "json" }`; `assert { ... }` keeps its keyword), empty when there are none.
    pub attrs: String,
}

pub const PHASE_EVALUATION: u8 = 0;
pub const PHASE_DEFER: u8 = 1;
pub const PHASE_SOURCE: u8 = 2;

/// What an import binding or re-export refers to in the other module.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Imported {
    /// A named export (`default` included).
    Name(String),
    /// `* as ns`.
    Namespace,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ImportBinding {
    pub sym: Sym,
    pub record: u32,
    pub name: Imported,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ExportTarget {
    Local(Sym),
    Reexport { record: u32, name: Imported },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PartKind {
    /// An `import` statement.
    Import,
    /// `export ... from` / `export *`.
    ExportFrom,
    /// `export { a, b }` without `from`.
    ExportLocal,
    /// A directive (`"use strict"`).
    Directive,
    /// Anything else.
    Stmt,
}

/// One top-level statement.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Part {
    pub node: u32,
    pub kind: PartKind,
    pub declares: Vec<Sym>,
    /// Module-scope symbols referenced (reads and writes), excluding `ns.x` member reads of
    /// namespace imports, which are in `ns_members`.
    pub uses: Vec<Sym>,
    /// `ns.x` reads of `import * as ns` bindings.
    pub ns_members: Vec<(Sym, String)>,
    /// `require()` / `import()` records inside the statement.
    pub records: Vec<u32>,
    pub side_effects: bool,
}

/// Kind of a module-scope symbol.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SymKind {
    Var,
    Lexical,
    Function,
    Class,
    Import,
    /// The synthesized name of an anonymous `export default`.
    Default,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SymInfo {
    pub sym: Sym,
    pub name: String,
    pub kind: SymKind,
}

/// The scan result for one module. See the module docs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ModuleMeta {
    pub records: Vec<Record>,
    pub imports: Vec<ImportBinding>,
    /// Sorted by name.
    pub exports: Vec<(String, ExportTarget)>,
    /// `export * from` records, in source order.
    pub star_exports: Vec<u32>,
    pub parts: Vec<Part>,
    /// Module-scope symbols in declaration order.
    pub syms: Vec<SymInfo>,
    /// Unresolved names referenced anywhere, sorted.
    pub globals: Vec<String>,
    /// `import`/`export` statements (not `import()`).
    pub has_esm_syntax: bool,
    /// `export` statements or exported declarations.
    pub has_esm_exports: bool,
    /// Free `module` / `exports` references.
    pub uses_cjs: bool,
    pub has_tla: bool,
    pub uses_import_meta: bool,
    pub has_direct_eval: bool,
    pub default_sym: Option<Sym>,
    /// A `"use strict"` directive starts the module.
    pub use_strict: bool,
    /// A `return` or `arguments` outside every function (CommonJS): the module can't run
    /// at the top of a chunk.
    pub needs_function: bool,
    /// Bytes the free `module` / `exports` references would cost over one-letter names:
    /// what running the module unwrapped (no mangled parameters) adds.
    pub cjs_ref_bytes: u32,
    /// `--mangle-props`: the module's property names (`ts_r_minify::props::collect`).
    pub props: ts_r_minify::props::PropNames,
}

impl ModuleMeta {
    pub fn export(&self, name: &str) -> Option<&ExportTarget> {
        self.exports
            .binary_search_by(|(n, _)| n.as_str().cmp(name))
            .ok()
            .map(|i| &self.exports[i].1)
    }

    pub fn import_of(&self, sym: Sym) -> Option<&ImportBinding> {
        self.imports.iter().find(|i| i.sym == sym)
    }

    pub fn sym_info(&self, sym: Sym) -> Option<&SymInfo> {
        self.syms.iter().find(|s| s.sym == sym)
    }

    /// Serializes to a compact binary form (for the persistent cache).
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::default();
        w.u32(self.records.len() as u32);
        for r in &self.records {
            w.u8(match r.kind {
                RecordKind::Import => 0,
                RecordKind::ReExport => 1,
                RecordKind::Require => 2,
                RecordKind::Dynamic => 3,
                RecordKind::CssImport => 4,
                RecordKind::CssUrl => 5,
                RecordKind::CssComposes => 6,
            });
            w.str(&r.spec);
            w.u32(r.start);
            w.u32(r.end);
            w.u32(r.node);
            w.u8(r.in_try as u8);
            w.u8(r.phase);
            w.str(&r.attrs);
        }
        w.u32(self.imports.len() as u32);
        for i in &self.imports {
            w.u32(i.sym);
            w.u32(i.record);
            w.imported(&i.name);
        }
        w.u32(self.exports.len() as u32);
        for (n, t) in &self.exports {
            w.str(n);
            match t {
                ExportTarget::Local(s) => {
                    w.u8(0);
                    w.u32(*s);
                }
                ExportTarget::Reexport { record, name } => {
                    w.u8(1);
                    w.u32(*record);
                    w.imported(name);
                }
            }
        }
        w.u32s(&self.star_exports);
        w.u32(self.parts.len() as u32);
        for p in &self.parts {
            w.u32(p.node);
            w.u8(match p.kind {
                PartKind::Import => 0,
                PartKind::ExportFrom => 1,
                PartKind::ExportLocal => 2,
                PartKind::Directive => 3,
                PartKind::Stmt => 4,
            });
            w.u32s(&p.declares);
            w.u32s(&p.uses);
            w.u32(p.ns_members.len() as u32);
            for (s, n) in &p.ns_members {
                w.u32(*s);
                w.str(n);
            }
            w.u32s(&p.records);
            w.u8(p.side_effects as u8);
        }
        w.u32(self.syms.len() as u32);
        for s in &self.syms {
            w.u32(s.sym);
            w.str(&s.name);
            w.u8(match s.kind {
                SymKind::Var => 0,
                SymKind::Lexical => 1,
                SymKind::Function => 2,
                SymKind::Class => 3,
                SymKind::Import => 4,
                SymKind::Default => 5,
            });
        }
        w.u32(self.globals.len() as u32);
        for g in &self.globals {
            w.str(g);
        }
        let flags = (self.has_esm_syntax as u8)
            | (self.uses_cjs as u8) << 1
            | (self.has_tla as u8) << 2
            | (self.uses_import_meta as u8) << 3
            | (self.has_direct_eval as u8) << 4
            | (self.default_sym.is_some() as u8) << 5
            | (self.has_esm_exports as u8) << 6;
        w.u8(flags);
        w.u8((self.use_strict as u8) | (self.needs_function as u8) << 1);
        w.u32(self.cjs_ref_bytes);
        w.u32(self.default_sym.unwrap_or(0));
        w.u32(self.props.mangled.len() as u32);
        for (n, c) in &self.props.mangled {
            w.str(n);
            w.u32(*c);
        }
        w.u32(self.props.kept.len() as u32);
        for n in &self.props.kept {
            w.str(n);
        }
        w.0
    }

    /// Inverse of [`ModuleMeta::encode`]; `None` for malformed input.
    pub fn decode(data: &[u8]) -> Option<ModuleMeta> {
        let mut r = Reader { data, pos: 0 };
        let mut m = ModuleMeta::default();
        for _ in 0..r.count()? {
            let kind = match r.u8()? {
                0 => RecordKind::Import,
                1 => RecordKind::ReExport,
                2 => RecordKind::Require,
                3 => RecordKind::Dynamic,
                4 => RecordKind::CssImport,
                5 => RecordKind::CssUrl,
                6 => RecordKind::CssComposes,
                _ => return None,
            };
            m.records.push(Record {
                kind,
                spec: r.str()?,
                start: r.u32()?,
                end: r.u32()?,
                node: r.u32()?,
                in_try: r.u8()? != 0,
                phase: r.u8()?,
                attrs: r.str()?,
            });
        }
        for _ in 0..r.count()? {
            m.imports.push(ImportBinding {
                sym: r.u32()?,
                record: r.u32()?,
                name: r.imported()?,
            });
        }
        for _ in 0..r.count()? {
            let n = r.str()?;
            let t = match r.u8()? {
                0 => ExportTarget::Local(r.u32()?),
                1 => ExportTarget::Reexport {
                    record: r.u32()?,
                    name: r.imported()?,
                },
                _ => return None,
            };
            m.exports.push((n, t));
        }
        m.star_exports = r.u32s()?;
        for _ in 0..r.count()? {
            let node = r.u32()?;
            let kind = match r.u8()? {
                0 => PartKind::Import,
                1 => PartKind::ExportFrom,
                2 => PartKind::ExportLocal,
                3 => PartKind::Directive,
                4 => PartKind::Stmt,
                _ => return None,
            };
            let declares = r.u32s()?;
            let uses = r.u32s()?;
            let mut ns_members = Vec::new();
            for _ in 0..r.count()? {
                ns_members.push((r.u32()?, r.str()?));
            }
            let records = r.u32s()?;
            let side_effects = r.u8()? != 0;
            m.parts.push(Part {
                node,
                kind,
                declares,
                uses,
                ns_members,
                records,
                side_effects,
            });
        }
        for _ in 0..r.count()? {
            let sym = r.u32()?;
            let name = r.str()?;
            let kind = match r.u8()? {
                0 => SymKind::Var,
                1 => SymKind::Lexical,
                2 => SymKind::Function,
                3 => SymKind::Class,
                4 => SymKind::Import,
                5 => SymKind::Default,
                _ => return None,
            };
            m.syms.push(SymInfo { sym, name, kind });
        }
        for _ in 0..r.count()? {
            m.globals.push(r.str()?);
        }
        let flags = r.u8()?;
        m.has_esm_syntax = flags & 1 != 0;
        m.uses_cjs = flags & 2 != 0;
        m.has_tla = flags & 4 != 0;
        m.uses_import_meta = flags & 8 != 0;
        m.has_direct_eval = flags & 16 != 0;
        m.has_esm_exports = flags & 64 != 0;
        let flags2 = r.u8()?;
        m.use_strict = flags2 & 1 != 0;
        m.needs_function = flags2 & 2 != 0;
        m.cjs_ref_bytes = r.u32()?;
        let d = r.u32()?;
        m.default_sym = (flags & 32 != 0).then_some(d);
        for _ in 0..r.count()? {
            m.props.mangled.push((r.str()?, r.u32()?));
        }
        for _ in 0..r.count()? {
            m.props.kept.push(r.str()?);
        }
        (r.pos == data.len()).then_some(m)
    }
}

/// Little-endian binary writer shared by the cache encoders.
#[derive(Default, Debug)]
pub struct Writer(pub Vec<u8>);

impl Writer {
    pub fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    pub fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    pub fn str(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.0.extend_from_slice(s.as_bytes());
    }
    pub fn u32s(&mut self, v: &[u32]) {
        self.u32(v.len() as u32);
        for &x in v {
            self.u32(x);
        }
    }
    fn imported(&mut self, i: &Imported) {
        match i {
            Imported::Name(n) => {
                self.u8(0);
                self.str(n);
            }
            Imported::Namespace => self.u8(1),
        }
    }
}

/// Reader for [`Writer`] output. Every method returns `None` past the end.
#[derive(Debug)]
pub struct Reader<'a> {
    pub data: &'a [u8],
    pub pos: usize,
}

impl Reader<'_> {
    pub fn u8(&mut self) -> Option<u8> {
        let v = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(v)
    }
    pub fn u32(&mut self) -> Option<u32> {
        let b = self.data.get(self.pos..self.pos + 4)?;
        self.pos += 4;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub fn u64(&mut self) -> Option<u64> {
        let b = self.data.get(self.pos..self.pos + 8)?;
        self.pos += 8;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Some(u64::from_le_bytes(a))
    }
    /// A length prefix, bounded by the remaining input so corrupt data can't allocate much.
    pub fn count(&mut self) -> Option<usize> {
        let n = self.u32()? as usize;
        (n <= self.data.len() - self.pos).then_some(n)
    }
    pub fn str(&mut self) -> Option<String> {
        let n = self.count()?;
        let b = self.data.get(self.pos..self.pos + n)?;
        self.pos += n;
        String::from_utf8(b.to_vec()).ok()
    }
    pub fn u32s(&mut self) -> Option<Vec<u32>> {
        let n = self.count()?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(self.u32()?);
        }
        Some(v)
    }
    fn imported(&mut self) -> Option<Imported> {
        match self.u8()? {
            0 => Some(Imported::Name(self.str()?)),
            1 => Some(Imported::Namespace),
            _ => None,
        }
    }
}
