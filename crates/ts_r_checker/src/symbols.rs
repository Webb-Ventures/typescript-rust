//! Program-wide symbol ids.
//!
//! [`SymId`]s number binder symbols first (`file_base[file] + local id`), then *merged*
//! symbols (one per group of same-named declarations merged across files: globals, nested
//! namespace exports, ambient modules, module augmentations), then *synthetic* symbols the
//! checker creates (properties of object literals, instantiated, union and intersection
//! types). Every binder symbol that takes part in a merge maps to its group's merged id
//! (its *canonical* id), so a merged interface has one identity everywhere.

use std::sync::OnceLock;

use rustc_hash::FxHashMap;
use ts_r_ast::{Node, NodeId};
use ts_r_binder::program::{ModuleTarget, Program, SymbolRef};
use ts_r_binder::{Name, SymbolFacts, SymbolFlags, SymbolId, TableId, meaning};
use ts_r_diagnostics::FileId;
use ts_r_types::{MapperId, StrId, SymId, TypeId, index_of, level_of};

use crate::checker::{Checker, Loc};
use crate::layers::{Layer, LinksRef};
use crate::program::SourceFile;

/// Lazily computed per-symbol state (binder and merged symbols).
/// An enum member and its constant value.
pub(crate) type EnumMemberValue = (SymId, Option<TypeId>);

#[derive(Default, Debug)]
pub(crate) struct SymLinks {
    pub declared: OnceLock<TypeId>,
    pub ty: OnceLock<TypeId>,
    /// Resolved alias target (`SymId::NONE` when unresolvable).
    pub alias: OnceLock<SymId>,
    pub bases: OnceLock<Box<[TypeId]>>,
    /// Enum members with their constant values.
    pub enum_members: OnceLock<Box<[EnumMemberValue]>>,
    /// Members of a class/interface/literal declared type before instantiation.
    pub raw_members: OnceLock<Box<ts_r_types::ResolvedMembers>>,
}

/// Extra facts of synthetic symbols.
pub mod check_flags {
    pub const READONLY: u32 = 1 << 0;
    pub const INSTANTIATED: u32 = 1 << 1;
    pub const SYNTHETIC_UNION: u32 = 1 << 2;
    pub const SYNTHETIC_INTERSECTION: u32 = 1 << 3;
    /// Present in some union members only.
    pub const PARTIAL: u32 = 1 << 4;
    pub const PRIVATE: u32 = 1 << 5;
    pub const PROTECTED: u32 = 1 << 6;
    pub const STATIC: u32 = 1 << 7;
    pub const ABSTRACT: u32 = 1 << 8;
    /// Union property whose types differ between members.
    pub const HAS_NON_UNIFORM_TYPE: u32 = 1 << 9;
    /// Union property with a literal type in some member (a discriminant candidate).
    pub const HAS_LITERAL_PART: u32 = 1 << 10;
    /// Created from an object literal member.
    pub const OBJECT_LITERAL: u32 = 1 << 11;
    /// From a method declaration (parameters are compared bivariantly).
    pub const METHOD_LIKE: u32 = 1 << 12;
    /// A merged member of several interface declarations.
    pub const MERGED_MEMBER: u32 = 1 << 13;
    /// A tuple element or `length` property.
    pub const TUPLE: u32 = 1 << 14;
    /// Getter without setter.
    pub const GETTER_ONLY: u32 = 1 << 15;
    /// The property is optional (synthesized union/intersection result).
    pub const OPTIONAL: u32 = 1 << 16;
    /// Declared with `#name`.
    pub const PRIVATE_NAME: u32 = 1 << 17;
    /// A property of a mapped type (its type is the template instantiated for its key;
    /// `READONLY` and `OPTIONAL` are final, not inherited from the origin).
    pub const MAPPED: u32 = 1 << 18;
    /// A mapped type property made required by `-?`: `undefined` is removed from its type.
    pub const STRIP_OPTIONAL: u32 = 1 << 19;
}

/// A symbol created by the checker.
#[derive(Debug)]
pub struct SynthSym {
    pub name: StrId,
    pub flags: SymbolFlags,
    pub check: u32,
    /// The declared symbol(s) this one comes from (declarations, docs).
    pub origins: Box<[SymId]>,
    /// When set, the type is `instantiate(type_of(origins[0]), mapper)`.
    pub mapper: MapperId,
    pub(crate) ty: OnceLock<TypeId>,
    /// Declaration node for symbols created from object literal members.
    pub decl: Option<Loc>,
    /// The type that declares the property (for accessibility checks).
    pub parent_type: Option<TypeId>,
    /// For mapped type properties: the mapped type and the key type.
    pub mapped: Option<(TypeId, TypeId)>,
}

impl SynthSym {
    pub fn new(name: StrId, flags: SymbolFlags, check: u32) -> SynthSym {
        SynthSym {
            name,
            flags,
            check,
            origins: Box::new([]),
            mapper: MapperId::IDENTITY,
            ty: OnceLock::new(),
            decl: None,
            parent_type: None,
            mapped: None,
        }
    }

    pub fn with_type(self, t: TypeId) -> SynthSym {
        let _ = self.ty.set(t);
        self
    }

    pub fn with_origins(mut self, o: Vec<SymId>) -> SynthSym {
        self.origins = o.into_boxed_slice();
        self
    }

    pub fn with_mapper(mut self, m: MapperId) -> SynthSym {
        self.mapper = m;
        self
    }

    pub fn with_decl(mut self, d: Option<Loc>) -> SynthSym {
        self.decl = d;
        self
    }

    pub fn with_mapped(mut self, mapped: TypeId, key: TypeId) -> SynthSym {
        self.mapped = Some((mapped, key));
        self
    }

    pub fn with_parent(mut self, t: Option<TypeId>) -> SynthSym {
        self.parent_type = t;
        self
    }

    pub fn known_type(&self) -> Option<TypeId> {
        self.ty.get().copied()
    }
}

/// A group of declarations merged across files.
#[derive(Debug)]
pub(crate) struct MergedSym {
    pub name: Box<str>,
    pub flags: SymbolFlags,
    pub facts: SymbolFacts,
    pub parts: Vec<SymbolRef>,
}

/// The symbols of one program level (all files, for a single-level checker): binder
/// symbols of the level's files, then merged groups formed among them. Ids carry the level
/// in their top bits (`ts_r_types::make_id`).
#[derive(Debug)]
pub(crate) struct SymbolTable {
    level: u8,
    /// First program file id of the level.
    file_start: u32,
    /// Per file of the level (relative to `file_start`), plus a sentinel.
    file_base: Vec<u32>,
    /// Per binder symbol of the level: its file (relative to `file_start`). `sym_kind` is
    /// on every symbol read; a lookup beats a binary search over `file_base`.
    file_of: Vec<u32>,
    n_binder: u32,
    merged: Vec<MergedSym>,
    canon: FxHashMap<u32, u32>,
    /// A merge group has parts both in this level and outside it (the levels are unsound).
    pub(crate) crosses_levels: bool,
}

/// What a [`SymId`] refers to.
#[derive(Clone, Copy, Debug)]
pub enum SymKind<'x> {
    Binder(FileId, SymbolId),
    Merged(&'x [SymbolRef]),
    Synth(&'x SynthSym),
}

const MAX_MERGE_DEPTH: u32 = 32;

impl SymbolTable {
    /// The symbols of files `file_start..file_end` of the program, at `level`.
    pub fn build(
        files: &[std::sync::Arc<SourceFile>],
        link: &Program<'_>,
        level: u8,
        file_start: u32,
        file_end: u32,
    ) -> SymbolTable {
        let mut file_base = Vec::with_capacity((file_end - file_start) as usize + 1);
        let mut n = 0u32;
        for f in &files[file_start as usize..file_end as usize] {
            file_base.push(n);
            n += f.bound.symbols.len() as u32;
        }
        file_base.push(n);
        let mut file_of = Vec::with_capacity(n as usize);
        for (i, w) in file_base.windows(2).enumerate() {
            file_of.extend(std::iter::repeat_n(i as u32, (w[1] - w[0]) as usize));
        }
        let mut t = SymbolTable {
            level,
            file_start,
            file_base,
            file_of,
            n_binder: n,
            merged: Vec::new(),
            canon: FxHashMap::default(),
            crosses_levels: false,
        };
        // Only groups whose parts are all in this level belong to it; groups wholly in other
        // levels are theirs, and a group across levels makes the levels unsound.
        let mine = |parts: &[SymbolRef], t: &mut SymbolTable| -> bool {
            let n_in = parts
                .iter()
                .filter(|r| r.file.0 >= file_start && r.file.0 < file_end)
                .count();
            if n_in > 0 && n_in < parts.len() {
                t.crosses_levels = true;
            }
            n_in == parts.len()
        };
        for (name, m) in &link.globals {
            if m.symbols.len() > 1 && mine(&m.symbols, &mut t) {
                t.group(files, name, m.symbols.clone(), 0);
            }
        }
        for (name, m) in &link.ambient_modules {
            if m.symbols.len() > 1 && mine(&m.symbols, &mut t) {
                t.group(files, name, m.symbols.clone(), 0);
            }
        }
        // Augmentations of re-exported names merge with the re-exported declaration.
        for &(aug, target) in &link.augmentation_redirects {
            // Redirects are listed for the whole program: one wholly in a lower level was
            // merged there, and one across levels makes the levels unsound.
            if !mine(&[aug, target], &mut t) {
                continue;
            }
            let f = &files[target.file.0 as usize];
            let name = f
                .bound
                .name_text(&f.ast, f.bound.symbol(target.symbol).name)
                .to_owned();
            t.group(files, &name, vec![target, aug], 0);
        }
        let mut targets: Vec<ModuleTarget> = Vec::new();
        for &(_, target) in &link.augmentations {
            if target != ModuleTarget::Unresolved && !targets.contains(&target) {
                targets.push(target);
            }
        }
        for target in targets {
            let parts = link.module_symbols(target);
            // A module whose `export =` is a namespace-like entity: its augmentations merge
            // with that entity (tsc), not with the module.
            if let Some(eq) = link.resolve_export(target, "export=")
                && let Some(entity) = link.resolve_alias(eq)
                && !parts.contains(&entity)
                && {
                    let ef = files[entity.file.0 as usize]
                        .bound
                        .symbol(entity.symbol)
                        .flags;
                    // Anything a namespace can merge with (tsc 7 merges into a function too).
                    ef.intersects(
                        SymbolFlags::NAMESPACE_MODULE
                            | SymbolFlags::VALUE_MODULE
                            | SymbolFlags::FUNCTION
                            | SymbolFlags::CLASS
                            | SymbolFlags::REGULAR_ENUM
                            | SymbolFlags::CONST_ENUM,
                    )
                }
            {
                let mut group = vec![entity];
                group.extend(
                    link.augmentations
                        .iter()
                        .filter(|a| a.1 == target)
                        .map(|a| a.0),
                );
                if group.len() > 1 && mine(&group, &mut t) {
                    let f = &files[entity.file.0 as usize];
                    let entity_flags = f.bound.symbol(entity.symbol).flags;
                    let name = f
                        .bound
                        .name_text(&f.ast, f.bound.symbol(entity.symbol).name)
                        .to_owned();
                    t.group(files, &name, group, 0);
                    // The entity stays an entity (the augmentations are module symbols);
                    // another file module (`import * as ns from "./x"; export = ns`) stays
                    // that module.
                    if !entity_flags.intersects(SymbolFlags::MODULE_FILE)
                        && let Some(&id) = t.canon.get(&t.raw(entity))
                    {
                        let m = &mut t.merged[(id - t.n_binder) as usize];
                        m.flags.remove(SymbolFlags::MODULE_FILE);
                    }
                }
                continue;
            }
            if parts.len() > 1 && mine(&parts, &mut t) {
                let name = match target {
                    ModuleTarget::Ambient(i) => link.ambient_modules[i as usize].0.to_string(),
                    _ => "__module".to_owned(),
                };
                t.group(files, &name, parts, 0);
            }
        }
        t
    }

    fn raw(&self, r: SymbolRef) -> u32 {
        self.file_base[(r.file.0 - self.file_start) as usize] + r.symbol.0
    }

    /// Creates (or extends) a merged group and recursively merges same-named exports.
    fn group(
        &mut self,
        files: &[std::sync::Arc<SourceFile>],
        name: &str,
        parts: Vec<SymbolRef>,
        depth: u32,
    ) {
        let mut parts: Vec<SymbolRef> = parts;
        parts.dedup();
        // Parts already in a group join it.
        let existing = parts
            .iter()
            .find_map(|&p| self.canon.get(&self.raw(p)).copied());
        let id = match existing {
            Some(id) => {
                let m = &mut self.merged[(id - self.n_binder) as usize];
                for &p in &parts {
                    if !m.parts.contains(&p) {
                        m.parts.push(p);
                    }
                }
                id
            }
            None => {
                let id = self.n_binder + self.merged.len() as u32;
                self.merged.push(MergedSym {
                    name: name.into(),
                    flags: SymbolFlags::NONE,
                    facts: SymbolFacts::NONE,
                    parts: parts.clone(),
                });
                id
            }
        };
        let mut flags = SymbolFlags::NONE;
        let mut facts = SymbolFacts::NONE;
        let all = self.merged[(id - self.n_binder) as usize].parts.clone();
        for &p in &all {
            let s = files[p.file.0 as usize].bound.symbol(p.symbol);
            flags |= s.flags;
            facts |= s.facts;
            let raw = self.raw(p);
            self.canon.entry(raw).or_insert(id);
        }
        let m = &mut self.merged[(id - self.n_binder) as usize];
        m.flags = flags;
        m.facts = facts;
        if depth >= MAX_MERGE_DEPTH {
            return;
        }
        if flags.intersects(SymbolFlags::NAMESPACE | SymbolFlags::CLASS | SymbolFlags::MODULE_FILE)
        {
            let mut by_name: Vec<(String, Vec<SymbolRef>)> = Vec::new();
            let mut index: FxHashMap<String, usize> = FxHashMap::default();
            for &p in &all {
                let f = &files[p.file.0 as usize];
                let et = f.bound.symbol(p.symbol).exports;
                for &(nm, s) in f.bound.table(et) {
                    let text = f.bound.name_text(&f.ast, nm).to_owned();
                    let r = SymbolRef {
                        file: p.file,
                        symbol: s,
                    };
                    match index.get(&text) {
                        Some(&i) => by_name[i].1.push(r),
                        None => {
                            index.insert(text.clone(), by_name.len());
                            by_name.push((text, vec![r]));
                        }
                    }
                }
            }
            for (nm, mut rs) in by_name {
                // A re-export and a redirected augmentation of the same name don't merge.
                if rs.len() > 1
                    && rs.iter().any(|r| {
                        files[r.file.0 as usize]
                            .bound
                            .symbol(r.symbol)
                            .flags
                            .intersects(SymbolFlags::ALIAS)
                    })
                    && rs.iter().any(|r| self.canon.contains_key(&self.raw(*r)))
                {
                    rs.retain(|r| {
                        !files[r.file.0 as usize]
                            .bound
                            .symbol(r.symbol)
                            .flags
                            .intersects(SymbolFlags::ALIAS)
                    });
                }
                if rs.len() > 1 {
                    self.group(files, &nm, rs, depth + 1);
                }
            }
        }
    }

    /// One past the last merged symbol's index (synthetic symbols follow).
    pub fn merged_end(&self) -> u32 {
        self.n_binder + self.merged.len() as u32
    }

    #[inline]
    pub fn canonical(&self, file: FileId, s: SymbolId) -> SymId {
        let raw = self.file_base[(file.0 - self.file_start) as usize] + s.0;
        SymId(ts_r_types::make_id(
            self.level,
            self.canon.get(&raw).copied().unwrap_or(raw),
        ))
    }

    /// The binder symbol behind a binder index of this level.
    fn split(&self, raw: u32) -> (FileId, SymbolId) {
        let i = self.file_of[raw as usize] as usize;
        (
            FileId(self.file_start + i as u32),
            SymbolId(raw - self.file_base[i]),
        )
    }

    fn merged_sym(&self, idx: u32) -> &MergedSym {
        &self.merged[(idx - self.n_binder) as usize]
    }
}

/// Iterator over the binder symbols of a [`SymId`].
#[derive(Debug)]
pub struct Parts<'x> {
    one: Option<SymbolRef>,
    many: std::slice::Iter<'x, SymbolRef>,
}

impl Iterator for Parts<'_> {
    type Item = SymbolRef;
    fn next(&mut self) -> Option<SymbolRef> {
        if let Some(r) = self.one.take() {
            return Some(r);
        }
        self.many.next().copied()
    }
}

pub(crate) fn symbol_meaning(flags: SymbolFlags) -> u8 {
    if flags.intersects(SymbolFlags::ALIAS) {
        return meaning::ALL;
    }
    let mut m = 0;
    if flags.intersects(SymbolFlags::VALUE) {
        m |= meaning::VALUE;
    }
    if flags.intersects(SymbolFlags::TYPE) {
        m |= meaning::TYPE;
    }
    if flags.intersects(SymbolFlags::NAMESPACE) {
        m |= meaning::NAMESPACE;
    }
    m
}

impl<'a> Checker<'a> {
    pub fn sym_kind(&self, s: SymId) -> SymKind<'_> {
        let layer = &self.layers[level_of(s.0) as usize];
        let t = &layer.syms;
        let i = index_of(s.0);
        if i < t.n_binder {
            let (f, l) = t.split(i);
            SymKind::Binder(f, l)
        } else if i < t.merged_end() {
            SymKind::Merged(&t.merged_sym(i).parts)
        } else {
            SymKind::Synth(layer.synth.get(i - t.merged_end()))
        }
    }

    /// Whether `s` names a symbol of this program (ids parsed back out of text may not).
    pub fn sym_exists(&self, s: SymId) -> bool {
        if s.is_none() {
            return false;
        }
        let Some(layer) = self.layers.get(level_of(s.0) as usize) else {
            return false;
        };
        let i = index_of(s.0);
        let end = layer.syms.merged_end();
        i < end || ((i - end) as usize) < layer.synth.len()
    }

    /// How a property keyed by a unique symbol (`__@name@id`) displays: `[Symbol.iterator]`
    /// for the well-known symbols, `[C.key]` for a symbol declared as a class or namespace
    /// member, `[key]` for one declared by a variable.
    pub fn symbol_key_display(&self, name: &str) -> Option<String> {
        let rest = name.strip_prefix("__@")?;
        let (base, id) = rest.rsplit_once('@').unwrap_or((rest, ""));
        let sym = id
            .parse::<u32>()
            .ok()
            .map(SymId)
            .filter(|&s| self.sym_exists(s));
        let Some(sym) = sym else {
            return Some(format!("[Symbol.{base}]"));
        };
        match self.sym_parent(sym) {
            Some(p) if self.sym_name(p) == "SymbolConstructor" => Some(format!("[Symbol.{base}]")),
            Some(p) if !self.sym_flags(p).intersects(SymbolFlags::MODULE_FILE) => {
                Some(format!("[{}.{base}]", self.sym_name(p)))
            }
            _ => Some(format!("[{base}]")),
        }
    }

    pub fn synth_sym(&self, s: SymId) -> Option<&SynthSym> {
        if s.is_none() {
            return None;
        }
        let layer = self.layers.get(level_of(s.0) as usize)?;
        let end = layer.syms.merged_end();
        let i = index_of(s.0);
        (i >= end).then(|| layer.synth.get(i - end))
    }

    /// Adds a synthetic symbol in the level of the highest of its parts (origins, mapper,
    /// parent type, declaration, known type).
    pub fn new_synth(&self, s: SynthSym) -> SymId {
        let mut l = ts_r_types::mapper_level(s.mapper);
        for o in s.origins.iter() {
            l = l.max(level_of(o.0));
        }
        if let Some(t) = s.parent_type {
            l = l.max(level_of(t.0));
        }
        if let Some(d) = s.decl {
            l = l.max(self.store.file_level(d.file.0));
        }
        if let Some(t) = s.known_type() {
            l = l.max(level_of(t.0));
        }
        if let Some((m, k)) = s.mapped {
            l = l.max(level_of(m.0)).max(level_of(k.0));
        }
        let layer = &self.layers[l as usize];
        let i = layer.syms.merged_end() + layer.synth.push(s);
        SymId(ts_r_types::make_id(l, i))
    }

    pub(crate) fn sym_links(&self, s: SymId) -> Option<LinksRef<'_>> {
        if s.is_none() {
            return None;
        }
        let l = level_of(s.0);
        let links = self
            .layers
            .get(l as usize)?
            .links
            .get(index_of(s.0) as usize)?;
        Some(LinksRef::new(links, s.0, l, &self.layers, &self.store))
    }

    /// The level whose files include `file`.
    #[inline]
    pub(crate) fn file_layer(&self, file: FileId) -> &Layer {
        let mut i = self.layers.len() - 1;
        while i > 0 && file.0 < self.layers[i].file_start {
            i -= 1;
        }
        &self.layers[i]
    }

    /// The canonical symbol of a binder symbol.
    #[inline]
    pub fn sym(&self, file: FileId, s: SymbolId) -> SymId {
        self.file_layer(file).syms.canonical(file, s)
    }

    pub fn sym_ref(&self, r: SymbolRef) -> SymId {
        self.sym(r.file, r.symbol)
    }

    /// Binder symbols behind `s` (several for merged symbols; the origins of synthetic ones).
    pub fn parts(&self, s: SymId) -> Parts<'_> {
        match self.sym_kind(s) {
            SymKind::Binder(f, l) => Parts {
                one: Some(SymbolRef { file: f, symbol: l }),
                many: [].iter(),
            },
            SymKind::Merged(p) => Parts {
                one: None,
                many: p.iter(),
            },
            SymKind::Synth(x) => match x.origins.first() {
                Some(&o) if self.synth_sym(o).is_none() && x.origins.len() == 1 => self.parts(o),
                _ => Parts {
                    one: None,
                    many: [].iter(),
                },
            },
        }
    }

    pub fn sym_flags(&self, s: SymId) -> SymbolFlags {
        if s.is_none() {
            return SymbolFlags::NONE;
        }
        match self.sym_kind(s) {
            SymKind::Binder(f, l) => self.bound(f).symbol(l).flags,
            SymKind::Merged(_) => self.merged_of(s).flags,
            SymKind::Synth(x) => x.flags,
        }
    }

    pub fn sym_facts(&self, s: SymId) -> SymbolFacts {
        if s.is_none() {
            return SymbolFacts::NONE;
        }
        match self.sym_kind(s) {
            SymKind::Binder(f, l) => self.bound(f).symbol(l).facts,
            SymKind::Merged(_) => self.merged_of(s).facts,
            SymKind::Synth(x) => {
                let mut f = SymbolFacts::NONE;
                if x.check & check_flags::READONLY != 0 {
                    f |= SymbolFacts::READONLY;
                }
                if x.check & check_flags::STATIC != 0 {
                    f |= SymbolFacts::STATIC;
                }
                if x.check & check_flags::PRIVATE_NAME != 0 {
                    f |= SymbolFacts::PRIVATE_NAME;
                }
                f
            }
        }
    }

    fn merged_of(&self, s: SymId) -> &MergedSym {
        self.layers[level_of(s.0) as usize]
            .syms
            .merged_sym(index_of(s.0))
    }

    pub fn sym_check_flags(&self, s: SymId) -> u32 {
        self.synth_sym(s).map_or(0, |x| x.check)
    }

    pub fn sym_name(&self, s: SymId) -> &str {
        if s.is_none() {
            return "";
        }
        match self.sym_kind(s) {
            SymKind::Binder(f, l) => {
                let file = self.file(f);
                file.bound.name_text(&file.ast, file.bound.symbol(l).name)
            }
            SymKind::Merged(_) => &self.merged_of(s).name,
            SymKind::Synth(x) => self.store.str(x.name),
        }
    }

    /// The symbol's name as an interned store string.
    pub fn sym_name_id(&self, s: SymId) -> StrId {
        match self.synth_sym(s) {
            Some(x) => x.name,
            None => self.store.intern_str(self.sym_name(s)),
        }
    }

    /// `declare module "x";` without a body (every declaration): the module and all of
    /// its exports are `any`.
    pub fn is_shorthand_ambient_module(&self, s: SymId) -> bool {
        if !self
            .sym_flags(s)
            .intersects(SymbolFlags::VALUE_MODULE | SymbolFlags::MODULE_FILE)
        {
            return false;
        }
        let decls = self.decls(s);
        !decls.is_empty()
            && decls.iter().all(|d| {
                matches!(
                    self.file(d.file).ast.node(d.node),
                    Node::Module { body: None, name, .. }
                        if matches!(self.file(d.file).ast.node(*name), Node::StringLit { .. })
                )
            })
    }

    /// All declarations, in binding/file order.
    pub fn decls(&self, s: SymId) -> Vec<Loc> {
        let mut out = Vec::new();
        if s.is_none() {
            return out;
        }
        if let Some(x) = self.synth_sym(s) {
            if let Some(d) = x.decl {
                out.push(d);
            }
            for &o in x.origins.iter() {
                if o != s {
                    out.extend(self.decls(o));
                }
            }
            return out;
        }
        for r in self.parts(s) {
            for &d in self.bound(r.file).decls(r.symbol) {
                out.push(Loc::new(r.file, d));
            }
        }
        out
    }

    pub fn first_decl(&self, s: SymId) -> Option<Loc> {
        if let Some(x) = self.synth_sym(s) {
            if x.decl.is_some() {
                return x.decl;
            }
            return x
                .origins
                .first()
                .and_then(|&o| if o == s { None } else { self.first_decl(o) });
        }
        self.parts(s).find_map(|r| {
            self.bound(r.file)
                .decls(r.symbol)
                .first()
                .map(|&d| Loc::new(r.file, d))
        })
    }

    pub fn value_decl(&self, s: SymId) -> Option<Loc> {
        if let Some(x) = self.synth_sym(s) {
            if x.decl.is_some() {
                return x.decl;
            }
            return x
                .origins
                .first()
                .and_then(|&o| if o == s { None } else { self.value_decl(o) });
        }
        self.parts(s).find_map(|r| {
            self.bound(r.file)
                .symbol(r.symbol)
                .value_decl
                .map(|d| Loc::new(r.file, d))
        })
    }

    pub fn sym_parent(&self, s: SymId) -> Option<SymId> {
        let r = self.parts(s).next()?;
        let p = self.bound(r.file).symbol(r.symbol).parent?;
        Some(self.sym(r.file, p))
    }

    /// The symbol a declaration node declares.
    pub fn sym_of_decl(&self, l: Loc) -> Option<SymId> {
        self.bound(l.file)
            .symbol_of_decl(l.node)
            .map(|s| self.sym(l.file, s))
    }

    /// Looks `name` up in the exports of every part of `s` (namespaces, enums, class
    /// statics, modules).
    pub fn export_of(&self, s: SymId, name: &str) -> Option<SymId> {
        let flags = self.sym_flags(s);
        if flags.intersects(SymbolFlags::MODULE_FILE) {
            let target = self.module_target_of(s)?;
            if let Some(r) = self.link.resolve_export(target, name) {
                return Some(self.sym_ref(r));
            }
            // Augmentations merged in through another module's `export =` (`import * as ns
            // from "./x"; declare module "pkg" { .. } export = ns`).
            return self
                .merged_augmentation_exports(s)
                .find_map(|(file, table)| {
                    let f = self.file(file);
                    let x = f.bound.lookup_str(&f.ast, table, name)?;
                    (!f.bound.symbol(x).facts.intersects(SymbolFacts::DETACHED))
                        .then(|| self.sym(file, x))
                });
        }
        // A declared export wins over an expando property of the same name (a function
        // merged with a namespace in another file).
        let mut expando = None;
        for r in self.parts(s) {
            let f = self.file(r.file);
            let t = f.bound.symbol(r.symbol).exports;
            if let Some(x) = f.bound.lookup_str(&f.ast, t, name) {
                let facts = f.bound.symbol(x).facts;
                if facts.intersects(SymbolFacts::DETACHED) {
                    continue;
                }
                if facts.intersects(SymbolFacts::EXPANDO) {
                    expando = expando.or(Some(self.sym(r.file, x)));
                    continue;
                }
                return Some(self.sym(r.file, x));
            }
        }
        expando
    }

    /// Looks `name` up in the export tables of the parts of `s` themselves: for an ambient
    /// `declare module "m"` block, its declarations' exports, not what its `export =`
    /// stands for (names inside the block never see the members of the `export =` target).
    /// An augmentation of a file module sees what the module exports.
    fn own_export_of(&self, s: SymId, name: &str) -> Option<SymId> {
        if !self.sym_flags(s).intersects(SymbolFlags::MODULE_FILE)
            || matches!(self.module_target_of(s), Some(ModuleTarget::File(_)))
        {
            return self.export_of(s, name);
        }
        for r in self.parts(s) {
            let f = self.file(r.file);
            let t = f.bound.symbol(r.symbol).exports;
            if let Some(x) = f.bound.lookup_str(&f.ast, t, name)
                && !f.bound.symbol(x).facts.intersects(SymbolFacts::DETACHED)
            {
                return Some(self.sym(r.file, x));
            }
        }
        None
    }

    /// Export tables of augmentations merged into a file module from outside its own
    /// module target: those of a module whose `export =` names this module.
    fn merged_augmentation_exports(&self, s: SymId) -> impl Iterator<Item = (FileId, TableId)> {
        let target = self.module_target_of(s);
        let own = target
            .map(|t| self.link.module_symbols(t))
            .unwrap_or_default();
        self.parts(s)
            .filter(move |r| !own.contains(r))
            .map(|r| (r.file, self.bound(r.file).symbol(r.symbol).exports))
            .collect::<Vec<_>>()
            .into_iter()
    }

    /// The module a module symbol stands for.
    pub fn module_target_of(&self, s: SymId) -> Option<ModuleTarget> {
        for r in self.parts(s) {
            let f = self.file(r.file);
            if r.symbol == f.bound.file_symbol {
                return Some(ModuleTarget::File(r.file));
            }
            let sym = f.bound.symbol(r.symbol);
            if sym.flags.intersects(SymbolFlags::MODULE_FILE) {
                let name = f.bound.name_text(&f.ast, sym.name);
                if let Some(i) = self.link.find_ambient_module(name) {
                    return Some(ModuleTarget::Ambient(i));
                }
            }
        }
        None
    }

    /// Every export of `s` as `(name, symbol)` in declaration order, de-duplicated by name
    /// (first part wins).
    pub fn exports_of(&self, s: SymId) -> Vec<(String, SymId)> {
        let mut out: Vec<(String, SymId)> = Vec::new();
        let mut seen: FxHashMap<String, ()> = FxHashMap::default();
        if self.sym_flags(s).intersects(SymbolFlags::MODULE_FILE) {
            if let Some(t) = self.module_target_of(s) {
                for (n, r) in self.link.export_names(t) {
                    out.push((n.to_string(), self.sym_ref(r)));
                }
            }
            for (file, table) in self.merged_augmentation_exports(s) {
                let f = self.file(file);
                let mut entries: Vec<(NodeId, String, SymbolId)> = f
                    .bound
                    .table(table)
                    .iter()
                    .filter(|(_, x)| !f.bound.symbol(*x).facts.intersects(SymbolFacts::DETACHED))
                    .map(|&(n, x)| {
                        let first = f.bound.decls(x).first().copied().unwrap_or(f.ast.root);
                        (first, f.bound.name_text(&f.ast, n).to_owned(), x)
                    })
                    .collect();
                entries.sort_by_key(|e| e.0);
                for (_, n, x) in entries {
                    if !out.iter().any(|e| e.0 == n) {
                        out.push((n, self.sym(file, x)));
                    }
                }
            }
            return out;
        }
        for r in self.parts(s) {
            let f = self.file(r.file);
            let t = f.bound.symbol(r.symbol).exports;
            let mut entries: Vec<(NodeId, String, SymbolId)> = f
                .bound
                .table(t)
                .iter()
                .filter(|(_, x)| !f.bound.symbol(*x).facts.intersects(SymbolFacts::DETACHED))
                .map(|&(n, x)| {
                    let first = f.bound.decls(x).first().copied().unwrap_or(f.ast.root);
                    (first, f.bound.name_text(&f.ast, n).to_owned(), x)
                })
                .collect();
            entries.sort_by_key(|e| e.0);
            for (_, n, x) in entries {
                let sym = self.sym(r.file, x);
                if seen.insert(n.clone(), ()).is_none() {
                    out.push((n, sym));
                } else if !f.bound.symbol(x).facts.intersects(SymbolFacts::EXPANDO)
                    && let Some(e) = out.iter_mut().find(|e| e.0 == n)
                    && self.sym_facts(e.1).intersects(SymbolFacts::EXPANDO)
                {
                    // A declared export wins over an expando property of the same name.
                    e.1 = sym;
                }
            }
        }
        out
    }

    /// Member tables (instance members) of every declaration of a class/interface/type
    /// literal/object literal symbol.
    pub fn member_tables(&self, s: SymId) -> Vec<(FileId, TableId)> {
        let mut out = Vec::new();
        for r in self.parts(s) {
            for t in self.bound(r.file).member_tables(r.symbol) {
                out.push((r.file, t));
            }
        }
        out
    }

    /// Looks up a global by name with the given meaning.
    pub fn global_symbol(&self, name: &str, meaning: u8) -> Option<SymId> {
        let g = self.link.global(name)?;
        let first = *g.symbols.first()?;
        let s = self.sym_ref(first);
        (symbol_meaning(self.sym_flags(s)) & meaning != 0).then_some(s)
    }

    /// The `JSX` namespace a JSX element at `l` is typed with, as tsc picks it: the `JSX`
    /// export of the namespace the factory's root names, looked up from `l` (`h` for a
    /// leading `@jsx h` pragma or `jsxFactory: "h"`, else `reactNamespace`, default
    /// `React`), else the global `JSX`. In the automatic runtime the namespace comes from
    /// the import source (not modelled): the global one.
    pub fn jsx_namespace(&self, l: Loc) -> Option<SymId> {
        if !self.opts.jsx_automatic {
            let file = self.file(l.file);
            let root = jsx_pragma_root(self.ast(l.file), &file.text)
                .unwrap_or(&self.opts.jsx_factory_roots[0]);
            let jsx = self
                .lookup_in_scope_chain(l, root, meaning::ALL)
                .or_else(|| self.lookup_in_merged_namespaces(l, root, meaning::ALL))
                .or_else(|| self.global_symbol(root, meaning::ALL))
                .and_then(|s| self.resolve_alias(s))
                .and_then(|s| self.export_of(s, "JSX"))
                .and_then(|s| self.resolve_alias(s))
                .filter(|&s| symbol_meaning(self.sym_flags(s)) & meaning::NAMESPACE != 0);
            if jsx.is_some() {
                return jsx;
            }
        }
        self.global_symbol("JSX", meaning::ALL)
    }

    /// Resolves an identifier reference (`meaning` from [`ts_r_binder::meaning`]): the
    /// binder's local resolution, else a global of that name.
    pub fn resolve_ident(&self, l: Loc, meaning: u8) -> Option<SymId> {
        // JSDoc `function` is the global `Function` type.
        if meaning & ts_r_binder::meaning::TYPE != 0
            && self.ast(l.file).is_jsdoc(l.node)
            && self.ast(l.file).name_of(l.node) == Some("function")
        {
            return self.global_symbol("Function", meaning);
        }
        let b = self.bound(l.file);
        let mut binder_resolved = b.resolved(l.node).is_some();
        if let Some(s) = b.resolved(l.node) {
            let id = self.sym(l.file, s);
            let f = self.sym_flags(id);
            if symbol_meaning(f) & meaning != 0 {
                if !self.member_out_of_scope(l, f, meaning) && !self.namespace_export_in_enum(l, id)
                {
                    return Some(id);
                }
                binder_resolved = false;
            } else if meaning == ts_r_binder::meaning::NAMESPACE {
                // The binder resolved the name for a wider meaning (`x` of an interface's
                // `extends x.T` as namespace or value) to a value only: a namespace of the
                // name further out is still found.
                binder_resolved = false;
            }
        }
        let name = match *self.node(l) {
            Node::Ident { name } => self.ast(l.file).atom(name),
            _ => return None,
        };
        // The binder resolved references in files that are modules too; a free reference
        // falls back to globals. Names visible through the file's own module scope that the
        // binder did not record (shouldn't happen) are looked up in its locals.
        if !binder_resolved
            && let Some(local) = self.lookup_in_scope_chain(l, name, meaning)
            && !self.member_out_of_scope(l, self.sym_flags(local), meaning)
            && !self.namespace_export_in_enum(l, local)
        {
            return Some(local);
        }
        if !binder_resolved && let Some(s) = self.lookup_in_merged_namespaces(l, name, meaning) {
            return Some(s);
        }
        self.global_symbol(name, meaning)
    }

    /// An export of a namespace merged with an enum, read inside an enum declaration: only
    /// enum members are in scope there (`namespace x { export let y } enum x { z = y }` is
    /// TS2304, observed with tsc 7.0.2).
    fn namespace_export_in_enum(&self, l: Loc, s: SymId) -> bool {
        if self.sym_flags(s).intersects(SymbolFlags::ENUM_MEMBER) {
            return false;
        }
        let Some(parent) = self.sym_parent(s) else {
            return false;
        };
        if !self.sym_flags(parent).intersects(SymbolFlags::ENUM) {
            return false;
        }
        let ast = self.ast(l.file);
        let parents = self.file(l.file).parents();
        let mut cur = parents.parent(l.node);
        while let Some(n) = cur {
            match ast.node(n) {
                Node::Enum { .. } => return self.sym_of_decl(Loc::new(l.file, n)) == Some(parent),
                Node::Module { .. } | Node::Program { .. } => return false,
                _ => {}
            }
            cur = parents.parent(n);
        }
        false
    }

    /// A class member or enum member the binder found through a merged namespace's exports
    /// table: class statics are never in scope as bare names, enum members only inside an
    /// enum declaration (`namespace Clod { x }` for `class Clod { static x }` is TS2304).
    /// Independent type and namespace declarations merged with a member remain in scope.
    fn member_out_of_scope(&self, l: Loc, f: SymbolFlags, meaning: u8) -> bool {
        let f = f.without(SymbolFlags::PROPERTY | SymbolFlags::METHOD | SymbolFlags::ACCESSOR);
        if symbol_meaning(f.without(SymbolFlags::ENUM_MEMBER)) & meaning != 0 {
            return false;
        }
        if symbol_meaning(f) & meaning == 0 {
            return true;
        }
        let ast = self.ast(l.file);
        let parents = self.file(l.file).parents();
        let mut cur = parents.parent(l.node);
        while let Some(n) = cur {
            match ast.node(n) {
                Node::Enum { .. } => return false,
                Node::Module { .. } => return true,
                _ => {}
            }
            cur = parents.parent(n);
        }
        true
    }

    /// Exports of the enclosing namespaces (and ambient `declare module "m"` blocks)
    /// declared in their other (merged) declarations, innermost first:
    /// `namespace A { export interface P {} }` in one file makes `P` visible inside
    /// `namespace A { ... }` of another.
    pub(crate) fn lookup_in_merged_namespaces(
        &self,
        l: Loc,
        name: &str,
        meaning: u8,
    ) -> Option<SymId> {
        let parents = self.file(l.file).parents();
        let ast = self.ast(l.file);
        let mut cur = parents.parent(l.node);
        let mut guard = 0;
        while let Some(node) = cur {
            guard += 1;
            if guard > 10_000 {
                break;
            }
            if let Node::Module { name: mname, .. } = *ast.node(node) {
                // `namespace A.B { }` is inside `A` too: B's exports, then A's (the outer
                // parts are declared at their name identifiers).
                let mut decl = node;
                let mut part = mname;
                loop {
                    let left = match *ast.node(part) {
                        Node::QualifiedName { left, .. } => Some(left),
                        Node::Ident { .. } | Node::StringLit { .. } => None,
                        _ => break,
                    };
                    // Class statics and enum members merged into the namespace are not
                    // in scope in its body.
                    if let Some(ns) = self.sym_of_decl(Loc::new(l.file, decl))
                        && self.sym_flags(ns).intersects(SymbolFlags::NAMESPACE)
                        && let Some(s) = self.own_export_of(ns, name)
                        && !self.sym_flags(s).intersects(SymbolFlags::ALIAS)
                        && !self.member_out_of_scope(l, self.sym_flags(s), meaning)
                    {
                        return Some(s);
                    }
                    let Some(left) = left else { break };
                    decl = match *ast.node(left) {
                        Node::QualifiedName { right, .. } => right,
                        _ => left,
                    };
                    part = left;
                }
            }
            cur = parents.parent(node);
        }
        None
    }

    /// Fallback lexical lookup for identifiers the binder did not record (synthesized
    /// lookups such as `this` parameter names in JSDoc-free positions).
    pub(crate) fn lookup_in_scope_chain(&self, l: Loc, name: &str, meaning: u8) -> Option<SymId> {
        let b = self.bound(l.file);
        let ast = self.ast(l.file);
        let n = b.names.find(ast, name)?;
        let parents = self.file(l.file).parents();
        let mut cur = Some(l.node);
        let mut guard = 0;
        while let Some(node) = cur {
            guard += 1;
            if guard > 10_000 {
                break;
            }
            if let Some(scope) = b.scope_of(node) {
                let sc = b.scopes[scope.0 as usize];
                if let Some(s) = b.lookup(sc.locals, n) {
                    let id = self.sym(l.file, s);
                    if symbol_meaning(self.sym_flags(id)) & meaning != 0 {
                        return Some(id);
                    }
                }
                // Names declared in `declare global { }` are the merged globals inside it
                // (as in the binder's reference resolution), when the merged global has the
                // meaning asked for: a global `var x` declared in the block does not hide the
                // file's `import * as x` from a type name `x.T` (observed with tsc 7.0.2).
                if sc.kind == ts_r_binder::ScopeKind::Module
                    && let Some(container) = sc.symbol
                    && b.symbol(container).exports == b.global_augmentations
                    && b.lookup(b.global_augmentations, n).is_some()
                    && self.global_symbol(name, meaning).is_some()
                {
                    return None;
                }
                // Exported declarations of a module file or namespace are in scope too
                // (`export const x = 1` then `let y: x` is TS2749, not TS2304).
                if let Some(id) = self.lookup_in_scope_exports(l.file, sc, n, meaning) {
                    return Some(id);
                }
            }
            cur = parents.parent(node);
        }
        let file_scope = b.scopes.first()?;
        if let Some(s) = b.lookup(file_scope.locals, n) {
            let id = self.sym(l.file, s);
            if symbol_meaning(self.sym_flags(id)) & meaning != 0 {
                return Some(id);
            }
        }
        self.lookup_in_scope_exports(l.file, *file_scope, n, meaning)
    }

    /// `name` in the exports of a file or namespace scope's symbol, with `meaning`.
    fn lookup_in_scope_exports(
        &self,
        file: FileId,
        sc: ts_r_binder::Scope,
        n: Name,
        meaning: u8,
    ) -> Option<SymId> {
        if !matches!(
            sc.kind,
            ts_r_binder::ScopeKind::File | ts_r_binder::ScopeKind::Module
        ) {
            return None;
        }
        let b = self.bound(file);
        let container = sc.symbol?;
        let exports = b.symbol(container).exports;
        if exports == b.global_augmentations {
            return None;
        }
        let s = b.lookup(exports, n)?;
        let id = self.sym(file, s);
        // Export specifiers and `export * as ns` are not in scope.
        let flags = self.sym_flags(id);
        (!flags.contains(SymbolFlags::ALIAS) && symbol_meaning(flags) & meaning != 0).then_some(id)
    }

    /// For a named import `import { x } from "m"` of a module whose `export =` entity is a
    /// value: that entity and `x`. The import's value meaning is then the property `x` of
    /// the entity's type (`import { Console } from "node:console"` where `console` is both
    /// a namespace with an interface `Console` and a variable with a `Console` property).
    pub fn export_equals_member(&self, s: SymId) -> Option<(SymId, String)> {
        let (file, local) = match self.sym_kind(s) {
            SymKind::Binder(f, l) => (f, l),
            _ => return None,
        };
        let b = self.bound(file);
        let info = b.alias(local)?;
        let ts_r_binder::AliasTarget::Module {
            request,
            import: ts_r_binder::ImportName::Named(n),
        } = info.target
        else {
            return None;
        };
        let target = self.link.module_target(file, request);
        let eq = self.link.resolve_export(target, "export=")?;
        let eq = self.link.resolve_alias(eq)?;
        let eq = self.sym_ref(eq);
        if symbol_meaning(self.sym_flags(eq)) & meaning::VALUE == 0 {
            return None;
        }
        let name = b.name_text(self.ast(file), n).to_owned();
        Some((eq, name))
    }

    /// Follows import/export aliases to the aliased symbol (`None` when unresolvable).
    pub fn resolve_alias(&self, s: SymId) -> Option<SymId> {
        if !self.sym_flags(s).intersects(SymbolFlags::ALIAS) {
            return Some(s);
        }
        if let Some(l) = self.sym_links(s)
            && let Some(&t) = l.alias.get()
        {
            return (!t.is_none()).then_some(t);
        }
        // The default import of a CommonJS module is the module (its synthetic default,
        // see `synthetic_default_module`), not an `exports.default` assignment.
        if self.commonjs_default_module(s).is_some() {
            if let Some(l) = self.sym_links(s) {
                let _ = l.alias.set(SymId::NONE);
            }
            return None;
        }
        let r = self.parts(s).next()?;
        let target = self.link.resolve_alias(r).map(|t| self.sym_ref(t));
        if let Some(l) = self.sym_links(s) {
            let _ = l.alias.set(target.unwrap_or(SymId::NONE));
        }
        target
    }

    /// The module a default import `s` (`import d from "m"`) stands for when the module has
    /// no default export and synthetic default imports are allowed (`None` otherwise).
    pub fn synthetic_default_module(&self, s: SymId) -> Option<SymId> {
        if let Some(m) = self.commonjs_default_module(s) {
            return Some(m);
        }
        if !self.opts.allow_synthetic_default_imports {
            return None;
        }
        let SymKind::Binder(file, local) = self.sym_kind(s) else {
            return None;
        };
        let info = self.bound(file).alias(local)?;
        let ts_r_binder::AliasTarget::Module {
            request,
            import: ts_r_binder::ImportName::Default,
        } = info.target
        else {
            return None;
        };
        let m = self.module_target_symbol(self.link.module_target(file, request))?;
        self.can_have_synthetic_default(file, m).then_some(m)
    }

    /// A default import `s` of a JavaScript CommonJS module that doesn't export `__esModule`:
    /// the whole module, even when it assigns `exports.default` (observed tsc 7).
    pub fn commonjs_default_module(&self, s: SymId) -> Option<SymId> {
        let SymKind::Binder(file, local) = self.sym_kind(s) else {
            return None;
        };
        let info = self.bound(file).alias(local)?;
        let ts_r_binder::AliasTarget::Module {
            request,
            import: ts_r_binder::ImportName::Default,
        } = info.target
        else {
            return None;
        };
        let ModuleTarget::File(f) = self.link.module_target(file, request) else {
            return None;
        };
        let b = self.bound(f);
        if !b.commonjs || !self.file(f).is_js() {
            return None;
        }
        let m = self.sym(f, b.file_symbol);
        (self.export_of(m, "export=").is_none() && self.export_of(m, "__esModule").is_none())
            .then_some(m)
    }

    /// The module symbol of a resolved module target (a TypeScript file module or a
    /// non-shorthand ambient module).
    pub fn module_target_symbol(&self, target: ModuleTarget) -> Option<SymId> {
        match target {
            ModuleTarget::File(f) => {
                let sf = self.file(f);
                (sf.bound.is_module && !sf.is_js()).then(|| self.sym(f, sf.bound.file_symbol))
            }
            ModuleTarget::Ambient(_) => {
                let &first = self.link.module_symbols(target).first()?;
                let s = self.sym_ref(first);
                (!self.is_shorthand_ambient_module(s)).then_some(s)
            }
            ModuleTarget::Unresolved => None,
        }
    }

    /// Whether a default import in `importer` of module `m` without a default export is the
    /// module itself: CommonJS-format declaration files and ambient modules (unless they
    /// export `__esModule`), not TypeScript sources, except a CommonJS-format file imported from an
    /// ES module under the Node module kinds (observed tsc 7).
    pub fn can_have_synthetic_default(&self, importer: FileId, m: SymId) -> bool {
        if self.export_of(m, "default").is_some() || self.export_of(m, "export=").is_some() {
            return false;
        }
        if let SymKind::Binder(f, l) = self.sym_kind(m)
            && self.bound(f).file_symbol == l
        {
            // A JSON module is its value (like `export =`).
            if self.file(f).is_json() {
                return true;
            }
            use ts_r_config::ModuleKind as M;
            if matches!(
                self.opts.module,
                M::Node16 | M::Node18 | M::Node20 | M::NodeNext
            ) && self.file(importer).esm_format
                && !self.file(f).esm_format
            {
                return true;
            }
            // A declaration file of ESM format (`.d.mts`, `"type": "module"`) is known to be
            // an ES module.
            if !self.file(f).is_declaration() || self.file(f).package_esm {
                return false;
            }
        }
        self.export_of(m, "__esModule").is_none()
    }

    /// `true` when the symbol meaning includes `m`.
    pub fn has_meaning(&self, s: SymId, m: u8) -> bool {
        symbol_meaning(self.sym_flags(s)) & m != 0
    }

    /// The `Name` of a symbol in its own file (binder symbols only).
    pub fn binder_name(&self, s: SymId) -> Option<(FileId, Name)> {
        match self.sym_kind(s) {
            SymKind::Binder(f, l) => Some((f, self.bound(f).symbol(l).name)),
            _ => None,
        }
    }
}

/// The root of the factory a `@jsx` pragma names in the file's leading comments (`h` for
/// `/** @jsx h.f */`); tsc reads pragmas only there.
fn jsx_pragma_root<'t>(ast: &ts_r_ast::Ast, text: &'t str) -> Option<&'t str> {
    let mut end = if text.starts_with("#!") {
        text.find('\n').unwrap_or(text.len())
    } else {
        0
    };
    for c in ast.comments_in(ts_r_ast::Span::new(0, text.len() as u32)) {
        let (start, stop) = (c.span.start as usize, c.span.end as usize);
        if !text.get(end..start)?.trim().is_empty() {
            break;
        }
        end = stop;
        let mut rest = text.get(start..stop)?;
        while let Some(i) = rest.find("@jsx") {
            rest = &rest[i + 4..];
            if rest.starts_with(char::is_whitespace) {
                let name = rest.trim_start();
                let len = name
                    .find(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '$'))
                    .unwrap_or(name.len());
                if len > 0 {
                    return Some(&name[..len]);
                }
            }
        }
    }
    None
}
